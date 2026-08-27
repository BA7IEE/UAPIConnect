//! System credential storage for the U-API Connect distribution.
//!
//! The ordinary settings file is intentionally limited to non-sensitive
//! connection metadata. The U-API key is stored directly in the OS credential
//! store. Official Codex auth can be much larger than Windows Credential
//! Manager's per-entry limit, so only a fixed-size encryption key is kept in
//! the credential store and the encrypted payload lives beside settings.json.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use fs2::FileExt;
use keyring::v1::{Entry, Error};
use serde::{Deserialize, Serialize};

const SERVICE_NAME: &str = crate::distribution::SILENT_BUNDLE_ID;
const UAPI_API_KEY_ACCOUNT: &str = "uapi-api-key";
const LEGACY_OFFICIAL_AUTH_ACCOUNT: &str = "official-auth-json";
const OFFICIAL_AUTH_MASTER_KEY_ACCOUNT: &str = "official-auth-master-key-v1";
const OFFICIAL_AUTH_FILE_NAME: &str = "uapi-official-auth.v1.enc";
const OFFICIAL_AUTH_LOCK_FILE_NAME: &str = "uapi-official-auth.v1.lock";
const OFFICIAL_AUTH_FILE_VERSION: u8 = 1;
const OFFICIAL_AUTH_AAD: &[u8] = b"uapi-connect:official-auth:v1";
const AES_256_KEY_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CredentialSlot {
    UapiApiKey,
    OfficialAuthJson,
}

pub(crate) trait CredentialVault {
    fn get(&self, slot: CredentialSlot) -> anyhow::Result<Option<String>>;
    fn set(&self, slot: CredentialSlot, secret: &str) -> anyhow::Result<()>;
    fn delete(&self, slot: CredentialSlot) -> anyhow::Result<()>;
}

trait KeyringBackend {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>>;
    fn set(&self, account: &str, secret: &str) -> anyhow::Result<()>;
    fn delete(&self, account: &str) -> anyhow::Result<()>;
}

#[derive(Debug, Default)]
struct SystemKeyringBackend;

impl SystemKeyringBackend {
    fn entry(account: &str) -> anyhow::Result<Entry> {
        Entry::new(SERVICE_NAME, account).context("系统凭证库不可用，请检查系统钥匙串或凭据管理器")
    }
}

impl KeyringBackend for SystemKeyringBackend {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(anyhow::anyhow!(error))
                .context("读取系统凭证库失败，请检查系统钥匙串或凭据管理器"),
        }
    }

    fn set(&self, account: &str, secret: &str) -> anyhow::Result<()> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(anyhow::Error::from)
            .context("保存到系统凭证库失败，请检查系统钥匙串或凭据管理器")
    }

    fn delete(&self, account: &str) -> anyhow::Result<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(anyhow::anyhow!(error))
                .context("更新系统凭证库失败，请检查系统钥匙串或凭据管理器"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EncryptedOfficialAuth {
    version: u8,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug)]
struct FileBackedCredentialVault<B> {
    keyring: B,
    official_auth_path: PathBuf,
    legacy_official_auth_path: Option<PathBuf>,
}

