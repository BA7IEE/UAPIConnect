//! One-shot activation requests for an already-running manager window.
//!
//! Launch Services may reactivate an existing macOS bundle without starting a
//! second process, while the Windows manager exits as soon as it loses the
//! single-instance guard. A tiny marker in U-API Connect's own state directory
//! gives both platforms the same, dependency-free hand-off path.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use serde::Serialize;

const PENDING_MANAGER_ACTIVATION_FILE: &str = "pending-manager-activation";
const CONFIGURE_MARKER: &str = "configure";

static ACTIVATION_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerActivation {
    Configure,
}

pub fn pending_manager_activation_path() -> PathBuf {
    crate::paths::default_app_state_dir().join(PENDING_MANAGER_ACTIVATION_FILE)
}

pub fn request_configure() -> anyhow::Result<()> {
    request_configure_at(&pending_manager_activation_path())
}

pub fn take_pending() -> anyhow::Result<Option<ManagerActivation>> {
    take_pending_at(&pending_manager_activation_path())
}

fn request_configure_at(path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("管理器激活标记缺少父目录"))?;
    fs::create_dir_all(parent).context("创建 U-API Connect 状态目录失败")?;

    let staging = sibling_work_path(path, "write");
    fs::write(&staging, format!("{CONFIGURE_MARKER}\n")).context("写入管理器激活临时标记失败")?;
    match fs::rename(&staging, path) {
        Ok(()) => Ok(()),
        // Windows does not replace an existing destination. All configure
        // requests are idempotent, so an existing regular marker is enough.
        Err(error) if path.is_file() => {
            let existing_is_configure =
                fs::read_to_string(path).is_ok_and(|contents| contents.trim() == CONFIGURE_MARKER);
            let _ = fs::remove_file(&staging);
            if existing_is_configure {
                Ok(())
            } else {
                Err(error).context("现有管理器激活标记无效，拒绝覆盖")
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&staging);
            Err(error).context("提交管理器激活标记失败")
        }
    }
}

fn take_pending_at(path: &Path) -> anyhow::Result<Option<ManagerActivation>> {
    let claimed = sibling_work_path(path, "claim");
    match fs::rename(path, &claimed) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("领取管理器激活标记失败"),
    }

    let contents = fs::read_to_string(&claimed).context("读取管理器激活标记失败");
    let _ = fs::remove_file(&claimed);
    match contents?.trim() {
        CONFIGURE_MARKER => Ok(Some(ManagerActivation::Configure)),
        marker => anyhow::bail!("忽略未知的管理器激活动作：{marker}"),
    }
}

fn sibling_work_path(path: &Path, purpose: &str) -> PathBuf {
    let sequence = ACTIVATION_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(PENDING_MANAGER_ACTIVATION_FILE);
    path.with_file_name(format!(
        ".{file_name}.{purpose}-{}-{sequence}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_activation_is_consumed_once() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PENDING_MANAGER_ACTIVATION_FILE);

        request_configure_at(&path).unwrap();

        assert_eq!(
            take_pending_at(&path).unwrap(),
            Some(ManagerActivation::Configure)
        );
        assert_eq!(take_pending_at(&path).unwrap(), None);
    }

    #[test]
    fn repeated_configure_requests_are_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PENDING_MANAGER_ACTIVATION_FILE);

        request_configure_at(&path).unwrap();
        request_configure_at(&path).unwrap();

        assert_eq!(
            take_pending_at(&path).unwrap(),
            Some(ManagerActivation::Configure)
        );
        assert_eq!(take_pending_at(&path).unwrap(), None);
    }

    #[test]
    fn unknown_activation_is_claimed_but_not_replayed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PENDING_MANAGER_ACTIVATION_FILE);
        fs::write(&path, "unknown\n").unwrap();

        let error = take_pending_at(&path).unwrap_err();

        assert!(error.to_string().contains("未知的管理器激活动作"));
        assert_eq!(take_pending_at(&path).unwrap(), None);
    }

    #[test]
    fn default_marker_belongs_to_uapi_state_directory() {
        assert!(
            pending_manager_activation_path().ends_with(".uapi-connect/pending-manager-activation")
        );
    }
}