impl<B> FileBackedCredentialVault<B>
where
    B: KeyringBackend,
{
    fn new(keyring: B, official_auth_path: PathBuf) -> Self {
        Self {
            keyring,
            official_auth_path,
            legacy_official_auth_path: None,
        }
    }

    fn with_legacy_official_auth_path(mut self, path: PathBuf) -> Self {
        if path != self.official_auth_path {
            self.legacy_official_auth_path = Some(path);
        }
        self
    }

    fn with_official_auth_lock<T>(
        &self,
        operation: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let mut lock_paths = vec![
            self.official_auth_path
                .with_file_name(OFFICIAL_AUTH_LOCK_FILE_NAME),
        ];
        if let Some(legacy_path) = self.legacy_official_auth_path.as_deref() {
            lock_paths.push(legacy_path.with_file_name(OFFICIAL_AUTH_LOCK_FILE_NAME));
        }
        lock_paths.sort();
        lock_paths.dedup();

        // Every process acquires both the isolated and legacy lock in the same
        // path order. This keeps migration compatible with an older process
        // that still protects only the shared legacy directory.
        let mut lock_files = Vec::with_capacity(lock_paths.len());
        for lock_path in lock_paths {
            let lock_file = open_lock_file(&lock_path)?;
            lock_file
                .lock_exclusive()
                .with_context(|| format!("锁定官方登录快照失败：{}", lock_path.display()))?;
            lock_files.push(lock_file);
        }
        operation()
    }

    fn read_official_auth(&self) -> anyhow::Result<Option<String>> {
        let encrypted = match std::fs::read(&self.official_auth_path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "读取加密官方登录快照失败：{}",
                        self.official_auth_path.display()
                    )
                });
            }
        };

        if let Some(encrypted) = encrypted {
            // Read the master key exactly once. A transient keyring error must
            // not be mistaken for corrupt current ciphertext and trigger a
            // rollback to an older legacy snapshot.
            let master_key = self.decryption_master_key()?;
            match Self::decrypt_official_auth_with_key(&encrypted, &master_key) {
                Ok(contents) => {
                    // A previous cleanup may have been interrupted after the
                    // encrypted file was committed. Retry on every successful
                    // read, but cleanup failure must not hide a valid snapshot.
                    let _ = self.keyring.delete(LEGACY_OFFICIAL_AUTH_ACCOUNT);
                    return Ok(Some(contents));
                }
                Err(current_error) => {
                    if let Some(legacy_path) = self.legacy_official_auth_path.as_deref()
                        && let Some(legacy_encrypted) = read_encrypted_snapshot(legacy_path)?
                    {
                        match Self::decrypt_official_auth_with_key(&legacy_encrypted, &master_key) {
                            Ok(contents) => {
                                self.commit_migrated_official_auth(&legacy_encrypted, legacy_path)?;
                                let _ = self.keyring.delete(LEGACY_OFFICIAL_AUTH_ACCOUNT);
                                return Ok(Some(contents));
                            }
                            Err(legacy_error) => {
                                anyhow::bail!(
                                    "当前和旧版官方登录快照均无法解密：当前={current_error}；旧版={legacy_error}"
                                );
                            }
                        }
                    }
                    return Err(current_error).context("当前加密官方登录快照无法读取");
                }
            }
        }

        if let Some(legacy_path) = self.legacy_official_auth_path.as_deref()
            && let Some(legacy_encrypted) = read_encrypted_snapshot(legacy_path)?
        {
            // Decrypt before publishing or removing anything. A corrupt legacy
            // file must remain available for manual recovery.
            let master_key = self.decryption_master_key()?;
            let contents = Self::decrypt_official_auth_with_key(&legacy_encrypted, &master_key)?;
            self.commit_migrated_official_auth(&legacy_encrypted, legacy_path)?;
            let _ = self.keyring.delete(LEGACY_OFFICIAL_AUTH_ACCOUNT);
            return Ok(Some(contents));
        }

        // 兼容上一版把完整 auth.json 直接写进钥匙串的格式。必须先成功
        // 写入加密文件，再清理旧条目，迁移中断时不会丢失登录快照。
        let Some(legacy) = self.keyring.get(LEGACY_OFFICIAL_AUTH_ACCOUNT)? else {
            return Ok(None);
        };
        self.write_official_auth(&legacy)?;
        Ok(Some(legacy))
    }

    fn commit_migrated_official_auth(
        &self,
        encrypted: &[u8],
        legacy_path: &Path,
    ) -> anyhow::Result<()> {
        // The envelope uses the same keyring master key. Publish and fsync the
        // isolated copy before attempting best-effort cleanup of the old one.
        crate::settings::atomic_write_private(&self.official_auth_path, encrypted)
            .context("迁移加密官方登录快照失败")?;
        restrict_file_permissions(&self.official_auth_path)?;
        sync_encrypted_file(&self.official_auth_path)?;
        let _ = remove_file_if_present(legacy_path);
        Ok(())
    }

    fn write_official_auth(&self, secret: &str) -> anyhow::Result<()> {
        let master_key = self.master_key()?;
        let cipher = Aes256Gcm::new_from_slice(&master_key)
            .map_err(|_| anyhow::anyhow!("初始化官方登录快照加密器失败"))?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: secret.as_bytes(),
                    aad: OFFICIAL_AUTH_AAD,
                },
            )
            .map_err(|_| anyhow::anyhow!("加密官方登录快照失败"))?;
        let envelope = EncryptedOfficialAuth {
            version: OFFICIAL_AUTH_FILE_VERSION,
            nonce: BASE64.encode(nonce),
            ciphertext: BASE64.encode(ciphertext),
        };
        let bytes = serde_json::to_vec(&envelope).context("序列化官方登录快照失败")?;
        crate::settings::atomic_write_private(&self.official_auth_path, &bytes)
            .context("保存加密官方登录快照失败")?;
        restrict_file_permissions(&self.official_auth_path)?;
        sync_encrypted_file(&self.official_auth_path)?;

        // 新格式已经原子落盘后才清理旧 direct entry。清理失败不影响已经
        // 可用的新快照，后续读取还会继续重试。
        let _ = self.keyring.delete(LEGACY_OFFICIAL_AUTH_ACCOUNT);
        Ok(())
    }

    fn decryption_master_key(&self) -> anyhow::Result<[u8; AES_256_KEY_LEN]> {
        self.existing_master_key()?
            .ok_or_else(|| anyhow::anyhow!("系统凭证库中缺少官方登录快照主密钥"))
    }

    fn decrypt_official_auth_with_key(
        encrypted: &[u8],
        master_key: &[u8; AES_256_KEY_LEN],
    ) -> anyhow::Result<String> {
        let envelope = serde_json::from_slice::<EncryptedOfficialAuth>(encrypted)
            .context("加密官方登录快照格式无效")?;
        if envelope.version != OFFICIAL_AUTH_FILE_VERSION {
            anyhow::bail!("不支持的官方登录快照版本：{}", envelope.version);
        }
        let nonce = BASE64
            .decode(envelope.nonce)
            .context("官方登录快照 nonce 无效")?;
        if nonce.len() != 12 {
            anyhow::bail!("官方登录快照 nonce 长度无效");
        }
        let ciphertext = BASE64
            .decode(envelope.ciphertext)
            .context("官方登录快照密文无效")?;
        let cipher = Aes256Gcm::new_from_slice(master_key)
            .map_err(|_| anyhow::anyhow!("初始化官方登录快照解密器失败"))?;
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: OFFICIAL_AUTH_AAD,
                },
            )
            .map_err(|_| anyhow::anyhow!("解密官方登录快照失败"))?;
        String::from_utf8(plaintext).context("官方登录快照不是有效 UTF-8")
    }

    fn master_key(&self) -> anyhow::Result<[u8; AES_256_KEY_LEN]> {
        if let Some(key) = self.existing_master_key()? {
            return Ok(key);
        }
        let generated = Aes256Gcm::generate_key(&mut OsRng);
        self.keyring
            .set(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT, &BASE64.encode(generated))?;
        Ok(generated.into())
    }

    fn existing_master_key(&self) -> anyhow::Result<Option<[u8; AES_256_KEY_LEN]>> {
        let Some(encoded) = self.keyring.get(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT)? else {
            return Ok(None);
        };
        let decoded = BASE64
            .decode(encoded)
            .context("系统凭证库中的官方登录快照主密钥无效")?;
        let key = decoded
            .try_into()
            .map_err(|_| anyhow::anyhow!("系统凭证库中的官方登录快照主密钥长度无效"))?;
        Ok(Some(key))
    }

    fn delete_official_auth(&self) -> anyhow::Result<()> {
        // 必须先确认旧 direct entry 已删除，再删除加密文件。否则旧条目删除
        // 失败时，下次读取会把本应删除的登录快照重新迁移回来。
        self.keyring.delete(LEGACY_OFFICIAL_AUTH_ACCOUNT)?;
        remove_file_if_present(&self.official_auth_path).context("删除加密官方登录快照失败")?;
        if let Some(legacy_path) = self.legacy_official_auth_path.as_deref() {
            remove_file_if_present(legacy_path).context("删除旧版加密官方登录快照失败")?;
        }
        // The master key has no purpose once every encrypted payload has been
        // removed. Delete it last so an interrupted cleanup never leaves an
        // undecryptable snapshot behind.
        self.keyring.delete(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT)?;
        Ok(())
    }
}

fn read_encrypted_snapshot(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("读取旧版加密官方登录快照失败：{}", path.display()))
        }
    }
}

fn remove_file_if_present(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

impl<B> CredentialVault for FileBackedCredentialVault<B>
where
    B: KeyringBackend,
{
    fn get(&self, slot: CredentialSlot) -> anyhow::Result<Option<String>> {
        match slot {
            CredentialSlot::UapiApiKey => self.keyring.get(UAPI_API_KEY_ACCOUNT),
            CredentialSlot::OfficialAuthJson => {
                self.with_official_auth_lock(|| self.read_official_auth())
            }
        }
    }

    fn set(&self, slot: CredentialSlot, secret: &str) -> anyhow::Result<()> {
        match slot {
            CredentialSlot::UapiApiKey => self.keyring.set(UAPI_API_KEY_ACCOUNT, secret),
            CredentialSlot::OfficialAuthJson => {
                self.with_official_auth_lock(|| self.write_official_auth(secret))
            }
        }
    }

    fn delete(&self, slot: CredentialSlot) -> anyhow::Result<()> {
        match slot {
            CredentialSlot::UapiApiKey => self.keyring.delete(UAPI_API_KEY_ACCOUNT),
            CredentialSlot::OfficialAuthJson => {
                self.with_official_auth_lock(|| self.delete_official_auth())
            }
        }
    }
}

fn open_lock_file(path: &Path) -> anyhow::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建官方登录快照锁目录失败：{}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("打开官方登录快照锁失败：{}", path.display()))?;
    restrict_file_permissions(path)?;
    Ok(file)
}

#[cfg(unix)]
fn sync_encrypted_file(path: &Path) -> anyhow::Result<()> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("打开加密官方登录快照失败：{}", path.display()))?
        .sync_all()
        .with_context(|| format!("持久化加密官方登录快照失败：{}", path.display()))?;
    if let Some(parent) = path.parent() {
        File::open(parent)
            .with_context(|| format!("打开官方登录快照目录失败：{}", parent.display()))?
            .sync_all()
            .with_context(|| format!("持久化官方登录快照目录失败：{}", parent.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_encrypted_file(_path: &Path) -> anyhow::Result<()> {
    // Windows atomic_write uses MoveFileExW with MOVEFILE_WRITE_THROUGH.
    Ok(())
}

#[derive(Debug)]
pub(crate) struct SystemCredentialVault {
    inner: FileBackedCredentialVault<SystemKeyringBackend>,
}

impl Default for SystemCredentialVault {
    fn default() -> Self {
        let official_auth_path =
            official_auth_path_for_settings(&crate::paths::default_settings_path());
        let legacy_official_auth_path =
            official_auth_path_for_settings(&crate::paths::legacy_upstream_settings_path());
        Self {
            inner: FileBackedCredentialVault::new(SystemKeyringBackend, official_auth_path)
                .with_legacy_official_auth_path(legacy_official_auth_path),
        }
    }
}

impl CredentialVault for SystemCredentialVault {
    fn get(&self, slot: CredentialSlot) -> anyhow::Result<Option<String>> {
        self.inner.get(slot)
    }

    fn set(&self, slot: CredentialSlot, secret: &str) -> anyhow::Result<()> {
        self.inner.set(slot, secret)
    }

    fn delete(&self, slot: CredentialSlot) -> anyhow::Result<()> {
        self.inner.delete(slot)
    }
}

fn official_auth_path_for_settings(settings_path: &Path) -> PathBuf {
    settings_path.with_file_name(OFFICIAL_AUTH_FILE_NAME)
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).context("收紧加密官方登录快照文件权限失败")
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    use super::{CredentialSlot, CredentialVault};

    #[derive(Debug, Default)]
    pub(crate) struct MemoryCredentialVault {
        secrets: Mutex<HashMap<CredentialSlot, String>>,
        failing_gets: Mutex<HashSet<CredentialSlot>>,
        failing_sets: Mutex<HashSet<CredentialSlot>>,
        failing_deletes: Mutex<HashSet<CredentialSlot>>,
    }

    impl MemoryCredentialVault {
        pub(crate) fn fail_get(&self, slot: CredentialSlot) {
            self.failing_gets.lock().unwrap().insert(slot);
        }

        pub(crate) fn fail_set(&self, slot: CredentialSlot) {
            self.failing_sets.lock().unwrap().insert(slot);
        }

        pub(crate) fn fail_delete(&self, slot: CredentialSlot) {
            self.failing_deletes.lock().unwrap().insert(slot);
        }
    }

    impl CredentialVault for MemoryCredentialVault {
        fn get(&self, slot: CredentialSlot) -> anyhow::Result<Option<String>> {
            if self.failing_gets.lock().unwrap().contains(&slot) {
                anyhow::bail!("simulated credential read failure");
            }
            Ok(self.secrets.lock().unwrap().get(&slot).cloned())
        }

        fn set(&self, slot: CredentialSlot, secret: &str) -> anyhow::Result<()> {
            if self.failing_sets.lock().unwrap().contains(&slot) {
                anyhow::bail!("simulated credential write failure");
            }
            self.secrets
                .lock()
                .unwrap()
                .insert(slot, secret.to_string());
            Ok(())
        }

        fn delete(&self, slot: CredentialSlot) -> anyhow::Result<()> {
            if self.failing_deletes.lock().unwrap().contains(&slot) {
                anyhow::bail!("simulated credential delete failure");
            }
            self.secrets.lock().unwrap().remove(&slot);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Barrier, Condvar, Mutex};
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Clone, Default)]
    struct MemoryKeyringBackend {
        secrets: Arc<Mutex<HashMap<String, String>>>,
        failing_gets: Arc<Mutex<HashMap<String, usize>>>,
        failing_deletes: Arc<Mutex<HashSet<String>>>,
    }

    impl MemoryKeyringBackend {
        fn value(&self, account: &str) -> Option<String> {
            self.secrets.lock().unwrap().get(account).cloned()
        }

        fn len(&self) -> usize {
            self.secrets.lock().unwrap().len()
        }

        fn fail_delete(&self, account: &str) {
            self.failing_deletes
                .lock()
                .unwrap()
                .insert(account.to_string());
        }

        fn fail_next_get(&self, account: &str) {
            *self
                .failing_gets
                .lock()
                .unwrap()
                .entry(account.to_string())
                .or_default() += 1;
        }
    }

    impl KeyringBackend for MemoryKeyringBackend {
        fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
            let mut failing_gets = self.failing_gets.lock().unwrap();
            if let Some(remaining) = failing_gets.get_mut(account)
                && *remaining > 0
            {
                *remaining -= 1;
                anyhow::bail!("simulated keyring get failure");
            }
            drop(failing_gets);
            Ok(self.value(account))
        }

        fn set(&self, account: &str, secret: &str) -> anyhow::Result<()> {
            self.secrets
                .lock()
                .unwrap()
                .insert(account.to_string(), secret.to_string());
            Ok(())
        }

        fn delete(&self, account: &str) -> anyhow::Result<()> {
            if self.failing_deletes.lock().unwrap().contains(account) {
                anyhow::bail!("simulated keyring delete failure");
            }
            self.secrets.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct CoordinatedKeyringState {
        secrets: HashMap<String, String>,
        missing_master_key_reads: usize,
    }

    #[derive(Debug, Default)]
    struct CoordinatedKeyringShared {
        state: Mutex<CoordinatedKeyringState>,
        first_reads: Condvar,
    }

    #[derive(Debug, Clone)]
    struct CoordinatedKeyringBackend {
        shared: Arc<CoordinatedKeyringShared>,
        before_master_key_set: Duration,
        after_master_key_set: Duration,
    }

    impl CoordinatedKeyringBackend {
        fn new(
            shared: Arc<CoordinatedKeyringShared>,
            before_master_key_set: Duration,
            after_master_key_set: Duration,
        ) -> Self {
            Self {
                shared,
                before_master_key_set,
                after_master_key_set,
            }
        }
    }

    impl KeyringBackend for CoordinatedKeyringBackend {
        fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
            let mut state = self.shared.state.lock().unwrap();
            if let Some(secret) = state.secrets.get(account) {
                return Ok(Some(secret.clone()));
            }
            if account != OFFICIAL_AUTH_MASTER_KEY_ACCOUNT {
                return Ok(None);
            }

            // Without the outer file lock, two independent vaults both observe
            // a missing key and continue together. With the lock, the first
            // caller times out, commits key+file, and the second sees that key.
            state.missing_master_key_reads += 1;
            if state.missing_master_key_reads == 1 {
                let (next, _) = self
                    .shared
                    .first_reads
                    .wait_timeout_while(state, Duration::from_millis(60), |state| {
                        state.missing_master_key_reads < 2
                    })
                    .unwrap();
                state = next;
            } else {
                self.shared.first_reads.notify_all();
            }
            Ok(state.secrets.get(account).cloned())
        }

        fn set(&self, account: &str, secret: &str) -> anyhow::Result<()> {
            if account == OFFICIAL_AUTH_MASTER_KEY_ACCOUNT {
                std::thread::sleep(self.before_master_key_set);
            }
            self.shared
                .state
                .lock()
                .unwrap()
                .secrets
                .insert(account.to_string(), secret.to_string());
            if account == OFFICIAL_AUTH_MASTER_KEY_ACCOUNT {
                std::thread::sleep(self.after_master_key_set);
            }
            Ok(())
        }

        fn delete(&self, account: &str) -> anyhow::Result<()> {
            self.shared.state.lock().unwrap().secrets.remove(account);
            Ok(())
        }
    }

    #[test]
    fn large_official_auth_is_encrypted_on_disk_and_keyring_only_keeps_master_key() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        let vault = FileBackedCredentialVault::new(keyring.clone(), path.clone());
        let auth = format!(
            r#"{{"auth_mode":"chatgpt","token":"{}"}}"#,
            "x".repeat(8000)
        );

        vault.set(CredentialSlot::OfficialAuthJson, &auth).unwrap();

        let encrypted = std::fs::read_to_string(path).unwrap();
        assert!(!encrypted.contains(&auth));
        assert!(!encrypted.contains(&"x".repeat(100)));
        assert_eq!(
            keyring
                .value(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT)
                .unwrap()
                .len(),
            44
        );
        assert_eq!(keyring.len(), 1);
        assert!(keyring.value(LEGACY_OFFICIAL_AUTH_ACCOUNT).is_none());
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(auth)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(temp.path().join(OFFICIAL_AUTH_FILE_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(temp.path().join(OFFICIAL_AUTH_LOCK_FILE_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn concurrent_first_writes_share_one_master_key_and_leave_decryptable_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(OFFICIAL_AUTH_FILE_NAME);
        let shared = Arc::new(CoordinatedKeyringShared::default());
        let start = Arc::new(Barrier::new(3));
        let vault_a = FileBackedCredentialVault::new(
            CoordinatedKeyringBackend::new(
                shared.clone(),
                Duration::ZERO,
                Duration::from_millis(100),
            ),
            path.clone(),
        );
        let vault_b = FileBackedCredentialVault::new(
            CoordinatedKeyringBackend::new(
                shared.clone(),
                Duration::from_millis(20),
                Duration::ZERO,
            ),
            path.clone(),
        );

        let start_a = start.clone();
        let thread_a = std::thread::spawn(move || {
            start_a.wait();
            vault_a.set(CredentialSlot::OfficialAuthJson, "snapshot-a")
        });
        let start_b = start.clone();
        let thread_b = std::thread::spawn(move || {
            start_b.wait();
            vault_b.set(CredentialSlot::OfficialAuthJson, "snapshot-b")
        });
        start.wait();

        thread_a.join().unwrap().unwrap();
        thread_b.join().unwrap().unwrap();

        let verifier = FileBackedCredentialVault::new(
            CoordinatedKeyringBackend::new(shared, Duration::ZERO, Duration::ZERO),
            path,
        );
        let final_snapshot = verifier
            .get(CredentialSlot::OfficialAuthJson)
            .unwrap()
            .unwrap();
        assert!(matches!(
            final_snapshot.as_str(),
            "snapshot-a" | "snapshot-b"
        ));
    }

    #[test]
    fn rotating_official_auth_atomically_replaces_ciphertext_and_keeps_master_key() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        let vault = FileBackedCredentialVault::new(keyring.clone(), path.clone());

        vault
            .set(CredentialSlot::OfficialAuthJson, "first snapshot")
            .unwrap();
        let first_file = std::fs::read(&path).unwrap();
        let first_key = keyring.value(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT).unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, "second snapshot")
            .unwrap();

        assert_ne!(std::fs::read(path).unwrap(), first_file);
        assert!(
            !temp
                .path()
                .join(OFFICIAL_AUTH_FILE_NAME)
                .with_extension("enc.tmp")
                .exists()
        );
        assert_eq!(
            keyring.value(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT).unwrap(),
            first_key
        );
        assert_eq!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("second snapshot")
        );
    }

    #[test]
    fn legacy_direct_keyring_entry_migrates_after_encrypted_file_is_written() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        keyring
            .set(LEGACY_OFFICIAL_AUTH_ACCOUNT, "legacy official auth")
            .unwrap();
        let vault = FileBackedCredentialVault::new(keyring.clone(), path.clone());

        assert_eq!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("legacy official auth")
        );
        assert!(path.is_file());
        assert!(keyring.value(LEGACY_OFFICIAL_AUTH_ACCOUNT).is_none());
        assert!(keyring.value(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT).is_some());
    }

    #[test]
    fn failed_legacy_direct_migration_keeps_old_keyring_entry() {
        let temp = tempfile::tempdir().unwrap();
        let blocked_parent = temp.path().join("blocked-parent");
        std::fs::write(&blocked_parent, "not a directory").unwrap();
        let keyring = MemoryKeyringBackend::default();
        keyring
            .set(LEGACY_OFFICIAL_AUTH_ACCOUNT, "legacy official auth")
            .unwrap();
        let vault = FileBackedCredentialVault::new(
            keyring.clone(),
            blocked_parent.join(OFFICIAL_AUTH_FILE_NAME),
        );

        assert!(vault.get(CredentialSlot::OfficialAuthJson).is_err());
        assert_eq!(
            keyring.value(LEGACY_OFFICIAL_AUTH_ACCOUNT).as_deref(),
            Some("legacy official auth")
        );
    }

    #[test]
    fn failed_legacy_cleanup_does_not_block_valid_encrypted_get_or_set() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        let vault = FileBackedCredentialVault::new(keyring.clone(), path);
        vault
            .set(CredentialSlot::OfficialAuthJson, "first snapshot")
            .unwrap();
        keyring
            .set(LEGACY_OFFICIAL_AUTH_ACCOUNT, "legacy snapshot")
            .unwrap();
        keyring.fail_delete(LEGACY_OFFICIAL_AUTH_ACCOUNT);

        assert_eq!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("first snapshot")
        );
        vault
            .set(CredentialSlot::OfficialAuthJson, "second snapshot")
            .unwrap();
        assert_eq!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("second snapshot")
        );
        assert_eq!(
            keyring.value(LEGACY_OFFICIAL_AUTH_ACCOUNT).as_deref(),
            Some("legacy snapshot")
        );
    }

    #[test]
    fn failed_explicit_legacy_delete_keeps_encrypted_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        let vault = FileBackedCredentialVault::new(keyring.clone(), path.clone());
        vault
            .set(CredentialSlot::OfficialAuthJson, "current snapshot")
            .unwrap();
        keyring
            .set(LEGACY_OFFICIAL_AUTH_ACCOUNT, "legacy snapshot")
            .unwrap();
        keyring.fail_delete(LEGACY_OFFICIAL_AUTH_ACCOUNT);

        assert!(vault.delete(CredentialSlot::OfficialAuthJson).is_err());
        assert!(path.is_file());
        assert_eq!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("current snapshot")
        );
    }

    #[test]
    fn explicit_delete_removes_encrypted_snapshot_and_master_key() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        let vault = FileBackedCredentialVault::new(keyring.clone(), path.clone());
        vault
            .set(CredentialSlot::OfficialAuthJson, "current snapshot")
            .unwrap();
        let lock_path = temp.path().join(OFFICIAL_AUTH_LOCK_FILE_NAME);
        assert!(lock_path.is_file());

        vault.delete(CredentialSlot::OfficialAuthJson).unwrap();

        assert!(!path.exists());
        assert!(lock_path.is_file());
        assert!(keyring.value(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT).is_none());
        assert!(keyring.value(LEGACY_OFFICIAL_AUTH_ACCOUNT).is_none());
    }

    #[test]
    fn explicit_delete_keeps_legacy_lock_file() {
        let temp = tempfile::tempdir().unwrap();
        let legacy_path = temp.path().join("legacy").join(OFFICIAL_AUTH_FILE_NAME);
        let isolated_path = temp.path().join("isolated").join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        let legacy_vault = FileBackedCredentialVault::new(keyring.clone(), legacy_path.clone());
        legacy_vault
            .set(CredentialSlot::OfficialAuthJson, "legacy snapshot")
            .unwrap();
        let legacy_lock_path = legacy_path.with_file_name(OFFICIAL_AUTH_LOCK_FILE_NAME);
        assert!(legacy_lock_path.is_file());
        let isolated_vault = FileBackedCredentialVault::new(keyring, isolated_path)
            .with_legacy_official_auth_path(legacy_path);

        isolated_vault
            .delete(CredentialSlot::OfficialAuthJson)
            .unwrap();

        assert!(legacy_lock_path.is_file());
    }

    #[test]
    fn encrypted_snapshot_moves_from_shared_legacy_directory_on_first_read() {
        let temp = tempfile::tempdir().unwrap();
        let legacy_path = temp.path().join("legacy").join(OFFICIAL_AUTH_FILE_NAME);
        let isolated_path = temp.path().join("isolated").join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        let legacy_vault = FileBackedCredentialVault::new(keyring.clone(), legacy_path.clone());
        legacy_vault
            .set(
                CredentialSlot::OfficialAuthJson,
                "legacy encrypted snapshot",
            )
            .unwrap();
        let isolated_vault = FileBackedCredentialVault::new(keyring, isolated_path.clone())
            .with_legacy_official_auth_path(legacy_path.clone());

        assert_eq!(
            isolated_vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("legacy encrypted snapshot")
        );
        assert!(isolated_path.is_file());
        assert!(!legacy_path.exists());
    }

    #[test]
    fn valid_legacy_snapshot_recovers_a_corrupted_current_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let legacy_path = temp.path().join("legacy").join(OFFICIAL_AUTH_FILE_NAME);
        let isolated_path = temp.path().join("isolated").join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        let legacy_vault = FileBackedCredentialVault::new(keyring.clone(), legacy_path.clone());
        legacy_vault
            .set(
                CredentialSlot::OfficialAuthJson,
                "recoverable legacy snapshot",
            )
            .unwrap();
        std::fs::create_dir_all(isolated_path.parent().unwrap()).unwrap();
        std::fs::write(&isolated_path, b"corrupted current ciphertext").unwrap();
        let isolated_vault = FileBackedCredentialVault::new(keyring, isolated_path.clone())
            .with_legacy_official_auth_path(legacy_path.clone());

        assert_eq!(
            isolated_vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("recoverable legacy snapshot")
        );
        assert!(!legacy_path.exists());
        assert_ne!(
            std::fs::read(&isolated_path).unwrap(),
            b"corrupted current ciphertext"
        );
        assert_eq!(
            isolated_vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("recoverable legacy snapshot")
        );
    }

    #[test]
    fn transient_master_key_read_failure_never_replaces_current_with_legacy() {
        let temp = tempfile::tempdir().unwrap();
        let legacy_path = temp.path().join("legacy").join(OFFICIAL_AUTH_FILE_NAME);
        let isolated_path = temp.path().join("isolated").join(OFFICIAL_AUTH_FILE_NAME);
        let keyring = MemoryKeyringBackend::default();
        FileBackedCredentialVault::new(keyring.clone(), legacy_path.clone())
            .set(CredentialSlot::OfficialAuthJson, "older legacy snapshot")
            .unwrap();
        FileBackedCredentialVault::new(keyring.clone(), isolated_path.clone())
            .set(CredentialSlot::OfficialAuthJson, "newest current snapshot")
            .unwrap();
        let current_before = std::fs::read(&isolated_path).unwrap();
        let legacy_before = std::fs::read(&legacy_path).unwrap();
        keyring.fail_next_get(OFFICIAL_AUTH_MASTER_KEY_ACCOUNT);
        let vault = FileBackedCredentialVault::new(keyring, isolated_path.clone())
            .with_legacy_official_auth_path(legacy_path.clone());

        assert!(vault.get(CredentialSlot::OfficialAuthJson).is_err());
        assert_eq!(std::fs::read(&isolated_path).unwrap(), current_before);
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_before);
        assert_eq!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some("newest current snapshot")
        );
    }

    #[test]
    fn corrupted_current_and_legacy_snapshots_are_preserved_on_read_failure() {
        let temp = tempfile::tempdir().unwrap();
        let legacy_path = temp.path().join("legacy").join(OFFICIAL_AUTH_FILE_NAME);
        let isolated_path = temp.path().join("isolated").join(OFFICIAL_AUTH_FILE_NAME);
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(isolated_path.parent().unwrap()).unwrap();
        let current_bytes = b"corrupted current ciphertext";
        let legacy_bytes = b"corrupted legacy ciphertext";
        std::fs::write(&isolated_path, current_bytes).unwrap();
        std::fs::write(&legacy_path, legacy_bytes).unwrap();
        let vault =
            FileBackedCredentialVault::new(MemoryKeyringBackend::default(), isolated_path.clone())
                .with_legacy_official_auth_path(legacy_path.clone());

        assert!(vault.get(CredentialSlot::OfficialAuthJson).is_err());

        assert_eq!(std::fs::read(isolated_path).unwrap(), current_bytes);
        assert_eq!(std::fs::read(legacy_path).unwrap(), legacy_bytes);
    }

    #[test]
    fn isolated_vault_waits_for_the_legacy_process_lock() {
        use std::sync::mpsc;

        let temp = tempfile::tempdir().unwrap();
        let legacy_path = temp.path().join("legacy").join(OFFICIAL_AUTH_FILE_NAME);
        let isolated_path = temp.path().join("isolated").join(OFFICIAL_AUTH_FILE_NAME);
        let legacy_lock_path = legacy_path.with_file_name(OFFICIAL_AUTH_LOCK_FILE_NAME);
        let held_legacy_lock = open_lock_file(&legacy_lock_path).unwrap();
        held_legacy_lock.lock_exclusive().unwrap();
        let vault =
            FileBackedCredentialVault::new(MemoryKeyringBackend::default(), isolated_path.clone())
                .with_legacy_official_auth_path(legacy_path);
        let (finished_tx, finished_rx) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            finished_tx
                .send(vault.get(CredentialSlot::OfficialAuthJson))
                .unwrap();
        });
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        FileExt::unlock(&held_legacy_lock).unwrap();

        assert!(
            finished_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .is_none()
        );
        worker.join().unwrap();
        assert!(legacy_lock_path.is_file());
        assert!(
            isolated_path
                .with_file_name(OFFICIAL_AUTH_LOCK_FILE_NAME)
                .is_file()
        );
    }
}
