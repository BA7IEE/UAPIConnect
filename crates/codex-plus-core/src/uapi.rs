//! Managed NewAPI integration for the U-API Connect distribution.
//!
//! The module deliberately sits beside the generic relay implementation instead
//! of changing it.  The distribution owns one relay profile while the upstream
//! relay, model-catalog, backup and rollback code remains reusable.

mod credentials;
mod desktop_compat;

pub use desktop_compat::{
    desktop_compatibility_enabled, desktop_compatibility_script, install_desktop_compatibility,
};

use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::path::Path;

use anyhow::Context;
use credentials::{CredentialSlot, CredentialVault, SystemCredentialVault};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item, TableLike};

use crate::distribution;
use crate::settings::{
    BackendSettings, RelayMode, RelayModelInsertMode, RelayProfile, RelayProtocol, SettingsStore,
};

const DEFAULT_CONTEXT_WINDOW: &str = "128000";
const LARGE_CONTEXT_WINDOW: &str = "272000";
const MAX_MODEL_ID_LEN: usize = 200;
const OFFICIAL_RELAY_ID: &str = "uapi_official";
const OFFICIAL_CODEX_PROVIDER_ID: &str = "openai";
const REFRESH_REQUEST_MARKER: &str = ".uapi-connect-refresh-request";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum UapiConnectionMode {
    #[default]
    Uapi,
    Official,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UapiModelInfo {
    pub id: String,
    pub supported_endpoint_types: Vec<String>,
    pub compatible: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UapiModelDiscovery {
    pub endpoint: String,
    pub models: Vec<UapiModelInfo>,
    pub compatible_models: Vec<String>,
    pub filtered_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UapiStatus {
    pub configured: bool,
    pub active: bool,
    pub connection_mode: UapiConnectionMode,
    pub uapi_ready: bool,
    pub official_login_saved: bool,
    pub official_authenticated: bool,
    pub official_account_label: Option<String>,
    pub credential_store_available: bool,
    pub credential_store_message: String,
    pub provider_id: String,
    pub base_url: String,
    pub current_model: String,
    pub compatible_models: Vec<String>,
    pub model_count: usize,
    pub api_key_masked: String,
    pub config_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UapiApplyResult {
    pub configured: bool,
    pub current_model: String,
    pub compatible_models: Vec<String>,
    pub filtered_models: Vec<String>,
    pub backup_path: Option<String>,
    pub config_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UapiModeSwitchResult {
    pub connection_mode: UapiConnectionMode,
    pub configured: bool,
    pub official_login_saved: bool,
    pub official_authenticated: bool,
    pub backup_path: Option<String>,
    pub config_path: String,
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelCandidate {
    id: String,
    supported_endpoint_types: Vec<String>,
}

pub fn status() -> UapiStatus {
    let vault = SystemCredentialVault::default();
    let store = SettingsStore::default();
    let migration = prepare_default_distribution_state(&store, &vault);
    let mut status =
        status_from_home_with_vault(&crate::codex_home::default_codex_home_dir(), &store, &vault);
    if let Err(error) = migration {
        status.credential_store_available = false;
        status.credential_store_message = format!("迁移旧版 U-API 数据失败：{error}");
    }
    status
}

pub fn status_from_home(home: &Path, store: &SettingsStore) -> UapiStatus {
    let vault = SystemCredentialVault::default();
    status_from_home_with_vault(home, store, &vault)
}

fn status_from_home_with_vault(
    home: &Path,
    store: &SettingsStore,
    vault: &impl CredentialVault,
) -> UapiStatus {
    match crate::relay_config::with_live_files_transaction(home, || {
        Ok(status_from_home_with_vault_locked(home, store, vault, true))
    }) {
        Ok(status) => status,
        Err(_) => status_from_home_with_vault_locked(home, store, vault, false),
    }
}

fn status_from_home_with_vault_locked(
    home: &Path,
    store: &SettingsStore,
    vault: &impl CredentialVault,
    allow_migration: bool,
) -> UapiStatus {
    let mut settings = store.load().unwrap_or_default();
    let legacy_profile_key = settings
        .relay_profiles
        .iter()
        .find(|profile| managed_profile_is_owned(profile))
        .map(crate::relay_config::relay_profile_api_key)
        .filter(|key| !key.trim().is_empty());
    let has_legacy_key = legacy_profile_key.is_some();
    let legacy_migration = if allow_migration {
        migrate_legacy_managed_api_key(store, &mut settings, vault)
    } else if has_legacy_key {
        Err(anyhow::anyhow!(
            "Codex 配置事务锁不可用，已跳过明文密钥迁移"
        ))
    } else {
        Ok(false)
    };
    let (stored_api_key, stored_official_auth, credential_store_available) =
        read_stored_credentials(vault);
    let profile = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        .and_then(|profile| canonicalize_managed_profile(profile).ok());
    let live = read_live_managed_state(home);
    let api_key = stored_api_key
        .filter(|api_key| !api_key.trim().is_empty())
        .or(legacy_profile_key)
        .or_else(|| live_managed_api_key(home, &live))
        .unwrap_or_default();
    let compatible_models = profile.as_ref().map(profile_model_ids).unwrap_or_default();
    let official_auth = crate::relay_config::chatgpt_auth_status_from_home(home);
    let official_login_saved = stored_official_auth.as_deref().is_some_and(|contents| {
        sanitize_stored_official_auth_contents(contents)
            .ok()
            .flatten()
            .is_some()
    });
    let current_model = live
        .model
        .filter(|model| contains_model(&compatible_models, model))
        .or_else(|| {
            profile
                .as_ref()
                .map(crate::relay_config::relay_profile_model)
                .filter(|model| contains_model(&compatible_models, model))
        })
        .unwrap_or_default();
    let configured = live.provider_matches
        && live.base_url_matches
        && !api_key.trim().is_empty()
        && !compatible_models.is_empty();
    let connection_mode = connection_mode(&settings);
    let uapi_is_exactly_active = settings.relay_profiles_enabled
        && settings.active_relay_id == distribution::FIXED_PROVIDER_ID
        && settings.active_aggregate_relay_id.trim().is_empty();
    UapiStatus {
        configured,
        active: uapi_is_exactly_active,
        connection_mode,
        uapi_ready: !api_key.trim().is_empty() && !compatible_models.is_empty(),
        official_login_saved,
        official_authenticated: official_auth.authenticated || official_login_saved,
        official_account_label: official_auth.account_label,
        credential_store_available: credential_store_available && legacy_migration.is_ok(),
        credential_store_message: if legacy_migration.is_err() {
            "检测到旧版明文密钥，但未能迁移到系统凭证库；原配置已保留，请检查系统钥匙串或凭据管理器。"
                .to_string()
        } else if !credential_store_available {
            "系统凭证库暂不可用，请检查系统钥匙串或凭据管理器。".to_string()
        } else if matches!(legacy_migration, Ok(true)) {
            "系统凭证库可用，旧版明文密钥已安全迁移。".to_string()
        } else {
            "系统凭证库可用，登录信息不会写入普通设置文件。".to_string()
        },
        provider_id: distribution::FIXED_PROVIDER_ID.to_string(),
        base_url: distribution::FIXED_BASE_URL.to_string(),
        current_model,
        model_count: compatible_models.len(),
        compatible_models,
        api_key_masked: mask_api_key(&api_key),
        config_path: home.join("config.toml").to_string_lossy().to_string(),
    }
}

fn read_stored_credentials(vault: &impl CredentialVault) -> (Option<String>, Option<String>, bool) {
    let api_key = vault.get(CredentialSlot::UapiApiKey);
    let official_auth = vault.get(CredentialSlot::OfficialAuthJson);
    let available = api_key.is_ok() && official_auth.is_ok();
    (
        api_key.ok().flatten(),
        official_auth.ok().flatten(),
        available,
    )
}

fn connection_mode(settings: &BackendSettings) -> UapiConnectionMode {
    if settings.active_relay_id == OFFICIAL_RELAY_ID {
        UapiConnectionMode::Official
    } else {
        UapiConnectionMode::Uapi
    }
}

fn active_connection_mode_for_launch(
    settings: &BackendSettings,
) -> anyhow::Result<Option<UapiConnectionMode>> {
    if !settings.relay_profiles_enabled {
        return Ok(None);
    }
    if !settings.active_aggregate_relay_id.trim().is_empty() {
        anyhow::bail!("当前激活了聚合中转，拒绝覆盖 Codex 实时连接");
    }
    match settings.active_relay_id.as_str() {
        distribution::FIXED_PROVIDER_ID => Ok(Some(UapiConnectionMode::Uapi)),
        OFFICIAL_RELAY_ID => Ok(Some(UapiConnectionMode::Official)),
        _ => anyhow::bail!("当前激活的中转不属于 U-API Connect，拒绝覆盖 Codex 实时连接"),
    }
}

fn official_auth_contents_are_valid(contents: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(contents) else {
        return false;
    };
    value.get("OPENAI_API_KEY").is_none() && official_auth_value_has_valid_tokens(&value)
}

fn official_auth_value_has_valid_tokens(value: &Value) -> bool {
    let is_chatgpt = value
        .get("auth_mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode.eq_ignore_ascii_case("chatgpt"));
    is_chatgpt
        && value
            .get("tokens")
            .and_then(Value::as_object)
            .is_some_and(|tokens| {
                ["access_token", "id_token", "refresh_token"]
                    .iter()
                    .any(|key| {
                        tokens
                            .get(*key)
                            .and_then(Value::as_str)
                            .is_some_and(|token| !token.trim().is_empty())
                    })
            })
}

fn sanitize_live_official_auth_contents(
    contents: &str,
    owned_api_keys: &[String],
) -> anyhow::Result<Option<String>> {
    let Ok(mut value) = serde_json::from_str::<Value>(contents) else {
        return Ok(None);
    };
    if !official_auth_value_has_valid_tokens(&value) {
        return Ok(None);
    }
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let key_may_be_removed = match object.get("OPENAI_API_KEY") {
        None => return Ok(Some(contents.to_string())),
        Some(Value::Null) => true,
        Some(Value::String(api_key)) if api_key.trim().is_empty() => true,
        Some(Value::String(api_key)) => owned_api_keys
            .iter()
            .any(|owned| owned.trim() == api_key.trim()),
        Some(_) => false,
    };
    if !key_may_be_removed {
        anyhow::bail!("Codex 实时 auth.json 的官方登录中夹带无法确认归属的 API Key");
    }
    object.remove("OPENAI_API_KEY");
    let sanitized = serde_json::to_string_pretty(&value)?;
    if !official_auth_contents_are_valid(&sanitized) {
        anyhow::bail!("Codex 实时官方登录净化后无效");
    }
    Ok(Some(sanitized))
}

fn sanitize_stored_official_auth_contents(contents: &str) -> anyhow::Result<Option<String>> {
    let Ok(mut value) = serde_json::from_str::<Value>(contents) else {
        return Ok(None);
    };
    if !official_auth_value_has_valid_tokens(&value) {
        return Ok(None);
    }
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let removed_api_key = object.remove("OPENAI_API_KEY").is_some();
    let sanitized = if removed_api_key {
        serde_json::to_string_pretty(&value)?
    } else {
        contents.to_string()
    };
    if !official_auth_contents_are_valid(&sanitized) {
        anyhow::bail!("已存官方登录快照净化后无效");
    }
    Ok(Some(sanitized))
}

pub fn enforce_distribution_defaults() -> anyhow::Result<()> {
    let store = SettingsStore::default();
    let home = crate::codex_home::default_codex_home_dir();
    let vault = SystemCredentialVault::default();
    crate::relay_config::with_live_files_transaction(&home, || {
        prepare_default_distribution_state(&store, &vault)?;
        let mut settings = store.load().context("读取 U-API Connect 发行版设置失败")?;
        migrate_legacy_managed_api_key(&store, &mut settings, &vault)?;
        apply_distribution_feature_defaults(&mut settings);
        store
            .save(&settings)
            .context("保存 U-API Connect 发行版设置失败")
    })
}

pub async fn validate_key(api_key: &str) -> anyhow::Result<UapiModelDiscovery> {
    discover_models(api_key).await
}

pub async fn configure(api_key: &str) -> anyhow::Result<UapiApplyResult> {
    let api_key = normalize_api_key(api_key)?;
    let vault = SystemCredentialVault::default();
    let store = SettingsStore::default();
    let home = crate::codex_home::default_codex_home_dir();
    let request_guard = crate::relay_config::with_live_files_transaction(&home, || {
        begin_model_request(&store, &home)
    })?;
    let discovery = discover_models(&api_key).await?;
    apply_configured_discovery_with_guard(
        &store,
        &home,
        &vault,
        &api_key,
        &request_guard,
        discovery,
    )
}

pub async fn refresh_models() -> anyhow::Result<UapiApplyResult> {
    let store = SettingsStore::default();
    let vault = SystemCredentialVault::default();
    let home = crate::codex_home::default_codex_home_dir();
    prepare_default_distribution_state(&store, &vault)?;
    let (api_key, migration_succeeded, refresh_guard) =
        crate::relay_config::with_live_files_transaction(&home, || {
            let mut settings = store.load().context("读取本地连接配置失败")?;
            let migration_succeeded = migrate_legacy_managed_api_key_best_effort(
                &store,
                &mut settings,
                &vault,
                "uapi.legacy_credential_migration_deferred_before_refresh",
            );
            let api_key = managed_api_key(&settings, &vault, &home)?;
            let refresh_guard = begin_model_refresh_request(&store, &home, &settings, &api_key)?;
            Ok((api_key, migration_succeeded, refresh_guard))
        })?;
    let discovery = discover_models(&api_key).await?;
    apply_refreshed_discovery_with_guard(
        &store,
        &home,
        &vault,
        &api_key,
        &refresh_guard,
        discovery,
        !migration_succeeded,
    )
}

pub fn switch_connection_mode(mode: UapiConnectionMode) -> anyhow::Result<UapiModeSwitchResult> {
    let store = SettingsStore::default();
    let home = crate::codex_home::default_codex_home_dir();
    let vault = SystemCredentialVault::default();
    prepare_default_distribution_state(&store, &vault)?;
    switch_connection_mode_with(&store, &home, &vault, mode)
}

/// Applies the active U-API Connect mode before the native Codex app launches.
///
/// This small distribution hook keeps the upstream launcher generic while
/// ensuring that credentials are hydrated only in the live Codex files.
pub fn apply_active_connection_profile() -> anyhow::Result<()> {
    let store = SettingsStore::default();
    let home = crate::codex_home::default_codex_home_dir();
    let vault = SystemCredentialVault::default();
    prepare_default_distribution_state(&store, &vault)?;
    apply_active_connection_profile_with(&store, &home, &vault)
}

/// Removes U-API-owned live projections, credentials and profile state before
/// the desktop binaries are uninstalled. The operation deliberately leaves all
/// unrelated Codex configuration and authentication data in place.
pub fn uninstall_cleanup() -> anyhow::Result<()> {
    let home = crate::codex_home::default_codex_home_dir();
    let vault = SystemCredentialVault::default();
    uninstall_cleanup_with(
        &crate::paths::default_settings_path(),
        &crate::paths::legacy_upstream_settings_path(),
        &home,
        &vault,
    )
}

fn apply_active_connection_profile_with(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
) -> anyhow::Result<()> {
    crate::relay_config::with_live_files_transaction(home, || {
        apply_active_connection_profile_locked(store, home, vault)
    })
}

fn apply_active_connection_profile_locked(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
) -> anyhow::Result<()> {
    let mut settings = store.load().context("读取 U-API Connect 设置失败")?;
    let Some(initial_mode) = active_connection_mode_for_launch(&settings)? else {
        return Ok(());
    };
    let preflight_uapi_key = if initial_mode == UapiConnectionMode::Uapi {
        let api_key = managed_api_key(&settings, vault, home)?;
        if let Some(contents) = preflight_automatic_uapi_auth(home, &api_key)? {
            vault
                .set(CredentialSlot::OfficialAuthJson, &contents)
                .context("保存最新官方登录失败，为避免丢失已保留 Codex 实时登录")?;
        }
        Some(api_key)
    } else {
        None
    };
    migrate_legacy_managed_api_key_best_effort(
        store,
        &mut settings,
        vault,
        "uapi.legacy_credential_migration_deferred_before_launch",
    );
    let mode = active_connection_mode_for_launch(&settings)?
        .ok_or_else(|| anyhow::anyhow!("启动前 U-API Connect 模式发生变化"))?;
    if mode != initial_mode {
        anyhow::bail!("启动前 U-API Connect 模式发生变化，拒绝覆盖实时连接");
    }

    match mode {
        UapiConnectionMode::Uapi => {
            let api_key = managed_api_key(&settings, vault, home)?;
            if preflight_uapi_key
                .as_deref()
                .is_none_or(|preflight_key| preflight_key.trim() != api_key.trim())
            {
                anyhow::bail!("启动自修复前后 U-API 密钥已改变，拒绝覆盖实时连接");
            }
            let mut profile = managed_profile(&settings)?;
            prioritize_profile_models(&mut profile);
            let profile = hydrate_managed_profile(&profile, &api_key)?;
            crate::relay_config::apply_relay_profile_to_home_with_switch_rules(
                home,
                &profile,
                &managed_common_config(&settings),
            )?;
        }
        UapiConnectionMode::Official => {
            let auth_contents = stored_official_auth_best_effort(
                vault,
                "uapi.official_auth_snapshot_unavailable_before_launch",
            );
            clear_managed_config_for_official(home, auth_contents.as_deref(), &settings, vault)?;
            // 先完成实时文件的归属校验和密钥剥离，再从最终
            // auth.json 刷新快照，避免失败转换提前污染凭证库。
            let _ = official_auth_for_launch(home, vault)?;
        }
    }
    Ok(())
}

pub async fn discover_models(api_key: &str) -> anyhow::Result<UapiModelDiscovery> {
    let api_key = normalize_api_key(api_key)?;
    let endpoint = format!(
        "{}/models",
        distribution::FIXED_BASE_URL.trim_end_matches('/')
    );
    let client = crate::http_client::proxied_client(distribution::PRODUCT_NAME)?;
    let response = client
        .get(&endpoint)
        .timeout(std::time::Duration::from_secs(30))
        .bearer_auth(&api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(map_connect_error)?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("密钥无效，请检查后重试");
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!("当前密钥没有使用权限");
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        anyhow::bail!("请求过于频繁，请稍后重试");
    }
    if status.is_server_error() {
        anyhow::bail!("服务暂时不可用，请稍后重试");
    }
    if !status.is_success() {
        anyhow::bail!("服务验证失败（状态码 {}）", status.as_u16());
    }

    let payload = response
        .json::<Value>()
        .await
        .map_err(|_| anyhow::anyhow!("服务返回的数据无法识别，请稍后重试"))?;
    parse_model_discovery(&endpoint, &payload)
}

fn parse_model_discovery(endpoint: &str, payload: &Value) -> anyhow::Result<UapiModelDiscovery> {
    let items = payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .ok_or_else(|| anyhow::anyhow!("服务返回的数据中没有模型列表"))?;

    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for item in items {
        let Some(candidate) = parse_model_candidate(item) else {
            continue;
        };
        if !seen.insert(candidate.id.to_ascii_lowercase()) {
            continue;
        }
        let (compatible, reason) = compatibility(&candidate);
        models.push(UapiModelInfo {
            id: candidate.id,
            supported_endpoint_types: candidate.supported_endpoint_types,
            compatible,
            reason,
        });
    }
    models.sort_by(|left, right| compare_model_priority(&left.id, &right.id));

    let compatible_models = models
        .iter()
        .filter(|model| model.compatible)
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    let filtered_models = models
        .iter()
        .filter(|model| !model.compatible)
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();

    if compatible_models.is_empty() {
        anyhow::bail!("当前密钥没有返回可用模型");
    }

    Ok(UapiModelDiscovery {
        endpoint: endpoint.to_string(),
        models,
        compatible_models,
        filtered_models,
    })
}

#[cfg(test)]
fn apply_discovery_with(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    api_key: &str,
    discovery: UapiModelDiscovery,
) -> anyhow::Result<UapiApplyResult> {
    apply_discovery_with_options(store, home, vault, api_key, discovery, true, false)
}

fn apply_discovery_with_options(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    api_key: &str,
    discovery: UapiModelDiscovery,
    persist_api_key: bool,
    preserve_legacy_api_key: bool,
) -> anyhow::Result<UapiApplyResult> {
    crate::relay_config::with_live_files_transaction(home, || {
        apply_discovery_with_options_locked(
            store,
            home,
            vault,
            api_key,
            discovery,
            persist_api_key,
            preserve_legacy_api_key,
        )
    })
}

fn apply_configured_discovery_with_guard(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    api_key: &str,
    request_guard: &ModelRefreshGuard,
    discovery: UapiModelDiscovery,
) -> anyhow::Result<UapiApplyResult> {
    apply_configured_discovery_with_guard_and_prepare(
        store,
        home,
        vault,
        api_key,
        request_guard,
        discovery,
        || {
            migrate_legacy_distribution_state_with(
                store,
                &crate::paths::default_settings_path(),
                &crate::paths::legacy_upstream_settings_path(),
                vault,
            )
        },
    )
}

fn apply_configured_discovery_with_guard_and_prepare<F>(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    api_key: &str,
    request_guard: &ModelRefreshGuard,
    discovery: UapiModelDiscovery,
    prepare: F,
) -> anyhow::Result<UapiApplyResult>
where
    F: FnOnce() -> anyhow::Result<()>,
{
    crate::relay_config::with_live_files_transaction(home, || {
        ensure_model_request_is_current(store, home, request_guard)?;
        // 旧版迁移可能写设置或凭证，必须在请求代次和原始状态
        // 都复核通过之后才允许执行。同一文件事务锁会一直持有到 apply 结束。
        prepare()?;
        apply_discovery_with_options_locked(store, home, vault, api_key, discovery, true, false)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelRefreshStateSnapshot {
    settings: Option<Vec<u8>>,
    config: Option<Vec<u8>>,
    auth: Option<Vec<u8>>,
    managed_catalog: Option<Vec<u8>>,
}

impl ModelRefreshStateSnapshot {
    fn capture(store: &SettingsStore, home: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            settings: store
                .snapshot_bytes()
                .context("读取模型刷新前设置快照失败")?,
            config: read_optional_bytes(&home.join("config.toml"))
                .context("读取模型刷新前 config.toml 快照失败")?,
            auth: read_optional_bytes(&home.join("auth.json"))
                .context("读取模型刷新前 auth.json 快照失败")?,
            managed_catalog: read_optional_bytes(&managed_catalog_path(home))
                .context("读取模型刷新前模型目录快照失败")?,
        })
    }
}

#[derive(Debug, Clone)]
struct ModelRefreshGuard {
    request_id: String,
    state: ModelRefreshStateSnapshot,
}

fn refresh_request_marker_path(home: &Path) -> std::path::PathBuf {
    home.join(REFRESH_REQUEST_MARKER)
}

fn begin_model_request(store: &SettingsStore, home: &Path) -> anyhow::Result<ModelRefreshGuard> {
    let state = ModelRefreshStateSnapshot::capture(store, home)?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let marker_path = refresh_request_marker_path(home);
    let previous_marker =
        read_optional_bytes(&marker_path).context("读取上一个模型请求标记失败")?;
    if let Err(error) = crate::settings::atomic_write(&marker_path, request_id.as_bytes()) {
        if let Err(rollback_error) = restore_optional_file(&marker_path, previous_marker.as_deref())
        {
            anyhow::bail!(
                "记录模型请求失败，且旧标记回滚失败：写入={error}，回滚={rollback_error}"
            );
        }
        return Err(error).context("记录模型请求失败");
    }
    Ok(ModelRefreshGuard { request_id, state })
}

fn ensure_model_request_is_current(
    store: &SettingsStore,
    home: &Path,
    request_guard: &ModelRefreshGuard,
) -> anyhow::Result<()> {
    let current_request_id = match std::fs::read_to_string(refresh_request_marker_path(home)) {
        Ok(request_id) => request_id,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            anyhow::bail!("模型请求已被更新的操作取代，已丢弃过期结果")
        }
        Err(error) => return Err(error).context("读取模型请求标记失败"),
    };
    if current_request_id != request_guard.request_id {
        anyhow::bail!("模型请求已被更新的操作取代，已丢弃过期结果");
    }
    let current_state = ModelRefreshStateSnapshot::capture(store, home)?;
    if current_state != request_guard.state {
        anyhow::bail!("模型请求期间本地连接状态已改变，已丢弃过期结果");
    }
    Ok(())
}

fn ensure_model_refresh_target_is_active(settings: &BackendSettings) -> anyhow::Result<()> {
    if !settings.relay_profiles_enabled
        || settings.active_relay_id != distribution::FIXED_PROVIDER_ID
        || !settings.active_aggregate_relay_id.trim().is_empty()
    {
        anyhow::bail!("模型刷新期间连接模式已改变，已丢弃过期结果");
    }
    Ok(())
}

fn ensure_live_model_refresh_projection(home: &Path, api_key: &str) -> anyhow::Result<()> {
    let live = read_live_managed_state(home);
    if !live.provider_matches || !live.base_url_matches {
        anyhow::bail!("Codex 实时配置已被其他供应商接管，已停止刷新模型");
    }
    let auth_contents =
        std::fs::read_to_string(home.join("auth.json")).context("读取模型刷新前 auth.json 失败")?;
    let auth = serde_json::from_str::<Value>(&auth_contents)
        .context("Codex 实时 auth.json 已损坏，已停止刷新模型")?;
    let object = auth
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Codex 实时 auth.json 根节点不是 JSON 对象"))?;
    let live_key = object
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty());
    if object.len() != 1 || live_key != Some(api_key.trim()) {
        anyhow::bail!("Codex 实时 auth.json 已被外部修改，已停止刷新模型");
    }
    Ok(())
}

fn begin_model_refresh_request(
    store: &SettingsStore,
    home: &Path,
    settings: &BackendSettings,
    api_key: &str,
) -> anyhow::Result<ModelRefreshGuard> {
    ensure_model_refresh_target_is_active(settings)?;
    ensure_live_model_refresh_projection(home, api_key)?;
    begin_model_request(store, home)
}

fn apply_refreshed_discovery_with_guard(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    requested_api_key: &str,
    refresh_guard: &ModelRefreshGuard,
    discovery: UapiModelDiscovery,
    preserve_legacy_api_key: bool,
) -> anyhow::Result<UapiApplyResult> {
    crate::relay_config::with_live_files_transaction(home, || {
        let settings = store.load().context("读取本地连接配置失败")?;
        ensure_model_refresh_target_is_active(&settings)?;
        let current_api_key = managed_api_key(&settings, vault, home)?;
        if current_api_key.trim() != requested_api_key.trim() {
            anyhow::bail!("模型刷新期间服务密钥已改变，已丢弃过期结果");
        }
        ensure_model_request_is_current(store, home, refresh_guard)?;
        ensure_live_model_refresh_projection(home, &current_api_key)?;
        apply_discovery_with_options_locked(
            store,
            home,
            vault,
            requested_api_key,
            discovery,
            false,
            preserve_legacy_api_key,
        )
    })
}

fn apply_discovery_with_options_locked(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    api_key: &str,
    discovery: UapiModelDiscovery,
    persist_api_key: bool,
    preserve_legacy_api_key: bool,
) -> anyhow::Result<UapiApplyResult> {
    let mut settings = store.load().context("读取本地连接配置失败")?;
    let existing_managed_profile = settings
        .relay_profiles
        .iter()
        .find(|profile| managed_profile_is_owned(profile))
        .cloned();
    let existing_managed_model = existing_managed_profile
        .as_ref()
        .map(crate::relay_config::relay_profile_model)
        .unwrap_or_default();
    let live_state = read_live_managed_state(home);
    let live_model = live_state.model.unwrap_or_default();
    // 与已写入 profile 的默认值不同，才视为用户后来在 Codex 里手动改过模型。
    // 默认模型则会在模型目录更新时升级到当前最强的 GPT。
    let manually_selected_model = (live_state.provider_matches
        && !live_model.trim().is_empty()
        && !live_model.eq_ignore_ascii_case(&existing_managed_model))
    .then_some(&live_model);
    let selected_model = choose_model(&discovery.compatible_models, manually_selected_model)
        .ok_or_else(|| anyhow::anyhow!("没有可用于 Codex 的模型"))?;

    let mut profile = build_managed_profile(&selected_model, &discovery.compatible_models)?;
    if preserve_legacy_api_key {
        preserve_legacy_key_in_profile(existing_managed_profile.as_ref(), &mut profile, api_key)?;
    }
    let mut owned_api_keys = Vec::new();
    collect_owned_profile_keys(&settings.relay_profiles, &mut owned_api_keys);
    match stored_managed_api_key(&settings, vault) {
        Ok(Some(stored_api_key)) => {
            let stored_api_key = stored_api_key.trim();
            if !stored_api_key.is_empty()
                && !owned_api_keys
                    .iter()
                    .any(|owned| owned.trim() == stored_api_key)
            {
                owned_api_keys.push(stored_api_key.to_string());
            }
        }
        Ok(None) => {}
        Err(error) if persist_api_key && owned_api_keys.is_empty() => {
            return Err(error).context("读取当前 U-API 密钥失败，无法确认旧实时凭证归属");
        }
        Err(_) => {}
    }
    let api_key = api_key.trim();
    if !owned_api_keys.iter().any(|owned| owned.trim() == api_key) {
        owned_api_keys.push(api_key.to_string());
    }
    let live_official_auth = capture_live_official_auth_with_owned_keys(home, &owned_api_keys)?;
    let profile_for_apply = hydrate_managed_profile(&profile, api_key)?;
    upsert_managed_profile(&mut settings, profile);
    apply_distribution_feature_defaults(&mut settings);
    settings.relay_profiles_enabled = true;
    settings.active_relay_id = distribution::FIXED_PROVIDER_ID.to_string();
    settings.active_aggregate_relay_id.clear();
    settings.relay_test_model = selected_model.clone();

    let switched = commit_connection_change(
        store,
        home,
        vault,
        &settings,
        persist_api_key.then_some(api_key),
        live_official_auth.as_deref(),
        || {
            crate::relay_config::apply_relay_profile_to_home_with_switch_rules(
                home,
                &profile_for_apply,
                &managed_common_config(&settings),
            )
        },
    )?;

    Ok(UapiApplyResult {
        configured: read_live_managed_state(home).provider_matches,
        current_model: selected_model,
        compatible_models: discovery.compatible_models,
        filtered_models: discovery.filtered_models,
        backup_path: switched.backup_path,
        config_path: home.join("config.toml").to_string_lossy().into_owned(),
    })
}

fn build_managed_profile(selected_model: &str, models: &[String]) -> anyhow::Result<RelayProfile> {
    let selected_model = selected_model.trim();
    if selected_model.is_empty() {
        anyhow::bail!("默认模型不能为空");
    }
    let mut models = models.to_vec();
    models.sort_by(|left, right| compare_model_priority(left, right));
    let model_list = models.join("\n");
    let model_windows = models
        .iter()
        .map(|model| {
            let window = if is_known_openai_family(model) {
                LARGE_CONTEXT_WINDOW
            } else {
                DEFAULT_CONTEXT_WINDOW
            };
            (model.clone(), window.to_string())
        })
        .collect::<BTreeMap<_, _>>();
    let config_contents = format!(
        "model = {}\nmodel_provider = {}\n\n[model_providers.{}]\nname = {}\nbase_url = {}\nwire_api = \"responses\"\nrequires_openai_auth = true\n",
        toml_string(selected_model),
        toml_string(distribution::FIXED_PROVIDER_ID),
        distribution::FIXED_PROVIDER_ID,
        toml_string(distribution::FIXED_PROVIDER_NAME),
        toml_string(distribution::FIXED_BASE_URL),
    );
    let mut profile = RelayProfile {
        id: distribution::FIXED_PROVIDER_ID.to_string(),
        name: distribution::FIXED_PROVIDER_NAME.to_string(),
        model: selected_model.to_string(),
        base_url: distribution::FIXED_BASE_URL.to_string(),
        upstream_base_url: String::new(),
        api_key: String::new(),
        protocol: RelayProtocol::Responses,
        relay_mode: RelayMode::PureApi,
        no_auth: false,
        official_mix_api_key: false,
        hide_official_usage_alert: true,
        test_model: selected_model.to_string(),
        config_contents,
        auth_contents: String::new(),
        use_common_config: true,
        context_window: String::new(),
        auto_compact_limit: String::new(),
        model_insert_mode: RelayModelInsertMode::ModelCatalog,
        model_list,
        model_windows: serde_json::to_string(&model_windows)?,
        model_auto_compact: String::new(),
        model_metadata: String::new(),
        model_vlm: String::new(),
        vlm_api_key: String::new(),
        vlm_model: String::new(),
        vlm_base_url: String::new(),
        user_agent: distribution::PRODUCT_NAME.to_string(),
        sub2api_enabled: false,
        sub2api_multiplier: String::new(),
        model_routes: Vec::new(),
    };
    crate::relay_config::normalize_relay_profile_for_storage(&mut profile)?;
    Ok(profile)
}

fn switch_connection_mode_with(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    mode: UapiConnectionMode,
) -> anyhow::Result<UapiModeSwitchResult> {
    crate::relay_config::with_live_files_transaction(home, || {
        switch_connection_mode_locked(store, home, vault, mode)
    })
}

fn switch_connection_mode_locked(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    mode: UapiConnectionMode,
) -> anyhow::Result<UapiModeSwitchResult> {
    match mode {
        UapiConnectionMode::Uapi => switch_to_uapi(store, home, vault),
        UapiConnectionMode::Official => switch_to_official(store, home, vault),
    }
}

fn switch_to_uapi(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
) -> anyhow::Result<UapiModeSwitchResult> {
    let mut settings = store.load().context("读取本地连接配置失败")?;
    let migration_succeeded = migrate_legacy_managed_api_key_best_effort(
        store,
        &mut settings,
        vault,
        "uapi.legacy_credential_migration_deferred_before_uapi_switch",
    );
    let Some(profile) = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        .and_then(|profile| canonicalize_managed_profile(profile).ok())
    else {
        return activate_unconfigured_uapi_mode(store, home, vault, settings);
    };
    if profile_model_ids(&profile).is_empty() {
        return activate_unconfigured_uapi_mode(store, home, vault, settings);
    }
    let Ok(api_key) = managed_api_key(&settings, vault, home) else {
        return activate_unconfigured_uapi_mode(store, home, vault, settings);
    };
    let mut profile = sanitize_managed_profile(profile)?;
    prioritize_profile_models(&mut profile);
    if !migration_succeeded {
        preserve_legacy_key_in_profile(
            settings
                .relay_profiles
                .iter()
                .find(|item| item.id == distribution::FIXED_PROVIDER_ID),
            &mut profile,
            &api_key,
        )?;
    }
    let owned_api_keys = [api_key.clone()];
    let live_official_auth = capture_live_official_auth_with_owned_keys(home, &owned_api_keys)?;
    let profile_for_apply = hydrate_managed_profile(&profile, &api_key)?;

    upsert_managed_profile(&mut settings, profile);
    apply_distribution_feature_defaults(&mut settings);
    settings.relay_profiles_enabled = true;
    settings.active_relay_id = distribution::FIXED_PROVIDER_ID.to_string();
    settings.active_aggregate_relay_id.clear();

    let applied = commit_connection_change(
        store,
        home,
        vault,
        &settings,
        migration_succeeded.then_some(api_key.as_str()),
        live_official_auth.as_deref(),
        || {
            crate::relay_config::apply_relay_profile_to_home_with_switch_rules(
                home,
                &profile_for_apply,
                &managed_common_config(&settings),
            )
        },
    )?;
    mode_switch_result(
        store,
        home,
        vault,
        UapiConnectionMode::Uapi,
        applied.backup_path,
    )
}

fn activate_unconfigured_uapi_mode(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    mut settings: BackendSettings,
) -> anyhow::Result<UapiModeSwitchResult> {
    // 还没有完整的中转配置时，也必须能从官方模式回到连接页；不改动当前
    // 官方登录或实时配置，避免用户在填写密钥前丢失可用的官方订阅。
    apply_distribution_feature_defaults(&mut settings);
    settings.relay_profiles_enabled = true;
    settings.active_relay_id = distribution::FIXED_PROVIDER_ID.to_string();
    settings.active_aggregate_relay_id.clear();
    store.save(&settings).context("保存 U-API 连接模式失败")?;
    mode_switch_result(store, home, vault, UapiConnectionMode::Uapi, None)
}

fn switch_to_official(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
) -> anyhow::Result<UapiModeSwitchResult> {
    let mut settings = store.load().context("读取本地连接配置失败")?;
    let migration_succeeded = migrate_legacy_managed_api_key_best_effort(
        store,
        &mut settings,
        vault,
        "uapi.legacy_credential_migration_deferred_before_official_switch",
    );
    // 此时只读取已存的纯官方快照。实时 auth.json 的归属判定、
    // owned key 剥离和最新 token 选择都由后面的文件事务完成。
    let saved_official_auth = stored_official_auth_best_effort(
        vault,
        "uapi.official_auth_snapshot_unavailable_before_official_switch",
    );

    if migration_succeeded {
        if let Some(profile) = settings
            .relay_profiles
            .iter_mut()
            .find(|profile| managed_profile_is_owned(profile))
        {
            *profile = sanitize_managed_profile(profile.clone())?;
        }
    } else if let Some(profile) = settings
        .relay_profiles
        .iter_mut()
        .find(|profile| managed_profile_is_owned(profile))
    {
        // Very old settings could deserialize a plaintext `apiKey` field that is
        // intentionally skipped on the next serialization. Keep the only usable
        // copy in the already-legacy auth field until a later secure migration.
        let legacy_key = crate::relay_config::relay_profile_api_key(profile);
        if !legacy_key.trim().is_empty() && auth_json_api_key(&profile.auth_contents).is_none() {
            let existing = profile.clone();
            preserve_legacy_key_in_profile(Some(&existing), profile, &legacy_key)?;
        }
    }
    apply_distribution_feature_defaults(&mut settings);
    settings.relay_profiles_enabled = true;
    settings.active_relay_id = OFFICIAL_RELAY_ID.to_string();
    settings.active_aggregate_relay_id.clear();

    let applied = commit_connection_change(store, home, vault, &settings, None, None, || {
        clear_managed_config_for_official(home, saved_official_auth.as_deref(), &settings, vault)
    })?;
    // 只有归属校验和文件转换成功后，才从最终 auth.json 刷新纯官方
    // 快照。存储同步失败不回滚已可用的官方实时登录。
    if let Some(contents) = capture_live_official_auth(home)?.as_deref() {
        if let Err(error) = vault.set(CredentialSlot::OfficialAuthJson, contents) {
            record_nonfatal_credential_error(
                "uapi.official_auth_snapshot_refresh_failed_after_official_switch",
                &error,
            );
        }
    }
    mode_switch_result(
        store,
        home,
        vault,
        UapiConnectionMode::Official,
        applied.backup_path,
    )
}

fn mode_switch_result(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    mode: UapiConnectionMode,
    backup_path: Option<String>,
) -> anyhow::Result<UapiModeSwitchResult> {
    let status = status_from_home_with_vault(home, store, vault);
    Ok(UapiModeSwitchResult {
        connection_mode: mode,
        configured: status.configured,
        official_login_saved: status.official_login_saved,
        official_authenticated: status.official_authenticated,
        backup_path,
        config_path: status.config_path,
        restart_required: true,
    })
}

fn managed_profile(settings: &BackendSettings) -> anyhow::Result<RelayProfile> {
    let profile = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        .ok_or_else(|| anyhow::anyhow!("尚未配置 U-API 服务密钥"))?;
    canonicalize_managed_profile(profile).context("U-API profile 无法安全重建")
}

fn managed_api_key(
    settings: &BackendSettings,
    vault: &impl CredentialVault,
    home: &Path,
) -> anyhow::Result<String> {
    let live = read_live_managed_state(home);
    match stored_managed_api_key(settings, vault) {
        Ok(Some(api_key)) => normalize_api_key(&api_key),
        Ok(None) => normalize_api_key(&live_managed_api_key(home, &live).unwrap_or_default()),
        Err(error) => {
            if let Some(api_key) = live_managed_api_key(home, &live) {
                normalize_api_key(&api_key)
            } else {
                Err(error).context("读取系统凭证库中的 U-API 密钥失败")
            }
        }
    }
}

fn prepare_default_distribution_state(
    store: &SettingsStore,
    vault: &impl CredentialVault,
) -> anyhow::Result<()> {
    let home = crate::codex_home::default_codex_home_dir();
    prepare_distribution_state_with(
        &home,
        store,
        &crate::paths::default_settings_path(),
        &crate::paths::legacy_upstream_settings_path(),
        vault,
    )
}

fn prepare_distribution_state_with(
    home: &Path,
    store: &SettingsStore,
    isolated_settings_path: &Path,
    legacy_settings_path: &Path,
    vault: &impl CredentialVault,
) -> anyhow::Result<()> {
    crate::relay_config::with_live_files_transaction(home, || {
        migrate_legacy_distribution_state_with(
            store,
            isolated_settings_path,
            legacy_settings_path,
            vault,
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyConnectionState {
    Inactive,
    Uapi,
    Official,
}

fn migrate_legacy_distribution_state_with(
    store: &SettingsStore,
    isolated_settings_path: &Path,
    legacy_settings_path: &Path,
    vault: &impl CredentialVault,
) -> anyhow::Result<()> {
    if isolated_settings_path == legacy_settings_path {
        return Ok(());
    }

    let mut isolated = if isolated_settings_path.exists() {
        // Never use a damaged new file as a reason to erase the only readable
        // legacy copy. The caller will surface this error and preserve both.
        Some(
            store
                .load()
                .context("读取独立 U-API 设置失败，旧版数据未改动")?,
        )
    } else {
        None
    };
    let isolated_has_managed_profile = isolated.as_ref().is_some_and(|settings| {
        settings
            .relay_profiles
            .iter()
            .any(|profile| canonicalize_managed_profile(profile).is_ok())
    });
    let legacy_has_owned_marker = legacy_settings_has_owned_marker(legacy_settings_path)?;
    let (legacy_profiles, legacy_connection_state) = if legacy_has_owned_marker {
        read_legacy_managed_profiles(legacy_settings_path)?
    } else {
        (Vec::new(), LegacyConnectionState::Inactive)
    };
    if legacy_has_owned_marker && legacy_profiles.is_empty() {
        anyhow::bail!("检测到旧版 U-API 标记，但无法确认可安全迁移的 profile；旧版数据未改动");
    }

    let profile_to_copy = if !isolated_has_managed_profile {
        legacy_profiles
            .first()
            .cloned()
            .map(sanitize_managed_profile)
            .transpose()?
    } else {
        None
    };
    let mut candidate_keys = Vec::new();
    if let Some(settings) = isolated.as_ref() {
        collect_owned_profile_keys(&settings.relay_profiles, &mut candidate_keys);
    }
    collect_owned_profile_keys(&legacy_profiles, &mut candidate_keys);
    if candidate_keys.len() > 1 {
        anyhow::bail!("独立设置与旧版共享设置中存在不同的 U-API 密钥；为避免覆盖，旧版数据未改动");
    }
    let credential_rollback = candidate_keys
        .first()
        .map(|key| reconcile_legacy_managed_api_key(vault, key))
        .transpose()?
        .flatten();

    if let Some(profile) = profile_to_copy {
        let isolated_settings = isolated.get_or_insert_with(BackendSettings::default);
        if isolated_settings.relay_profiles.as_slice() == [RelayProfile::default()] {
            isolated_settings.relay_profiles.clear();
        }
        upsert_managed_profile(isolated_settings, profile);
        isolated_settings.relay_profiles_enabled =
            legacy_connection_state != LegacyConnectionState::Inactive;
        isolated_settings.active_relay_id = match legacy_connection_state {
            LegacyConnectionState::Inactive => String::new(),
            LegacyConnectionState::Uapi => distribution::FIXED_PROVIDER_ID.to_string(),
            LegacyConnectionState::Official => OFFICIAL_RELAY_ID.to_string(),
        };
        isolated_settings.active_aggregate_relay_id.clear();
        apply_distribution_feature_defaults(isolated_settings);
        if let Err(save_error) = store.save(isolated_settings) {
            if let Some(previous_credential) = credential_rollback.as_ref() {
                restore_credential(
                    vault,
                    CredentialSlot::UapiApiKey,
                    previous_credential.as_deref(),
                )
                .context("独立设置写入失败，且 U-API 密钥回滚失败")?;
            }
            return Err(save_error).context("保存独立 U-API 设置失败，旧版数据未改动");
        }
    } else if let Some(isolated_settings) = isolated.as_mut() {
        migrate_legacy_managed_api_key(store, isolated_settings, vault)
            .context("清理独立设置中的旧版 U-API 明文密钥失败，旧版数据未改动")?;
    }

    // Reading through the system vault atomically moves the encrypted
    // U-API-owned snapshot into the isolated directory when necessary.
    if let Err(error) = vault.get(CredentialSlot::OfficialAuthJson) {
        // A damaged or temporarily unavailable official snapshot must not
        // block an otherwise valid U-API profile. The vault keeps both legacy
        // and isolated recovery paths and retries on later reads.
        record_nonfatal_credential_error("uapi.legacy_official_auth_migration_deferred", &error);
    }
    if legacy_has_owned_marker {
        remove_owned_settings_state(legacy_settings_path, false)
            .context("清理旧版共享设置中的 U-API 数据失败")?;
    }
    Ok(())
}

fn legacy_settings_has_owned_marker(path: &Path) -> anyhow::Result<bool> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("读取旧版共享设置失败：{}", path.display()));
        }
    };
    if let Ok(root) = serde_json::from_slice::<Value>(&bytes) {
        return Ok(root
            .get("relayProfiles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(managed_profile_value_is_owned));
    }

    // A malformed shared file should block cleanup only when its raw payload
    // still carries the complete generated transport signature. A bare ID or
    // an unrelated field containing the U-API URL is not ownership evidence.
    let raw = String::from_utf8_lossy(&bytes);
    Ok(raw.contains(&format!(
        "model_provider = \\\"{}\\\"",
        distribution::FIXED_PROVIDER_ID
    )) && raw.contains(&format!(
        "[model_providers.{}]",
        distribution::FIXED_PROVIDER_ID
    )) && raw.contains(&format!(
        "base_url = \\\"{}\\\"",
        distribution::FIXED_BASE_URL
    )) && raw.contains("wire_api = \\\"responses\\\"")
        && (raw.contains("requires_openai_auth = false")
            || raw.contains("requires_openai_auth = true")))
}

fn read_legacy_managed_profiles(
    path: &Path,
) -> anyhow::Result<(Vec<RelayProfile>, LegacyConnectionState)> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok((Vec::new(), LegacyConnectionState::Inactive));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("读取旧版共享设置失败：{}", path.display()));
        }
    };
    let root = serde_json::from_str::<Value>(&contents)
        .with_context(|| format!("旧版共享设置不是有效 JSON：{}", path.display()))?;
    let object = root
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("旧版共享设置根节点不是 JSON 对象：{}", path.display()))?;
    let mut profiles = Vec::new();
    for profile in object
        .get("relayProfiles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|profile| managed_profile_value_is_owned(profile))
    {
        profiles.push(
            serde_json::from_value::<RelayProfile>(profile.clone())
                .context("旧版 U-API profile 包含无法迁移的字段；为避免丢失密钥，旧版数据未改动")?,
        );
    }
    let enabled = object.get("relayProfilesEnabled").and_then(Value::as_bool) == Some(true);
    let aggregate_is_empty = match object.get("activeAggregateRelayId") {
        None => true,
        Some(value) => value
            .as_str()
            .is_some_and(|aggregate| aggregate.trim().is_empty()),
    };
    let connection_state = if enabled && aggregate_is_empty {
        match object.get("activeRelayId").and_then(Value::as_str) {
            Some(distribution::FIXED_PROVIDER_ID) => LegacyConnectionState::Uapi,
            Some(OFFICIAL_RELAY_ID) => LegacyConnectionState::Official,
            _ => LegacyConnectionState::Inactive,
        }
    } else {
        LegacyConnectionState::Inactive
    };
    Ok((profiles, connection_state))
}

fn collect_owned_profile_keys(profiles: &[RelayProfile], keys: &mut Vec<String>) {
    for key in profiles
        .iter()
        .filter(|profile| managed_profile_is_owned(profile))
        .map(crate::relay_config::relay_profile_api_key)
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
    {
        if !keys.iter().any(|existing| existing == &key) {
            keys.push(key);
        }
    }
}

fn collect_owned_profile_keys_from_file(path: &Path, keys: &mut Vec<String>) -> anyhow::Result<()> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("读取设置失败：{}", path.display()));
        }
    };
    let root = serde_json::from_str::<Value>(&contents)
        .with_context(|| format!("设置不是有效 JSON：{}", path.display()))?;
    let object = root
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("设置根节点不是 JSON 对象：{}", path.display()))?;
    for value in object
        .get("relayProfiles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|value| managed_profile_value_is_owned(value))
    {
        // Prefer the typed representation so extraction follows the normal
        // relay precedence. If an unrelated future field cannot deserialize,
        // fall back to the legacy plaintext locations without weakening
        // the strict ownership check above.
        let key = serde_json::from_value::<RelayProfile>(value.clone())
            .ok()
            .map(|profile| crate::relay_config::relay_profile_api_key(&profile))
            .filter(|key| !key.trim().is_empty())
            .or_else(|| {
                value
                    .get("authContents")
                    .and_then(Value::as_str)
                    .and_then(auth_json_api_key)
            })
            .or_else(|| {
                let contents = value.get("configContents")?.as_str()?;
                let config = contents.parse::<DocumentMut>().ok()?;
                config
                    .get("experimental_bearer_token")
                    .and_then(Item::as_str)
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .map(ToString::to_string)
            })
            .or_else(|| {
                value
                    .get("apiKey")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .map(ToString::to_string)
            });
        if let Some(key) = key {
            let key = key.trim();
            if !keys.iter().any(|existing| existing == key) {
                keys.push(key.to_string());
            }
        }
    }
    Ok(())
}

/// Returns the exact previous vault value only when this call wrote the slot.
fn reconcile_legacy_managed_api_key(
    vault: &impl CredentialVault,
    legacy_key: &str,
) -> anyhow::Result<Option<Option<String>>> {
    let legacy_key = legacy_key.trim();
    if legacy_key.is_empty() {
        return Ok(None);
    }
    let previous = vault
        .get(CredentialSlot::UapiApiKey)
        .context("读取系统凭证库中的 U-API 密钥失败")?;
    if let Some(existing) = previous.as_deref().filter(|value| !value.trim().is_empty()) {
        if existing.trim() == legacy_key {
            return Ok(None);
        }
        anyhow::bail!("系统凭证库与旧版设置中的 U-API 密钥不一致；为避免覆盖，原配置已保留");
    }

    vault
        .set(CredentialSlot::UapiApiKey, legacy_key)
        .context("迁移旧版 U-API 密钥到系统凭证库失败")?;
    let verification = vault
        .get(CredentialSlot::UapiApiKey)
        .context("校验已迁移的 U-API 密钥失败")
        .and_then(|stored| {
            stored
                .is_some_and(|stored| stored.trim() == legacy_key)
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("U-API 密钥写入后校验不一致"))
        });
    if let Err(verification_error) = verification {
        if let Err(rollback_error) =
            restore_credential(vault, CredentialSlot::UapiApiKey, previous.as_deref())
        {
            anyhow::bail!("U-API 密钥写入后校验失败，且凭证库回滚失败：{rollback_error}");
        }
        return Err(verification_error).context("U-API 密钥写入后校验失败，原凭证库状态已恢复");
    }
    Ok(Some(previous))
}

fn validate_owned_settings_state(path: &Path) -> anyhow::Result<()> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("读取设置失败：{}", path.display()));
        }
    };
    let root = serde_json::from_str::<Value>(&contents)
        .with_context(|| format!("设置不是有效 JSON：{}", path.display()))?;
    if !root.is_object() {
        anyhow::bail!("设置根节点不是 JSON 对象：{}", path.display());
    }
    Ok(())
}

/// Removes only the fixed profile and mode selector from a settings document.
/// Unknown and non-U-API fields are retained verbatim at the JSON value level.
fn remove_owned_settings_state(path: &Path, isolated_uapi_settings: bool) -> anyhow::Result<bool> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("读取设置失败：{}", path.display()));
        }
    };
    let mut root = serde_json::from_str::<Value>(&contents)
        .with_context(|| format!("设置不是有效 JSON：{}", path.display()))?;
    let object = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("设置根节点不是 JSON 对象：{}", path.display()))?;
    let mut changed = false;
    let mut removed_managed_profile = false;
    if let Some(profiles) = object
        .get_mut("relayProfiles")
        .and_then(Value::as_array_mut)
    {
        let before = profiles.len();
        profiles.retain(|profile| {
            if profile.get("id").and_then(Value::as_str) != Some(distribution::FIXED_PROVIDER_ID) {
                return true;
            }
            if isolated_uapi_settings {
                return false;
            }
            !managed_profile_value_is_owned(profile)
        });
        removed_managed_profile = profiles.len() != before;
        changed |= removed_managed_profile;
    }
    let active_id = object
        .get("activeRelayId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let active_mode_is_owned = active_id == distribution::FIXED_PROVIDER_ID
        || active_id == OFFICIAL_RELAY_ID && (removed_managed_profile || isolated_uapi_settings);
    if active_mode_is_owned {
        object.insert("activeRelayId".to_string(), Value::String(String::new()));
        changed = true;
    }
    if !changed {
        return Ok(false);
    }
    let bytes = serde_json::to_vec_pretty(&root).context("序列化清理后的设置失败")?;
    crate::settings::atomic_write(path, &bytes)
        .with_context(|| format!("保存清理后的设置失败：{}", path.display()))?;
    Ok(true)
}

fn migrate_legacy_managed_api_key_best_effort(
    store: &SettingsStore,
    settings: &mut BackendSettings,
    vault: &impl CredentialVault,
    diagnostic_event: &str,
) -> bool {
    match migrate_legacy_managed_api_key(store, settings, vault) {
        Ok(_) => true,
        Err(error) => {
            record_nonfatal_credential_error(diagnostic_event, &error);
            false
        }
    }
}

/// Moves credentials written by pre-vault versions out of settings.json.
///
/// The secure write happens first. Only after it succeeds do we sanitize and
/// save settings. If saving settings fails, the credential slot is restored to
/// its exact previous value so callers never observe a half-committed migration.
fn migrate_legacy_managed_api_key(
    store: &SettingsStore,
    settings: &mut BackendSettings,
    vault: &impl CredentialVault,
) -> anyhow::Result<bool> {
    let profile_indices = settings
        .relay_profiles
        .iter()
        .enumerate()
        .filter_map(|(index, profile)| managed_profile_is_owned(profile).then_some(index))
        .collect::<Vec<_>>();
    if profile_indices.is_empty() {
        return Ok(false);
    }
    let mut legacy_keys = Vec::new();
    for index in &profile_indices {
        let key = crate::relay_config::relay_profile_api_key(&settings.relay_profiles[*index]);
        let key = key.trim();
        if !key.is_empty() && !legacy_keys.iter().any(|existing| existing == key) {
            legacy_keys.push(key.to_string());
        }
    }
    if legacy_keys.len() > 1 {
        anyhow::bail!("本地设置中存在不同的旧版 U-API 明文密钥；为避免覆盖，原配置已保留");
    }
    let Some(legacy_key) = legacy_keys.first() else {
        return Ok(false);
    };

    // Validate every duplicate before writing the vault so a malformed later
    // profile cannot leave a half-migrated credential behind.
    let sanitized_profiles = profile_indices
        .iter()
        .map(|index| sanitize_managed_profile(settings.relay_profiles[*index].clone()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let credential_rollback = reconcile_legacy_managed_api_key(vault, legacy_key)?;

    let mut sanitized_settings = settings.clone();
    for (index, sanitized) in profile_indices.into_iter().zip(sanitized_profiles) {
        sanitized_settings.relay_profiles[index] = sanitized;
    }
    if let Err(save_error) = store.save(&sanitized_settings) {
        if let Some(previous_credential) = credential_rollback.as_ref() {
            if let Err(restore_error) = restore_credential(
                vault,
                CredentialSlot::UapiApiKey,
                previous_credential.as_deref(),
            ) {
                anyhow::bail!(
                    "清理旧版明文密钥失败，且系统凭证库回滚失败：设置={}，系统凭证库={}",
                    save_error,
                    restore_error
                );
            }
        }
        return Err(save_error).context("清理设置文件中的旧版明文密钥失败，原配置已保留");
    }
    *settings = sanitized_settings;
    Ok(true)
}

fn stored_managed_api_key(
    settings: &BackendSettings,
    vault: &impl CredentialVault,
) -> anyhow::Result<Option<String>> {
    let legacy_key = settings
        .relay_profiles
        .iter()
        .find(|profile| managed_profile_is_owned(profile))
        .map(crate::relay_config::relay_profile_api_key)
        .filter(|key| !key.trim().is_empty());
    let api_key = match vault.get(CredentialSlot::UapiApiKey) {
        Ok(Some(api_key)) if !api_key.trim().is_empty() => Some(api_key),
        Ok(_) => legacy_key,
        Err(_error) if legacy_key.is_some() => legacy_key,
        Err(error) => return Err(error),
    };
    Ok(api_key)
}

fn stored_official_auth(vault: &impl CredentialVault) -> anyhow::Result<Option<String>> {
    let Some(contents) = vault.get(CredentialSlot::OfficialAuthJson)? else {
        return Ok(None);
    };
    sanitize_stored_official_auth_contents(&contents)
}

fn stored_official_auth_best_effort(
    vault: &impl CredentialVault,
    diagnostic_event: &str,
) -> Option<String> {
    match stored_official_auth(vault) {
        Ok(contents) => contents,
        Err(error) => {
            record_nonfatal_credential_error(diagnostic_event, &error);
            None
        }
    }
}

fn official_auth_for_launch(
    home: &Path,
    vault: &impl CredentialVault,
) -> anyhow::Result<Option<String>> {
    // auth.json may have been refreshed by Codex after the last mode switch.
    // A valid live login is therefore authoritative and must replace a stale
    // snapshot before launch. Snapshot failures must not overwrite or block an
    // otherwise usable live login.
    if let Some(contents) = capture_live_official_auth(home)? {
        if let Err(error) = vault.set(CredentialSlot::OfficialAuthJson, &contents) {
            record_nonfatal_credential_error(
                "uapi.official_auth_snapshot_refresh_failed_before_launch",
                &error,
            );
        }
        return Ok(Some(contents));
    }
    Ok(stored_official_auth_best_effort(
        vault,
        "uapi.official_auth_snapshot_unavailable_before_launch",
    ))
}

fn record_nonfatal_credential_error(event: &str, error: &anyhow::Error) {
    #[cfg(not(test))]
    {
        let _ = crate::diagnostic_log::append_diagnostic_log(
            event,
            json!({ "message": error.to_string() }),
        );
    }
    #[cfg(test)]
    let _ = (event, error);
}

fn capture_live_official_auth(home: &Path) -> anyhow::Result<Option<String>> {
    capture_live_official_auth_with_owned_keys(home, &[])
}

fn capture_live_official_auth_with_owned_keys(
    home: &Path,
    owned_api_keys: &[String],
) -> anyhow::Result<Option<String>> {
    let contents = match std::fs::read_to_string(home.join("auth.json")) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("读取当前官方登录信息失败"),
    };
    sanitize_live_official_auth_contents(&contents, owned_api_keys)
}

fn preflight_automatic_uapi_auth(
    home: &Path,
    expected_uapi_key: &str,
) -> anyhow::Result<Option<String>> {
    let contents = match std::fs::read_to_string(home.join("auth.json")) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("读取启动前 Codex auth.json 失败"),
    };
    let value = serde_json::from_str::<Value>(&contents)
        .context("Codex 实时 auth.json 已损坏，拒绝在启动时自动覆盖")?;
    let object = value.as_object().ok_or_else(|| {
        anyhow::anyhow!("Codex 实时 auth.json 根节点不是 JSON 对象，拒绝自动覆盖")
    })?;
    if object.is_empty() {
        return Ok(None);
    }
    let exact_owned_key_only = object.len() == 1
        && object
            .get("OPENAI_API_KEY")
            .and_then(Value::as_str)
            .is_some_and(|key| key.trim() == expected_uapi_key.trim());
    if exact_owned_key_only {
        return Ok(None);
    }
    let owned_api_keys = [expected_uapi_key.to_string()];
    if let Some(contents) = sanitize_live_official_auth_contents(&contents, &owned_api_keys)? {
        return Ok(Some(contents));
    }
    anyhow::bail!("Codex 实时 auth.json 不属于可安全自修复的 U-API 状态，拒绝自动覆盖")
}

fn clear_managed_config_for_official(
    home: &Path,
    auth_contents: Option<&str>,
    settings: &BackendSettings,
    vault: &impl CredentialVault,
) -> anyhow::Result<crate::relay_config::RelayApplyResult> {
    crate::relay_config::with_live_files_transaction(home, || {
        clear_managed_config_for_official_locked(home, auth_contents, settings, vault)
    })
}

fn clear_managed_config_for_official_locked(
    home: &Path,
    auth_contents: Option<&str>,
    settings: &BackendSettings,
    vault: &impl CredentialVault,
) -> anyhow::Result<crate::relay_config::RelayApplyResult> {
    let mut owned_api_keys = Vec::new();
    collect_owned_profile_keys(&settings.relay_profiles, &mut owned_api_keys);
    if let Ok(Some(api_key)) = stored_managed_api_key(settings, vault) {
        let api_key = api_key.trim();
        if !api_key.is_empty() && !owned_api_keys.iter().any(|existing| existing == api_key) {
            owned_api_keys.push(api_key.to_string());
        }
    }

    let config_path = home.join("config.toml");
    let config_contents = match std::fs::read_to_string(&config_path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("读取官方模式 config.toml 失败"),
    };
    let mut config = config_contents
        .as_deref()
        .map(str::parse::<DocumentMut>)
        .transpose()
        .context("Codex 实时 config.toml 已损坏，拒绝切换官方模式")?;

    let mut active_owned = false;
    let mut provider_owned = false;
    let mut catalog_owned = false;
    if let Some(document) = config.as_ref() {
        let active_provider = document
            .get("model_provider")
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|provider| !provider.is_empty());
        provider_owned = document
            .get("model_providers")
            .and_then(Item::as_table)
            .and_then(|providers| providers.get(distribution::FIXED_PROVIDER_ID))
            .and_then(Item::as_table_like)
            .is_some_and(managed_provider_table_is_owned);
        catalog_owned = document
            .get("model_catalog_json")
            .and_then(Item::as_str)
            .is_some_and(|path| managed_catalog_pointer_matches(home, path));
        active_owned = active_provider == Some(distribution::FIXED_PROVIDER_ID) && provider_owned;
        if live_config_has_unowned_official_transport_override(document) {
            anyhow::bail!("Codex 实时配置包含非 U-API 管理的官方传输覆盖，拒绝写入官方登录信息");
        }
        match active_provider {
            Some(provider) if provider == distribution::FIXED_PROVIDER_ID && !provider_owned => {
                anyhow::bail!("U-API 实时供应商配置已被修改，拒绝覆盖并切换官方模式");
            }
            Some(provider)
                if provider != distribution::FIXED_PROVIDER_ID
                    && provider != OFFICIAL_CODEX_PROVIDER_ID =>
            {
                anyhow::bail!("Codex 实时配置当前由其他供应商管理，拒绝覆盖并切换官方模式");
            }
            _ => {}
        }
    }

    let auth_bytes = official_auth_transition_bytes(home, auth_contents, &owned_api_keys)?;
    let mut config_changed = false;
    if let Some(document) = config.as_mut() {
        if active_owned {
            document.as_table_mut().remove("model_provider");
            document.as_table_mut().remove("model");
            config_changed = true;
        }
        if catalog_owned {
            document.as_table_mut().remove("model_catalog_json");
            config_changed = true;
        }
        if provider_owned {
            let providers_empty = document
                .get_mut("model_providers")
                .and_then(Item::as_table_mut)
                .map(|providers| {
                    providers.remove(distribution::FIXED_PROVIDER_ID);
                    providers.is_empty()
                })
                .unwrap_or(false);
            if providers_empty {
                document.as_table_mut().remove("model_providers");
            }
            config_changed = true;
        }
    }

    if config_changed || auth_bytes.is_some() {
        let snapshot = UapiLiveFilesSnapshot::capture(home)
            .context("读取切换官方模式前 Codex 实时配置快照失败")?;
        let transition_result = (|| {
            if config_changed {
                let contents = config.as_ref().map(ToString::to_string).unwrap_or_default();
                crate::settings::atomic_write(&config_path, contents.as_bytes())
                    .context("清理 U-API 实时供应商配置失败")?;
            }
            if let Some(auth_bytes) = auth_bytes.as_deref() {
                crate::settings::atomic_write_private(&home.join("auth.json"), auth_bytes)
                    .context("恢复 Codex 官方登录信息失败")?;
            }
            Ok(())
        })();
        if let Err(error) = transition_result {
            if let Err(rollback_error) = snapshot.restore(home) {
                anyhow::bail!(
                    "切换官方模式失败，且实时配置回滚不完整：切换={error}，回滚={rollback_error}"
                );
            }
            return Err(error);
        }
    }

    let status = crate::relay_config::relay_config_status_from_home(home);
    Ok(crate::relay_config::RelayApplyResult {
        config_path: status.config_path,
        backup_path: None,
        configured: status.configured,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LiveAuthForOfficial {
    Missing,
    Empty,
    Official,
    SanitizedOfficial(Vec<u8>),
    OfficialLike,
    OwnedUapiKey(String),
}

fn classify_live_auth_for_official(
    home: &Path,
    owned_api_keys: &[String],
) -> anyhow::Result<LiveAuthForOfficial> {
    let contents = match std::fs::read_to_string(home.join("auth.json")) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(LiveAuthForOfficial::Missing);
        }
        Err(error) => return Err(error).context("读取官方模式 auth.json 失败"),
    };
    let value = serde_json::from_str::<Value>(&contents)
        .context("Codex 实时 auth.json 已损坏，拒绝切换官方模式")?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Codex 实时 auth.json 根节点不是 JSON 对象"))?;
    if let Some(api_key) = object
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|api_key| !api_key.is_empty())
    {
        if owned_api_keys.iter().any(|owned| owned.trim() == api_key) {
            return Ok(LiveAuthForOfficial::OwnedUapiKey(api_key.to_string()));
        }
        anyhow::bail!("Codex 实时 auth.json 包含无法确认归属的 API Key，拒绝切换官方模式");
    }
    if let Some(sanitized) = sanitize_live_official_auth_contents(&contents, owned_api_keys)? {
        if sanitized == contents {
            return Ok(LiveAuthForOfficial::Official);
        }
        return Ok(LiveAuthForOfficial::SanitizedOfficial(
            sanitized.into_bytes(),
        ));
    }
    if object
        .get("auth_mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode.eq_ignore_ascii_case("chatgpt"))
    {
        return Ok(LiveAuthForOfficial::OfficialLike);
    }
    if object.is_empty() {
        return Ok(LiveAuthForOfficial::Empty);
    }
    anyhow::bail!("Codex 实时 auth.json 归属不明，拒绝切换官方模式")
}

fn official_auth_transition_bytes(
    home: &Path,
    auth_contents: Option<&str>,
    owned_api_keys: &[String],
) -> anyhow::Result<Option<Vec<u8>>> {
    let saved_official_auth =
        auth_contents.filter(|contents| official_auth_contents_are_valid(contents));
    match classify_live_auth_for_official(home, owned_api_keys)? {
        LiveAuthForOfficial::Missing
        | LiveAuthForOfficial::Empty
        | LiveAuthForOfficial::OfficialLike => {
            Ok(saved_official_auth.map(|contents| contents.as_bytes().to_vec()))
        }
        LiveAuthForOfficial::Official => Ok(None),
        LiveAuthForOfficial::SanitizedOfficial(contents) => Ok(Some(contents)),
        LiveAuthForOfficial::OwnedUapiKey(api_key) => {
            let stripped = remove_live_uapi_key_from_auth(home, &api_key)?
                .ok_or_else(|| anyhow::anyhow!("U-API 实时密钥在切换前发生变化"))?;
            let stripped_is_official = std::str::from_utf8(&stripped)
                .ok()
                .is_some_and(official_auth_contents_are_valid);
            if stripped_is_official {
                Ok(Some(stripped))
            } else if let Some(contents) = saved_official_auth {
                Ok(Some(contents.as_bytes().to_vec()))
            } else {
                Ok(Some(stripped))
            }
        }
    }
}

fn live_config_has_unowned_official_transport_override(document: &DocumentMut) -> bool {
    [
        "OPENAI_API_KEY",
        "base_url",
        "openai_base_url",
        "chatgpt_base_url",
        "codex_plus_chat_base_url",
        "experimental_bearer_token",
        "env_key",
        "requires_openai_auth",
    ]
    .iter()
    .any(|key| document.get(*key).is_some())
        || document
            .get("model_providers")
            .and_then(Item::as_table_like)
            .is_some_and(|providers| providers.get(OFFICIAL_CODEX_PROVIDER_ID).is_some())
}

fn managed_profile_is_owned(profile: &RelayProfile) -> bool {
    if profile.id != distribution::FIXED_PROVIDER_ID
        || profile.protocol != RelayProtocol::Responses
        || profile.relay_mode != RelayMode::PureApi
    {
        return false;
    }
    managed_config_is_owned(&profile.config_contents)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedProfileOwnershipView {
    id: String,
    #[serde(default)]
    protocol: RelayProtocol,
    #[serde(rename = "relayMode", default)]
    relay_mode: RelayMode,
    #[serde(rename = "configContents", default)]
    config_contents: String,
}

fn managed_profile_value_is_owned(value: &Value) -> bool {
    let Ok(profile) = serde_json::from_value::<ManagedProfileOwnershipView>(value.clone()) else {
        return false;
    };
    profile.id == distribution::FIXED_PROVIDER_ID
        && profile.protocol == RelayProtocol::Responses
        && profile.relay_mode == RelayMode::PureApi
        && managed_config_is_owned(&profile.config_contents)
}

fn managed_config_is_owned(config_contents: &str) -> bool {
    let Ok(config) = config_contents.parse::<DocumentMut>() else {
        return false;
    };
    if config.get("model_provider").and_then(Item::as_str) != Some(distribution::FIXED_PROVIDER_ID)
    {
        return false;
    }
    let Some(provider) = config
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(distribution::FIXED_PROVIDER_ID))
        .and_then(Item::as_table_like)
    else {
        return false;
    };
    managed_provider_table_is_owned(provider)
}

fn managed_provider_table_is_owned(provider: &dyn TableLike) -> bool {
    provider
        .get("base_url")
        .and_then(Item::as_str)
        .is_some_and(|url| normalize_base_url(url) == distribution::FIXED_BASE_URL)
        && provider.get("wire_api").and_then(Item::as_str) == Some("responses")
        // false 是旧版生成值，true 是使用 auth.json 中 API Key 的修正值。
        // 两者都可迁移；缺失或非布尔值仍不能作为受管配置的归属依据。
        && provider.get("requires_openai_auth").and_then(Item::as_bool).is_some()
}

/// Rebuilds the persisted managed profile from the only data users are allowed
/// to influence: discovered model identifiers and the selected model. This is
/// deliberately stricter than generic relay normalization, because accepting a
/// syntactically valid but modified provider table could send the U-API key to
/// an attacker-controlled endpoint during the next launch.
fn canonicalize_managed_profile(profile: &RelayProfile) -> anyhow::Result<RelayProfile> {
    if !managed_profile_is_owned(profile) {
        anyhow::bail!("U-API profile 的固定供应商配置已损坏或被修改");
    }
    let config = profile
        .config_contents
        .parse::<DocumentMut>()
        .context("U-API profile 配置格式无效")?;
    let selected_model = config
        .get("model")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|model| valid_model_id(model))
        .or_else(|| {
            let model = profile.model.trim();
            valid_model_id(model).then_some(model)
        });
    let mut seen = HashSet::new();
    let mut models = profile
        .model_list
        .split(['\r', '\n', ','])
        .map(str::trim)
        .filter(|model| valid_model_id(model))
        .filter(|model| seen.insert(model.to_ascii_lowercase()))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if let Some(selected_model) = selected_model
        && seen.insert(selected_model.to_ascii_lowercase())
    {
        models.push(selected_model.to_string());
    }
    if models.is_empty() {
        anyhow::bail!("U-API profile 没有可用的模型");
    }
    models.sort_by(|left, right| compare_model_priority(left, right));
    let selected_model = selected_model
        .and_then(|selected| {
            models
                .iter()
                .find(|model| model.eq_ignore_ascii_case(selected))
                .cloned()
        })
        .or_else(|| choose_model(&models, std::iter::empty::<&String>()))
        .ok_or_else(|| anyhow::anyhow!("U-API profile 没有可用的默认模型"))?;
    build_managed_profile(&selected_model, &models)
}

fn sanitize_managed_profile(profile: RelayProfile) -> anyhow::Result<RelayProfile> {
    canonicalize_managed_profile(&profile)
}

fn uninstall_cleanup_with(
    isolated_settings_path: &Path,
    legacy_settings_path: &Path,
    home: &Path,
    vault: &impl CredentialVault,
) -> anyhow::Result<()> {
    uninstall_cleanup_with_failure(
        isolated_settings_path,
        legacy_settings_path,
        home,
        vault,
        None,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UninstallFailurePoint {
    AfterIsolatedSettingsCleanup,
    AfterCatalogCleanup,
}

fn uninstall_cleanup_with_failure(
    isolated_settings_path: &Path,
    legacy_settings_path: &Path,
    home: &Path,
    vault: &impl CredentialVault,
    failure_point: Option<UninstallFailurePoint>,
) -> anyhow::Result<()> {
    // Validate both settings documents before deleting any credential. A
    // damaged file must remain untouched and must keep the uninstaller around
    // so the user can repair or retry instead of silently losing data.
    validate_owned_settings_state(isolated_settings_path)?;
    let legacy_has_owned_marker = legacy_settings_path != isolated_settings_path
        && legacy_settings_has_owned_marker(legacy_settings_path)?;
    if legacy_has_owned_marker {
        validate_owned_settings_state(legacy_settings_path)?;
    }
    let mut owned_api_keys = Vec::new();
    for path in [isolated_settings_path, legacy_settings_path] {
        if path == legacy_settings_path && !legacy_has_owned_marker {
            continue;
        }
        collect_owned_profile_keys_from_file(path, &mut owned_api_keys)
            .context("读取卸载前 U-API profile 密钥失败")?;
    }

    crate::relay_config::with_live_files_transaction(home, || {
        let live_snapshot =
            UapiLiveFilesSnapshot::capture(home).context("读取卸载前 Codex 实时配置快照失败")?;
        let settings_snapshot =
            SettingsFilesSnapshot::capture(isolated_settings_path, legacy_settings_path)
                .context("读取卸载前 U-API 设置快照失败")?;
        let credential_snapshot = CredentialSnapshot::capture_for_uninstall(vault)
            .context("读取卸载前 U-API 凭证快照失败")?;

        let cleanup_result = (|| {
            cleanup_live_uapi_projection_locked(home, &credential_snapshot, &owned_api_keys)?;

            remove_owned_settings_state(isolated_settings_path, true)
                .context("删除独立设置中的 U-API profile 失败")?;
            fail_uninstall_if_requested(
                failure_point,
                UninstallFailurePoint::AfterIsolatedSettingsCleanup,
            )?;
            if legacy_has_owned_marker {
                remove_owned_settings_state(legacy_settings_path, false)
                    .context("删除旧版共享设置中的 U-API profile 失败")?;
            }

            ensure_managed_catalog_is_not_referenced(home)?;
            remove_file_if_present(&managed_catalog_path(home))
                .context("删除 U-API 受管模型目录失败")?;
            remove_file_if_present(&refresh_request_marker_path(home))
                .context("删除 U-API 模型刷新请求标记失败")?;
            fail_uninstall_if_requested(failure_point, UninstallFailurePoint::AfterCatalogCleanup)?;

            // Credentials are the least recoverable part of the transaction,
            // so delete them only after every file mutation has succeeded.
            vault
                .delete(CredentialSlot::UapiApiKey)
                .context("删除系统凭证库中的 U-API 密钥失败")?;
            vault
                .delete(CredentialSlot::OfficialAuthJson)
                .context("删除 U-API 加密官方登录快照失败")?;
            Ok(())
        })();

        match cleanup_result {
            Ok(()) => Ok(()),
            Err(error) => {
                let credential_restore_error = credential_snapshot.restore(vault).err();
                let settings_restore_error = settings_snapshot
                    .restore(isolated_settings_path, legacy_settings_path)
                    .err();
                let live_restore_error = live_snapshot.restore(home).err();
                if credential_restore_error.is_some()
                    || settings_restore_error.is_some()
                    || live_restore_error.is_some()
                {
                    anyhow::bail!(
                        "卸载前清理失败，且自动回滚不完整：清理={error}，系统凭证库={}，设置={}，Codex 文件={}",
                        rollback_status(credential_restore_error),
                        rollback_status(settings_restore_error),
                        rollback_status(live_restore_error),
                    );
                }
                Err(error)
            }
        }
    })
}

fn fail_uninstall_if_requested(
    requested: Option<UninstallFailurePoint>,
    current: UninstallFailurePoint,
) -> anyhow::Result<()> {
    if requested == Some(current) {
        anyhow::bail!("simulated uninstall cleanup failure at {current:?}");
    }
    Ok(())
}

fn cleanup_live_uapi_projection_locked(
    home: &Path,
    credential_snapshot: &CredentialSnapshot,
    owned_api_keys: &[String],
) -> anyhow::Result<()> {
    let config_path = home.join("config.toml");
    let config_contents = match std::fs::read_to_string(&config_path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("读取 Codex 实时配置失败"),
    };
    let mut config = config_contents
        .as_deref()
        .map(str::parse::<DocumentMut>)
        .transpose()
        .context("Codex 实时 config.toml 已损坏，拒绝在卸载时覆盖")?;

    let mut active_owned = false;
    let mut provider_owned = false;
    let mut config_changed = false;
    if let Some(document) = config.as_mut() {
        provider_owned = document
            .get("model_providers")
            .and_then(Item::as_table)
            .and_then(|providers| providers.get(distribution::FIXED_PROVIDER_ID))
            .and_then(Item::as_table_like)
            .is_some_and(managed_provider_table_is_owned);
        active_owned = provider_owned
            && document.get("model_provider").and_then(Item::as_str)
                == Some(distribution::FIXED_PROVIDER_ID);
        let catalog_owned = document
            .get("model_catalog_json")
            .and_then(Item::as_str)
            .is_some_and(|path| managed_catalog_pointer_matches(home, path));

        if active_owned {
            document.as_table_mut().remove("model_provider");
            document.as_table_mut().remove("model");
            config_changed = true;
        }
        if catalog_owned {
            document.as_table_mut().remove("model_catalog_json");
            config_changed = true;
        }
        if provider_owned {
            let providers_empty = document
                .get_mut("model_providers")
                .and_then(Item::as_table_mut)
                .map(|providers| {
                    providers.remove(distribution::FIXED_PROVIDER_ID);
                    providers.is_empty()
                })
                .unwrap_or(false);
            if providers_empty {
                document.as_table_mut().remove("model_providers");
            }
            config_changed = true;
        }
    }

    let stored_uapi_key = match &credential_snapshot.uapi_api_key {
        CapturedCredential::Present(key) => Some(key.as_str()),
        CapturedCredential::Unchanged | CapturedCredential::Missing => None,
    }
    .filter(|key| !key.trim().is_empty());
    let mut all_owned_api_keys = owned_api_keys.to_vec();
    if let Some(stored_uapi_key) = stored_uapi_key
        && !all_owned_api_keys
            .iter()
            .any(|owned| owned.trim() == stored_uapi_key.trim())
    {
        all_owned_api_keys.push(stored_uapi_key.to_string());
    }
    let live_auth_path = home.join("auth.json");
    let (live_auth_contents, live_auth_missing) = match std::fs::read_to_string(&live_auth_path) {
        Ok(contents) => (Some(contents), false),
        Err(error) if error.kind() == ErrorKind::NotFound => (None, true),
        // Invalid UTF-8 and other unreadable states are not proof of ownership.
        // The outer byte snapshot will still preserve or roll back the file.
        Err(_) => (None, false),
    };
    if let Some(contents) = live_auth_contents.as_deref() {
        // 有效官方 token 中若夹带非 owned key，不得以“保留原文件”
        // 为由继续删除其他所有权证据，必须让外层整体回滚。
        let _ = sanitize_live_official_auth_contents(contents, &all_owned_api_keys)?;
    }
    let live_auth_is_official = live_auth_contents
        .as_deref()
        .is_some_and(official_auth_contents_are_valid);
    let live_auth_key = live_auth_contents.as_deref().and_then(auth_json_api_key);
    let matching_owned_key = live_auth_key.as_deref().and_then(|live_key| {
        stored_uapi_key
            .as_deref()
            .filter(|key| key.trim() == live_key)
            .or_else(|| {
                owned_api_keys
                    .iter()
                    .map(String::as_str)
                    .find(|key| key.trim() == live_key)
            })
    });
    let auth_bytes = if active_owned {
        let stored_official_auth = match &credential_snapshot.official_auth_json {
            CapturedCredential::Present(contents) => {
                sanitize_stored_official_auth_contents(contents)?
            }
            CapturedCredential::Unchanged | CapturedCredential::Missing => None,
        };
        if let Some(matching_owned_key) = matching_owned_key {
            let stripped = remove_live_uapi_key_from_auth(home, matching_owned_key)?;
            let stripped_is_official = stripped
                .as_deref()
                .and_then(|contents| std::str::from_utf8(contents).ok())
                .is_some_and(official_auth_contents_are_valid);
            if stripped_is_official {
                stripped
            } else if let Some(contents) = stored_official_auth.as_deref() {
                Some(contents.as_bytes().to_vec())
            } else {
                stripped
            }
        } else if live_auth_is_official {
            None
        } else if live_auth_missing {
            stored_official_auth.map(String::into_bytes)
        } else {
            None
        }
    } else if provider_owned {
        if let Some(matching_owned_key) = matching_owned_key {
            remove_live_uapi_key_from_auth(home, matching_owned_key)?
        } else {
            None
        }
    } else {
        None
    };

    if !config_changed && auth_bytes.is_none() {
        return Ok(());
    }

    let snapshot =
        UapiLiveFilesSnapshot::capture(home).context("读取卸载前 Codex 实时配置快照失败")?;
    let result = (|| {
        if config_changed {
            let contents = config.as_ref().map(ToString::to_string).unwrap_or_default();
            crate::settings::atomic_write(&config_path, contents.as_bytes())
                .context("清理 Codex 实时 U-API 配置失败")?;
        }
        if let Some(auth_bytes) = auth_bytes.as_deref() {
            crate::settings::atomic_write_private(&home.join("auth.json"), auth_bytes)
                .context("恢复 Codex 官方登录信息失败")?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        if let Err(rollback_error) = snapshot.restore(home) {
            anyhow::bail!(
                "卸载前清理失败，且实时配置回滚不完整：清理={error}，回滚={rollback_error}"
            );
        }
        return Err(error);
    }
    Ok(())
}

fn remove_live_uapi_key_from_auth(
    home: &Path,
    expected_key: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let auth_path = home.join("auth.json");
    let contents = match std::fs::read_to_string(&auth_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("读取 Codex 实时 auth.json 失败"),
    };
    let mut value = serde_json::from_str::<Value>(&contents)
        .context("Codex 实时 auth.json 已损坏，拒绝在卸载时覆盖")?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Codex 实时 auth.json 根节点不是 JSON 对象"))?;
    let matches = object
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|value| value.trim() == expected_key.trim());
    if !matches {
        return Ok(None);
    }
    if object.remove("OPENAI_API_KEY").is_none() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_vec_pretty(&value)?))
}

fn managed_catalog_pointer_matches(home: &Path, pointer: &str) -> bool {
    let normalized = pointer.trim().replace('\\', "/");
    let relative = normalized.trim_start_matches("./");
    let expected_relative =
        crate::relay_config::managed_model_catalog_relative_path(distribution::FIXED_PROVIDER_ID)
            .replace('\\', "/");
    if relative.eq_ignore_ascii_case(&expected_relative) {
        return true;
    }
    let expected_absolute = managed_catalog_path(home)
        .to_string_lossy()
        .replace('\\', "/");
    normalized.eq_ignore_ascii_case(&expected_absolute)
}

fn ensure_managed_catalog_is_not_referenced(home: &Path) -> anyhow::Result<()> {
    let config_path = home.join("config.toml");
    let contents = match std::fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("检查 Codex 模型目录引用失败"),
    };
    let document = contents
        .parse::<DocumentMut>()
        .context("Codex 实时 config.toml 已损坏，拒绝删除可能仍在使用的模型目录")?;
    if document
        .get("model_catalog_json")
        .and_then(Item::as_str)
        .is_some_and(|path| managed_catalog_pointer_matches(home, path))
    {
        anyhow::bail!("Codex 实时配置仍引用 U-API 模型目录，拒绝删除");
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn preserve_legacy_key_in_profile(
    existing: Option<&RelayProfile>,
    profile: &mut RelayProfile,
    api_key: &str,
) -> anyhow::Result<()> {
    let existing_auth = existing
        .map(|existing| existing.auth_contents.trim())
        .filter(|contents| auth_json_api_key(contents).is_some());
    profile.auth_contents = match existing_auth {
        Some(contents) => contents.to_string(),
        None => serde_json::to_string_pretty(&json!({
            "OPENAI_API_KEY": api_key.trim()
        }))?,
    };
    Ok(())
}

fn hydrate_managed_profile(profile: &RelayProfile, api_key: &str) -> anyhow::Result<RelayProfile> {
    let mut profile = profile.clone();
    profile.api_key.clear();
    profile.auth_contents = serde_json::to_string_pretty(&json!({
        "OPENAI_API_KEY": api_key.trim()
    }))?;
    Ok(profile)
}

fn managed_common_config(settings: &BackendSettings) -> String {
    let sections = [
        settings.relay_common_config_contents.trim(),
        settings.relay_context_config_contents.trim(),
    ]
    .into_iter()
    .filter(|section| !section.is_empty())
    .collect::<Vec<_>>();
    if sections.is_empty() {
        String::new()
    } else {
        crate::relay_config::normalize_config_text(&format!("{}\n", sections.join("\n\n")))
    }
}

fn commit_connection_change<F>(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    next_settings: &BackendSettings,
    uapi_api_key: Option<&str>,
    official_auth: Option<&str>,
    apply: F,
) -> anyhow::Result<crate::relay_config::RelayApplyResult>
where
    F: FnOnce() -> anyhow::Result<crate::relay_config::RelayApplyResult>,
{
    crate::relay_config::with_live_files_transaction(home, || {
        commit_connection_change_locked(
            store,
            home,
            vault,
            next_settings,
            uapi_api_key,
            official_auth,
            apply,
        )
    })
}

fn commit_connection_change_locked<F>(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    next_settings: &BackendSettings,
    uapi_api_key: Option<&str>,
    official_auth: Option<&str>,
    apply: F,
) -> anyhow::Result<crate::relay_config::RelayApplyResult>
where
    F: FnOnce() -> anyhow::Result<crate::relay_config::RelayApplyResult>,
{
    let original_settings_bytes = store.snapshot_bytes().context("读取原始本地设置快照失败")?;
    store.load().context("读取原始本地设置失败")?;
    let live_snapshot =
        UapiLiveFilesSnapshot::capture(home).context("读取当前 Codex 实时配置失败")?;
    let credential_snapshot =
        CredentialSnapshot::capture(vault, uapi_api_key.is_some(), official_auth.is_some())
            .context("读取系统凭证库失败，请检查系统钥匙串或凭据管理器")?;

    let result = (|| {
        if let Some(api_key) = uapi_api_key {
            vault.set(CredentialSlot::UapiApiKey, api_key)?;
        }
        if let Some(auth_contents) = official_auth {
            vault.set(CredentialSlot::OfficialAuthJson, auth_contents)?;
        }
        store.save(next_settings).context("保存本地连接设置失败")?;
        apply()
    })();

    match result {
        Ok(result) => Ok(result),
        Err(error) => {
            let settings_restore_error = store
                .restore_bytes(original_settings_bytes.as_deref())
                .err();
            let live_restore_error = live_snapshot.restore(home).err();
            let credential_restore_error = credential_snapshot.restore(vault).err();
            if settings_restore_error.is_some()
                || live_restore_error.is_some()
                || credential_restore_error.is_some()
            {
                anyhow::bail!(
                    "更新连接配置失败，且自动回滚不完整：设置={}，Codex 文件={}，系统凭证库={}",
                    settings_restore_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "ok".to_string()),
                    live_restore_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "ok".to_string()),
                    credential_restore_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "ok".to_string()),
                );
            }
            Err(error)
        }
    }
}

#[derive(Debug, Clone)]
struct UapiLiveFilesSnapshot {
    config: Option<Vec<u8>>,
    auth: Option<Vec<u8>>,
    managed_catalog: Option<Vec<u8>>,
    refresh_request_marker: Option<Vec<u8>>,
}

impl UapiLiveFilesSnapshot {
    fn capture(home: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            config: read_optional_bytes(&home.join("config.toml"))?,
            auth: read_optional_bytes(&home.join("auth.json"))?,
            managed_catalog: read_optional_bytes(&managed_catalog_path(home))?,
            refresh_request_marker: read_optional_bytes(&refresh_request_marker_path(home))?,
        })
    }

    fn restore(&self, home: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(home)?;
        let config_error =
            restore_optional_file(&home.join("config.toml"), self.config.as_deref()).err();
        let auth_error =
            restore_optional_private_file(&home.join("auth.json"), self.auth.as_deref()).err();
        let catalog_error =
            restore_optional_file(&managed_catalog_path(home), self.managed_catalog.as_deref())
                .err();
        let refresh_marker_error = restore_optional_file(
            &refresh_request_marker_path(home),
            self.refresh_request_marker.as_deref(),
        )
        .err();
        if config_error.is_none()
            && auth_error.is_none()
            && catalog_error.is_none()
            && refresh_marker_error.is_none()
        {
            return Ok(());
        }
        anyhow::bail!(
            "Codex 实时文件回滚不完整：config.toml={}，auth.json={}，model catalog={}，refresh marker={}",
            rollback_status(config_error),
            rollback_status(auth_error),
            rollback_status(catalog_error),
            rollback_status(refresh_marker_error),
        )
    }
}

#[derive(Debug, Clone)]
struct SettingsFilesSnapshot {
    isolated: Option<Vec<u8>>,
    legacy: CapturedFile,
}

#[derive(Debug, Clone)]
enum CapturedFile {
    SameAsIsolated,
    Contents(Option<Vec<u8>>),
}

impl SettingsFilesSnapshot {
    fn capture(isolated_path: &Path, legacy_path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            isolated: read_optional_bytes(isolated_path)?,
            legacy: if isolated_path == legacy_path {
                CapturedFile::SameAsIsolated
            } else {
                CapturedFile::Contents(read_optional_bytes(legacy_path)?)
            },
        })
    }

    fn restore(&self, isolated_path: &Path, legacy_path: &Path) -> anyhow::Result<()> {
        let isolated_error = restore_optional_file(isolated_path, self.isolated.as_deref()).err();
        let legacy_error = match &self.legacy {
            CapturedFile::SameAsIsolated => None,
            CapturedFile::Contents(contents) => {
                restore_optional_file(legacy_path, contents.as_deref()).err()
            }
        };
        if isolated_error.is_none() && legacy_error.is_none() {
            return Ok(());
        }
        anyhow::bail!(
            "设置文件回滚不完整：独立设置={}，旧版共享设置={}",
            rollback_status(isolated_error),
            rollback_status(legacy_error),
        )
    }
}

fn managed_catalog_path(home: &Path) -> std::path::PathBuf {
    home.join("model-catalogs")
        .join(format!("{}.json", distribution::FIXED_PROVIDER_ID))
}

#[derive(Debug, Clone)]
struct CredentialSnapshot {
    uapi_api_key: CapturedCredential,
    official_auth_json: CapturedCredential,
}

#[derive(Debug, Clone)]
enum CapturedCredential {
    Unchanged,
    Missing,
    Present(String),
}

impl CredentialSnapshot {
    fn capture(
        vault: &impl CredentialVault,
        capture_uapi_api_key: bool,
        capture_official_auth_json: bool,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            uapi_api_key: capture_credential(
                vault,
                CredentialSlot::UapiApiKey,
                capture_uapi_api_key,
            )?,
            official_auth_json: capture_credential(
                vault,
                CredentialSlot::OfficialAuthJson,
                capture_official_auth_json,
            )?,
        })
    }

    fn capture_for_uninstall(vault: &impl CredentialVault) -> anyhow::Result<Self> {
        Self::capture(vault, true, true)
    }

    fn restore(&self, vault: &impl CredentialVault) -> anyhow::Result<()> {
        let uapi_error =
            restore_captured_credential(vault, CredentialSlot::UapiApiKey, &self.uapi_api_key)
                .err();
        let official_error = restore_captured_credential(
            vault,
            CredentialSlot::OfficialAuthJson,
            &self.official_auth_json,
        )
        .err();
        if uapi_error.is_none() && official_error.is_none() {
            return Ok(());
        }
        anyhow::bail!(
            "系统凭证库回滚不完整：U-API Key={}，官方登录快照={}",
            rollback_status(uapi_error),
            rollback_status(official_error),
        )
    }
}

fn rollback_status(error: Option<anyhow::Error>) -> String {
    error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "ok".to_string())
}

fn capture_credential(
    vault: &impl CredentialVault,
    slot: CredentialSlot,
    should_capture: bool,
) -> anyhow::Result<CapturedCredential> {
    if !should_capture {
        return Ok(CapturedCredential::Unchanged);
    }
    Ok(match vault.get(slot)? {
        Some(contents) => CapturedCredential::Present(contents),
        None => CapturedCredential::Missing,
    })
}

fn restore_captured_credential(
    vault: &impl CredentialVault,
    slot: CredentialSlot,
    captured: &CapturedCredential,
) -> anyhow::Result<()> {
    match captured {
        CapturedCredential::Unchanged => Ok(()),
        CapturedCredential::Missing => vault.delete(slot),
        CapturedCredential::Present(contents) => vault.set(slot, contents),
    }
}

fn restore_credential(
    vault: &impl CredentialVault,
    slot: CredentialSlot,
    contents: Option<&str>,
) -> anyhow::Result<()> {
    match contents {
        Some(contents) => vault.set(slot, contents),
        None => vault.delete(slot),
    }
}

fn read_optional_bytes(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_optional_file(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    match contents {
        Some(contents) => crate::settings::atomic_write(path, contents),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn restore_optional_private_file(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    match contents {
        Some(contents) => crate::settings::atomic_write_private(path, contents),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn upsert_managed_profile(settings: &mut BackendSettings, profile: RelayProfile) {
    if let Some(existing) = settings
        .relay_profiles
        .iter_mut()
        .find(|item| item.id == distribution::FIXED_PROVIDER_ID)
    {
        *existing = profile;
    } else {
        settings.relay_profiles.push(profile);
    }
}

pub fn apply_distribution_feature_defaults(settings: &mut BackendSettings) {
    // The managed edition relies on native Codex configuration and model_catalog_json.
    // 完整页面增强保持关闭；启动器只加载独立的语言/原生推理菜单兼容层，
    // 不加载广告、自定义脚本、主题或高级 Codex++ 菜单。
    settings.enhancements_enabled = false;
    settings.provider_sync_enabled = false;
    settings.provider_sync_saved_providers.clear();
    settings.provider_sync_manual_providers.clear();
    settings.provider_sync_last_selected_provider.clear();
    settings.codex_app_plugin_marketplace_unlock = false;
    settings.codex_app_model_whitelist_unlock = true;
    settings.codex_app_session_delete = false;
    settings.codex_app_markdown_export = false;
    settings.codex_app_paste_fix = false;
    settings.codex_app_thread_id_badge = false;
    settings.codex_app_conversation_view = false;
    settings.codex_app_thread_scroll_restore = false;
    settings.codex_app_zed_remote_open = false;
    settings.zed_remote_project_registry_enabled = false;
    settings.zed_remote_sync_to_zed_settings = false;
    settings.codex_app_upstream_worktree_create = false;
    settings.codex_app_native_menu_placement = false;
    settings.codex_app_native_menu_localization = false;
    settings.codex_app_service_tier_controls = false;
    settings.codex_app_pet_real_mouse_look = false;
    settings.codex_app_stepwise_enabled = false;
    settings.codex_app_stepwise_direct_send = false;
    settings.codex_app_answer_outline_enabled = false;
    settings.codex_app_image_overlay_enabled = false;
    settings.codex_app_dream_skin_enabled = false;
    settings.codex_app_dream_skin_paused = true;
    settings.codex_goals_enabled = false;
    settings.weixin_connect_enabled = false;
}

fn profile_model_ids(profile: &RelayProfile) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut models = profile
        .model_list
        .split(['\r', '\n', ','])
        .chain(std::iter::once(profile.model.as_str()))
        .map(str::trim)
        .filter(|model| valid_model_id(model))
        .filter(|model| seen.insert(model.to_ascii_lowercase()))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    models.sort_by(|left, right| compare_model_priority(left, right));
    models
}

fn prioritize_profile_models(profile: &mut RelayProfile) {
    profile.model_list = profile_model_ids(profile).join("\n");
}

#[derive(Debug, Default)]
struct LiveManagedState {
    provider_matches: bool,
    base_url_matches: bool,
    model: Option<String>,
}

fn auth_json_api_key(contents: &str) -> Option<String> {
    serde_json::from_str::<Value>(contents)
        .ok()?
        .get("OPENAI_API_KEY")?
        .as_str()
        .map(str::trim)
        .filter(|api_key| !api_key.is_empty())
        .map(ToString::to_string)
}

fn live_managed_api_key(home: &Path, live: &LiveManagedState) -> Option<String> {
    if !live.provider_matches || !live.base_url_matches {
        return None;
    }
    let contents = std::fs::read_to_string(home.join("auth.json")).ok()?;
    auth_json_api_key(&contents)
}

fn read_live_managed_state(home: &Path) -> LiveManagedState {
    let Ok(contents) = std::fs::read_to_string(home.join("config.toml")) else {
        return LiveManagedState::default();
    };
    let Ok(doc) = contents.parse::<DocumentMut>() else {
        return LiveManagedState::default();
    };
    let provider_id = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let provider = doc
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(distribution::FIXED_PROVIDER_ID))
        .and_then(Item::as_table_like);
    let base_url_matches = provider
        .and_then(|provider| provider.get("base_url"))
        .and_then(Item::as_str)
        .is_some_and(|url| normalize_base_url(url) == distribution::FIXED_BASE_URL);
    let provider_owned = provider.is_some_and(managed_provider_table_is_owned);
    LiveManagedState {
        provider_matches: provider_id == distribution::FIXED_PROVIDER_ID && provider_owned,
        base_url_matches,
        model: doc
            .get("model")
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|model| valid_model_id(model))
            .map(ToString::to_string),
    }
}

fn parse_model_candidate(item: &Value) -> Option<ModelCandidate> {
    let id = item
        .get("id")
        .or_else(|| item.get("name"))?
        .as_str()?
        .trim()
        .trim_start_matches("models/")
        .to_string();
    if !valid_model_id(&id) {
        return None;
    }
    let endpoints = item
        .get("supported_endpoint_types")
        .or_else(|| item.get("supportedEndpointTypes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|endpoint| endpoint.trim().to_ascii_lowercase())
        .filter(|endpoint| !endpoint.is_empty())
        .collect::<Vec<_>>();
    Some(ModelCandidate {
        id,
        supported_endpoint_types: endpoints,
    })
}

fn compatibility(candidate: &ModelCandidate) -> (bool, String) {
    // 与上游 model_catalog 保持一致：/v1/models 返回什么就列什么，
    // 是否支持 Responses 只作为一条状态信息展示，不用来隐藏模型。
    // 中转站常常不填 supported_endpoint_types，或只声明 openai/openai-chat，
    // 拿它当可见性开关会把实际可用的模型整片藏掉。
    // 唯一保留的排除项是明显不能当对话模型用的那些（embedding/rerank/tts 等）。
    if !is_text_model(&candidate.id) {
        return (false, "非文本模型".to_string());
    }
    if candidate
        .supported_endpoint_types
        .iter()
        .any(|endpoint| endpoint == "openai-response")
    {
        return (true, "明确支持 Responses API".to_string());
    }
    if candidate.supported_endpoint_types.is_empty() {
        return (true, "服务未返回端点元数据".to_string());
    }
    (
        true,
        "服务未声明 Responses 端点，仍按可用模型列出".to_string(),
    )
}

fn choose_model<'a, I>(models: &[String], preferred: I) -> Option<String>
where
    I: IntoIterator<Item = &'a String>,
{
    for preferred_model in preferred {
        let preferred_model = preferred_model.trim();
        if preferred_model.is_empty() {
            continue;
        }
        if let Some(model) = models
            .iter()
            .find(|model| model.eq_ignore_ascii_case(preferred_model))
        {
            return Some(model.clone());
        }
    }
    strongest_gpt_model(models)
        .or_else(|| {
            models
                .iter()
                .find(|model| model.to_ascii_lowercase().contains("codex"))
                .cloned()
        })
        .or_else(|| {
            models
                .iter()
                .find(|model| {
                    let lower = model.to_ascii_lowercase();
                    lower.starts_with("o1-") || lower.starts_with("o3-") || lower.starts_with("o4-")
                })
                .cloned()
        })
        .or_else(|| models.first().cloned())
}

fn compare_model_priority(left: &str, right: &str) -> std::cmp::Ordering {
    match (is_gpt_model(left), is_gpt_model(right)) {
        (true, true) => gpt_strength_key(right)
            .cmp(&gpt_strength_key(left))
            .then_with(|| left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase())),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (false, false) => left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()),
    }
}

fn strongest_gpt_model(models: &[String]) -> Option<String> {
    models
        .iter()
        .filter(|model| is_gpt_model(model))
        .min_by(|left, right| compare_model_priority(left, right))
        .cloned()
}

fn is_gpt_model(model: &str) -> bool {
    model.trim().to_ascii_lowercase().starts_with("gpt-")
}

fn gpt_strength_key(model: &str) -> (Vec<u32>, bool) {
    let lower = model.trim().to_ascii_lowercase();
    let version = lower
        .strip_prefix("gpt-")
        .unwrap_or_default()
        .split('-')
        .next()
        .unwrap_or_default()
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok())
        .collect();
    let is_sol = lower
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| part == "sol");
    (version, is_sol)
}

fn contains_model(models: &[String], candidate: &str) -> bool {
    models
        .iter()
        .any(|model| model.eq_ignore_ascii_case(candidate.trim()))
}

fn valid_model_id(model: &str) -> bool {
    let model = model.trim();
    !model.is_empty()
        && model.len() <= MAX_MODEL_ID_LEN
        && !model
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn is_text_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    ![
        "embedding",
        "rerank",
        "moderation",
        "whisper",
        "transcrib",
        "speech",
        "audio",
        "tts",
        "realtime",
        "image",
        "dall-e",
        "sora",
        "video",
    ]
    .iter()
    .any(|kind| lower.contains(kind))
}

fn is_known_openai_family(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("codex")
        || lower.starts_with("gpt-")
        || lower.starts_with("o1-")
        || lower.starts_with("o3-")
        || lower.starts_with("o4-")
}

fn normalize_api_key(api_key: &str) -> anyhow::Result<String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        anyhow::bail!("请输入服务密钥");
    }
    if api_key.len() < 16 || api_key.chars().any(char::is_whitespace) {
        anyhow::bail!("密钥格式不正确，请重新复制");
    }
    Ok(api_key.to_string())
}

fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn mask_api_key(api_key: &str) -> String {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return String::new();
    }
    let tail = api_key.chars().rev().take(4).collect::<String>();
    let tail = tail.chars().rev().collect::<String>();
    format!("****{tail}")
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn map_connect_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow::anyhow!("连接服务超时，请检查网络后重试")
    } else if error.is_connect() {
        anyhow::anyhow!("无法连接服务，请检查网络后重试")
    } else {
        anyhow::anyhow!("服务连接失败，请稍后重试")
    }
}

#[cfg(test)]
mod tests {
    use super::credentials::testing::MemoryCredentialVault;
    use super::*;

    #[derive(Debug, Default)]
    struct SingleReadCredentialVault {
        secrets: std::sync::Mutex<std::collections::HashMap<CredentialSlot, String>>,
        reads: std::sync::Mutex<std::collections::HashMap<CredentialSlot, usize>>,
    }

    impl SingleReadCredentialVault {
        fn read_count(&self, slot: CredentialSlot) -> usize {
            *self.reads.lock().unwrap().get(&slot).unwrap_or(&0)
        }

        fn peek(&self, slot: CredentialSlot) -> Option<String> {
            self.secrets.lock().unwrap().get(&slot).cloned()
        }
    }

    impl CredentialVault for SingleReadCredentialVault {
        fn get(&self, slot: CredentialSlot) -> anyhow::Result<Option<String>> {
            let mut reads = self.reads.lock().unwrap();
            let count = reads.entry(slot).or_default();
            *count += 1;
            if *count > 1 {
                anyhow::bail!("simulated credential read failure after first read");
            }
            Ok(self.secrets.lock().unwrap().get(&slot).cloned())
        }

        fn set(&self, slot: CredentialSlot, secret: &str) -> anyhow::Result<()> {
            self.secrets
                .lock()
                .unwrap()
                .insert(slot, secret.to_string());
            Ok(())
        }

        fn delete(&self, slot: CredentialSlot) -> anyhow::Result<()> {
            self.secrets.lock().unwrap().remove(&slot);
            Ok(())
        }
    }

    fn discovery(models: &[&str]) -> UapiModelDiscovery {
        UapiModelDiscovery {
            endpoint: "https://example.test/v1/models".to_string(),
            models: models
                .iter()
                .map(|id| UapiModelInfo {
                    id: (*id).to_string(),
                    supported_endpoint_types: vec!["openai-response".to_string()],
                    compatible: true,
                    reason: "测试模型".to_string(),
                })
                .collect(),
            compatible_models: models.iter().map(|id| (*id).to_string()).collect(),
            filtered_models: Vec::new(),
        }
    }

    fn official_auth_json() -> String {
        official_auth_json_with_access_token("official-access-token-for-test")
    }

    fn official_auth_json_with_access_token(access_token: &str) -> String {
        serde_json::to_string_pretty(&json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": access_token,
                "refresh_token": "official-refresh-token-for-test"
            }
        }))
        .unwrap()
    }

    fn managed_settings(profile: RelayProfile, mode: UapiConnectionMode) -> BackendSettings {
        let mut settings = BackendSettings::default();
        settings.relay_profiles_enabled = true;
        settings.active_relay_id = match mode {
            UapiConnectionMode::Uapi => distribution::FIXED_PROVIDER_ID.to_string(),
            UapiConnectionMode::Official => OFFICIAL_RELAY_ID.to_string(),
        };
        settings.relay_profiles.push(profile);
        settings
    }

    fn write_live_uapi(home: &Path, profile: &RelayProfile, api_key: &str) {
        let hydrated = hydrate_managed_profile(profile, api_key).unwrap();
        crate::relay_config::apply_relay_profile_to_home_with_switch_rules(home, &hydrated, "")
            .unwrap();
    }

    fn model_refresh_guard(store: &SettingsStore, home: &Path, api_key: &str) -> ModelRefreshGuard {
        crate::relay_config::with_live_files_transaction(home, || {
            let settings = store.load().unwrap();
            begin_model_refresh_request(store, home, &settings, api_key)
        })
        .unwrap()
    }

    fn configure_request_guard(store: &SettingsStore, home: &Path) -> ModelRefreshGuard {
        crate::relay_config::with_live_files_transaction(home, || begin_model_request(store, home))
            .unwrap()
    }

    fn candidate(id: &str, endpoints: &[&str]) -> ModelCandidate {
        ModelCandidate {
            id: id.to_string(),
            supported_endpoint_types: endpoints.iter().map(|item| item.to_string()).collect(),
        }
    }

    #[test]
    fn lists_chat_only_models_instead_of_hiding_them() {
        // 上游从不按端点能力过滤；只声明 openai/openai-chat 的模型也要列出来。
        assert!(compatibility(&candidate("deepseek-chat", &["openai"])).0);
        assert!(compatibility(&candidate("glm-4", &["openai"])).0);
        assert!(compatibility(&candidate("kimi-k3", &["openai-chat"])).0);
    }

    #[test]
    fn includes_explicit_responses_models() {
        assert!(
            compatibility(&candidate(
                "deepseek-v3-codex",
                &["openai", "openai-response"]
            ))
            .0
        );
    }

    #[test]
    fn lists_models_without_endpoint_metadata_regardless_of_family() {
        assert!(compatibility(&candidate("gpt-5.5", &[])).0);
        assert!(compatibility(&candidate("custom-codex", &[])).0);
        assert!(compatibility(&candidate("unknown-chat-model", &[])).0);
    }

    #[test]
    fn still_excludes_models_that_cannot_serve_chat() {
        assert!(!compatibility(&candidate("text-embedding-3-large", &["openai"])).0);
        assert!(!compatibility(&candidate("bge-reranker-v2", &[])).0);
        assert!(!compatibility(&candidate("whisper-1", &["openai"])).0);
    }

    #[test]
    fn responses_capability_is_reported_as_a_label_not_a_gate() {
        let (visible, reason) = compatibility(&candidate("glm-4", &["openai"]));
        assert!(visible);
        assert!(reason.contains("未声明 Responses"));

        let (visible, reason) = compatibility(&candidate("glm-4-codex", &["openai-response"]));
        assert!(visible);
        assert!(reason.contains("明确支持 Responses"));
    }

    #[test]
    fn discovery_and_default_model_prioritize_the_strongest_gpt() {
        let discovery = parse_model_discovery(
            "https://example.test/v1/models",
            &json!({
                "data": [
                    { "id": "kimi-k3" },
                    { "id": "gpt-5.6-luna" },
                    { "id": "gpt-5.6-sol" },
                    { "id": "deepseek-v4-pro" },
                    { "id": "gpt-5.5" },
                    { "id": "gpt-6" }
                ]
            }),
        )
        .unwrap();
        assert_eq!(
            discovery.compatible_models,
            vec![
                "gpt-6",
                "gpt-5.6-sol",
                "gpt-5.6-luna",
                "gpt-5.5",
                "deepseek-v4-pro",
                "kimi-k3"
            ]
        );

        let models = vec![
            "gpt-5.5".to_string(),
            "gpt-5.6-luna".to_string(),
            "gpt-5.6-sol".to_string(),
            "gpt-6".to_string(),
            "domestic-codex".to_string(),
        ];
        let current = "gpt-5.6-luna".to_string();
        assert_eq!(
            choose_model(&models, [&current]).as_deref(),
            Some("gpt-5.6-luna")
        );
        assert_eq!(
            choose_model(&models, std::iter::empty::<&String>()).as_deref(),
            Some("gpt-6")
        );

        let manually_selected = "domestic-codex".to_string();
        assert_eq!(
            choose_model(&models, [&manually_selected]).as_deref(),
            Some("domestic-codex")
        );
    }

    #[test]
    fn managed_profile_uses_matching_root_and_provider_table() {
        let profile = build_managed_profile(
            "gpt-custom-codex",
            &["gpt-custom-codex".to_string(), "domestic-codex".to_string()],
        )
        .unwrap();
        assert_eq!(
            crate::relay_config::root_key_string(&profile.config_contents, "model_provider")
                .as_deref(),
            Some(distribution::FIXED_PROVIDER_ID)
        );
        assert!(
            profile
                .config_contents
                .contains("[model_providers.uapi_connect]")
        );
        assert!(!profile.config_contents.contains("[model_providers.custom]"));
        assert!(
            profile
                .config_contents
                .contains("requires_openai_auth = true")
        );
    }

    #[test]
    fn managed_profile_serialization_does_not_expose_credentials() {
        let profile =
            build_managed_profile("gpt-custom-codex", &["gpt-custom-codex".to_string()]).unwrap();
        let value = serde_json::to_value(profile).unwrap();
        assert!(value.get("apiKey").is_none());
        assert_eq!(value.get("authContents").and_then(Value::as_str), Some(""));
    }

    #[test]
    fn managed_profile_ownership_requires_fixed_transport_contract() {
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        assert!(managed_profile_is_owned(&managed));

        let mut foreign_endpoint = managed.clone();
        foreign_endpoint.config_contents = foreign_endpoint
            .config_contents
            .replace(distribution::FIXED_BASE_URL, "https://foreign.example/v1");
        assert!(!managed_profile_is_owned(&foreign_endpoint));

        let mut wrong_protocol = managed.clone();
        wrong_protocol.protocol = RelayProtocol::ChatCompletions;
        assert!(!managed_profile_is_owned(&wrong_protocol));

        let mut wrong_mode = managed.clone();
        wrong_mode.relay_mode = RelayMode::MixedApi;
        assert!(!managed_profile_is_owned(&wrong_mode));

        let mut missing_provider_table = managed;
        missing_provider_table.config_contents =
            "model = \"gpt-5-codex\"\nmodel_provider = \"uapi_connect\"\n".to_string();
        assert!(!managed_profile_is_owned(&missing_provider_table));

        let mut wrong_wire =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        wrong_wire.config_contents = wrong_wire
            .config_contents
            .replace("wire_api = \"responses\"", "wire_api = \"chat\"");
        assert!(!managed_profile_is_owned(&wrong_wire));

        let mut invalid_auth =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        invalid_auth.config_contents = invalid_auth.config_contents.replace(
            "requires_openai_auth = true",
            "requires_openai_auth = \"true\"",
        );
        assert!(!managed_profile_is_owned(&invalid_auth));
    }

    #[test]
    fn legacy_auth_flag_is_owned_and_canonicalized_to_use_auth_json() {
        let mut legacy =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        legacy.config_contents = legacy.config_contents.replace(
            "requires_openai_auth = true",
            "requires_openai_auth = false",
        );
        assert!(managed_profile_is_owned(&legacy));
        assert!(managed_profile_value_is_owned(
            &serde_json::to_value(&legacy).unwrap()
        ));
        let canonical = canonicalize_managed_profile(&legacy).unwrap();
        assert!(
            canonical
                .config_contents
                .contains("requires_openai_auth = true")
        );
        assert!(
            !canonical
                .config_contents
                .contains("requires_openai_auth = false")
        );
    }

    #[test]
    fn launch_upgrades_legacy_and_manually_fixed_auth_without_reentering_key() {
        for old_flag in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("codex-home");
            std::fs::create_dir_all(&home).unwrap();
            let store = SettingsStore::new(temp.path().join("settings.json"));
            let mut profile = build_managed_profile("gpt-5.6", &["gpt-5.6".to_string()]).unwrap();
            profile.config_contents = profile.config_contents.replace(
                "requires_openai_auth = true",
                &format!("requires_openai_auth = {old_flag}"),
            );
            std::fs::write(home.join("config.toml"), &profile.config_contents).unwrap();
            let key = "test-auth-upgrade-key";
            std::fs::write(
                home.join("auth.json"),
                json!({"OPENAI_API_KEY": key}).to_string(),
            )
            .unwrap();
            store
                .save(&managed_settings(profile, UapiConnectionMode::Uapi))
                .unwrap();
            let vault = MemoryCredentialVault::default();
            vault.set(CredentialSlot::UapiApiKey, key).unwrap();

            apply_active_connection_profile_with(&store, &home, &vault).unwrap();

            let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
            assert!(config.contains("requires_openai_auth = true"));
            assert!(config.contains("model_catalog_json"));
            assert!(!config.contains(key));
            let auth: Value =
                serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
            assert_eq!(auth["OPENAI_API_KEY"], key);
            let catalog: Value =
                serde_json::from_slice(&std::fs::read(managed_catalog_path(&home)).unwrap())
                    .unwrap();
            let model = &catalog["models"][0];
            assert_eq!(model["slug"], "gpt-5.6");
            assert_eq!(model["display_name"], "GPT-5.6");
            let efforts = model["supported_reasoning_levels"].as_array().unwrap();
            for effort in ["max", "ultra"] {
                assert!(efforts.iter().any(|entry| entry["effort"] == effort));
            }
            assert!(
                !serde_json::to_string(&store.load().unwrap())
                    .unwrap()
                    .contains(key)
            );
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap();
            assert!(
                !std::fs::read_to_string(home.join("config.toml"))
                    .unwrap()
                    .contains("[model_providers.uapi_connect]")
            );
        }
    }

    #[test]
    fn canonical_profile_rebuild_discards_untrusted_config_and_secret_fields() {
        let mut profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        profile.config_contents.push_str(
            "\n[model_providers.untrusted]\nbase_url = \"https://foreign.example/v1\"\nexperimental_bearer_token = \"test-untrusted-key-value\"\n",
        );
        profile.auth_contents = r#"{"OPENAI_API_KEY":"test-legacy-profile-key"}"#.to_string();
        profile.upstream_base_url = "https://foreign.example/v1".to_string();
        profile.model_metadata =
            r#"{"gpt-5-codex":{"context_window":1,"slug":"foreign"}}"#.to_string();
        profile.model_auto_compact = r#"{"gpt-5-codex":"1%"}"#.to_string();
        profile.model_vlm = r#"{"gpt-5-codex":"vlm"}"#.to_string();
        profile.vlm_api_key = "test-untrusted-vlm-key".to_string();
        profile.vlm_base_url = "https://foreign.example/v1".to_string();
        profile.vlm_model = "foreign-vision".to_string();

        let canonical = canonicalize_managed_profile(&profile).unwrap();

        assert!(managed_profile_is_owned(&canonical));
        assert!(!canonical.config_contents.contains("untrusted"));
        assert!(!canonical.config_contents.contains("foreign.example"));
        assert!(
            !canonical
                .config_contents
                .contains("experimental_bearer_token")
        );
        assert!(canonical.auth_contents.is_empty());
        assert!(!canonical.upstream_base_url.contains("foreign.example"));
        assert!(canonical.model_metadata.is_empty());
        assert!(canonical.model_auto_compact.is_empty());
        assert!(canonical.model_vlm.is_empty());
        assert!(canonical.vlm_api_key.is_empty());
        assert!(canonical.vlm_base_url.is_empty());
        assert!(canonical.vlm_model.is_empty());
    }

    #[test]
    fn old_managed_profile_keeps_windows_without_enabling_new_overrides() {
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        let mut old = serde_json::to_value(&profile).unwrap();
        old.as_object_mut().unwrap().remove("modelAutoCompact");
        old.as_object_mut().unwrap().remove("modelMetadata");
        let loaded: RelayProfile = serde_json::from_value(old).unwrap();
        assert_eq!(loaded.model_windows, profile.model_windows);
        assert!(loaded.model_auto_compact.is_empty());
        assert!(loaded.model_metadata.is_empty());

        let temp = tempfile::tempdir().unwrap();
        crate::relay_config::apply_relay_profile_to_home_with_switch_rules(
            temp.path(),
            &loaded,
            "",
        )
        .unwrap();
        let catalog: Value =
            serde_json::from_slice(&std::fs::read(managed_catalog_path(temp.path())).unwrap())
                .unwrap();
        assert_eq!(
            catalog["models"][0]["context_window"].as_u64(),
            Some(LARGE_CONTEXT_WINDOW.parse::<u64>().unwrap())
        );
        assert!(catalog["models"][0]["auto_compact_token_limit"].is_null());
    }

    #[test]
    fn tampered_or_damaged_profile_is_never_ready_or_applied_on_launch() {
        for config_kind in ["tampered-endpoint", "damaged-toml"] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("codex-home");
            std::fs::create_dir_all(&home).unwrap();
            let original_config = "model = \"user-model\"\n";
            let original_auth = r#"{"OPENAI_API_KEY":"user-owned-key"}"#;
            std::fs::write(home.join("config.toml"), original_config).unwrap();
            std::fs::write(home.join("auth.json"), original_auth).unwrap();
            let store = SettingsStore::new(temp.path().join("settings.json"));
            let mut profile =
                build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
            profile.config_contents = if config_kind == "tampered-endpoint" {
                profile
                    .config_contents
                    .replace(distribution::FIXED_BASE_URL, "https://foreign.example/v1")
            } else {
                "model = [damaged toml".to_string()
            };
            store
                .save(&managed_settings(profile, UapiConnectionMode::Uapi))
                .unwrap();
            let vault = MemoryCredentialVault::default();
            vault
                .set(CredentialSlot::UapiApiKey, "test-uapi-vault-key-012345")
                .unwrap();

            let status = status_from_home_with_vault(&home, &store, &vault);
            assert!(!status.uapi_ready, "{config_kind}");
            assert!(!status.configured, "{config_kind}");
            assert!(
                apply_active_connection_profile_with(&store, &home, &vault).is_err(),
                "{config_kind}"
            );
            assert_eq!(
                std::fs::read_to_string(home.join("config.toml")).unwrap(),
                original_config,
                "{config_kind}"
            );
            assert_eq!(
                std::fs::read_to_string(home.join("auth.json")).unwrap(),
                original_auth,
                "{config_kind}"
            );
        }
    }

    #[test]
    fn shared_same_id_foreign_profile_is_not_migrated_or_removed() {
        let temp = tempfile::tempdir().unwrap();
        let isolated_path = temp.path().join("isolated").join("settings.json");
        let legacy_path = temp.path().join("shared").join("settings.json");
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let store = SettingsStore::new(isolated_path.clone());
        store.save(&BackendSettings::default()).unwrap();
        let mut foreign =
            build_managed_profile("foreign-model", &["foreign-model".to_string()]).unwrap();
        foreign.name = "User-owned same-name provider".to_string();
        foreign.config_contents = foreign
            .config_contents
            .replace(distribution::FIXED_BASE_URL, "https://foreign.example/v1");
        foreign.auth_contents = r#"{"OPENAI_API_KEY":"test-user-owned-foreign-key"}"#.to_string();
        let legacy_bytes = serde_json::to_vec_pretty(&json!({
            "upstreamOnly": true,
            "relayProfiles": [serde_json::to_value(foreign).unwrap()],
            "activeRelayId": distribution::FIXED_PROVIDER_ID,
        }))
        .unwrap();
        std::fs::write(&legacy_path, &legacy_bytes).unwrap();
        let vault = MemoryCredentialVault::default();

        migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
            .unwrap();
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_bytes);
        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_bytes);
    }

    #[test]
    fn upsert_preserves_unrelated_profiles() {
        let mut settings = BackendSettings::default();
        settings.relay_profiles.push(RelayProfile {
            id: "unrelated".to_string(),
            name: "Unrelated".to_string(),
            ..RelayProfile::default()
        });
        let managed =
            build_managed_profile("gpt-custom-codex", &["gpt-custom-codex".to_string()]).unwrap();
        upsert_managed_profile(&mut settings, managed);
        assert!(
            settings
                .relay_profiles
                .iter()
                .any(|item| item.id == "unrelated")
        );
        assert!(
            settings
                .relay_profiles
                .iter()
                .any(|item| item.id == distribution::FIXED_PROVIDER_ID)
        );
    }

    #[test]
    fn legacy_uapi_key_migrates_before_settings_are_sanitized() {
        let temp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let mut settings = BackendSettings::default();
        let mut profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        profile.auth_contents = r#"{"OPENAI_API_KEY":"sk-legacy-uapi-0123456789"}"#.to_string();
        settings.relay_profiles.push(profile);
        store.save(&settings).unwrap();
        let vault = MemoryCredentialVault::default();

        let migrated = migrate_legacy_managed_api_key(&store, &mut settings, &vault).unwrap();

        assert!(migrated);
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("sk-legacy-uapi-0123456789")
        );
        let persisted = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
        assert!(!persisted.contains("sk-legacy-uapi-0123456789"));
        assert!(
            settings
                .relay_profiles
                .iter()
                .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
                .unwrap()
                .auth_contents
                .is_empty()
        );
    }

    #[test]
    fn duplicate_isolated_profiles_with_the_same_key_are_all_sanitized() {
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let store = SettingsStore::new(settings_path.clone());
        let mut settings = BackendSettings::default();
        for model in ["gpt-5-codex", "gpt-5.6-sol"] {
            let mut profile = build_managed_profile(model, &[model.to_string()]).unwrap();
            profile.auth_contents =
                r#"{"OPENAI_API_KEY":"test-duplicate-legacy-key-012345"}"#.to_string();
            settings.relay_profiles.push(profile);
        }
        store.save(&settings).unwrap();
        let vault = MemoryCredentialVault::default();

        assert!(migrate_legacy_managed_api_key(&store, &mut settings, &vault).unwrap());

        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-duplicate-legacy-key-012345")
        );
        assert!(
            settings
                .relay_profiles
                .iter()
                .all(|profile| profile.auth_contents.is_empty())
        );
        assert!(
            !std::fs::read_to_string(settings_path)
                .unwrap()
                .contains("test-duplicate-legacy-key-012345")
        );
    }

    #[test]
    fn legacy_config_bearer_token_is_not_rehydrated_into_persisted_auth_contents() {
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let store = SettingsStore::new(settings_path.clone());
        let mut settings = BackendSettings::default();
        let mut profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        profile
            .config_contents
            .push_str("experimental_bearer_token = \"test-uapi-key-legacy-config\"\n");
        settings.relay_profiles.push(profile);
        std::fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&settings).unwrap(),
        )
        .unwrap();
        let vault = MemoryCredentialVault::default();

        let migrated = migrate_legacy_managed_api_key(&store, &mut settings, &vault).unwrap();

        assert!(migrated);
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-uapi-key-legacy-config")
        );
        let persisted = std::fs::read_to_string(settings_path).unwrap();
        assert!(!persisted.contains("test-uapi-key-legacy-config"));
        let managed = settings
            .relay_profiles
            .iter()
            .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
            .unwrap();
        assert!(managed.api_key.is_empty());
        assert!(managed.auth_contents.is_empty());
        assert!(
            !managed
                .config_contents
                .contains("experimental_bearer_token")
        );
    }

    fn assert_inactive_legacy_state_does_not_activate_after_migration(
        relay_profiles_enabled: bool,
        active_relay_id: &str,
        active_aggregate_relay_id: &str,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated").join("settings.json");
        let legacy_path = temp.path().join("shared").join("settings.json");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let store = SettingsStore::new(isolated_path.clone());
        let mut managed =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        let legacy_key = "test-inactive-legacy-key-012345";
        managed.auth_contents =
            serde_json::to_string(&json!({"OPENAI_API_KEY": legacy_key})).unwrap();
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&json!({
                "relayProfiles": [serde_json::to_value(managed).unwrap()],
                "relayProfilesEnabled": relay_profiles_enabled,
                "activeRelayId": active_relay_id,
                "activeAggregateRelayId": active_aggregate_relay_id,
            }))
            .unwrap(),
        )
        .unwrap();
        let config_before = b"model = \"foreign-model\"\nmodel_provider = \"foreign\"\n";
        let auth_before = b"{\"OPENAI_API_KEY\":\"foreign-live-key\"}\n";
        let catalog_before = b"{\"foreignCatalog\":true}\n";
        std::fs::write(home.join("config.toml"), config_before).unwrap();
        std::fs::write(home.join("auth.json"), auth_before).unwrap();
        let catalog_path = managed_catalog_path(&home);
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        std::fs::write(&catalog_path, catalog_before).unwrap();
        let vault = MemoryCredentialVault::default();

        migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
            .unwrap();

        let migrated = store.load().unwrap();
        assert!(!migrated.relay_profiles_enabled);
        assert!(migrated.active_relay_id.is_empty());
        assert!(migrated.active_aggregate_relay_id.is_empty());
        assert!(migrated.relay_profiles.iter().any(managed_profile_is_owned));
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some(legacy_key)
        );
        assert!(
            !std::fs::read_to_string(&isolated_path)
                .unwrap()
                .contains(legacy_key)
        );

        apply_active_connection_profile_with(&store, &home, &vault).unwrap();

        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(std::fs::read(catalog_path).unwrap(), catalog_before);
    }

    #[test]
    fn disabled_legacy_profile_migrates_without_becoming_active() {
        assert_inactive_legacy_state_does_not_activate_after_migration(
            false,
            distribution::FIXED_PROVIDER_ID,
            "",
        );
    }

    #[test]
    fn foreign_active_legacy_profile_migrates_without_becoming_active() {
        assert_inactive_legacy_state_does_not_activate_after_migration(true, "foreign-relay", "");
    }

    #[test]
    fn aggregated_legacy_profile_migrates_without_becoming_active() {
        assert_inactive_legacy_state_does_not_activate_after_migration(
            true,
            distribution::FIXED_PROVIDER_ID,
            "legacy-aggregate",
        );
    }

    #[test]
    fn shared_legacy_settings_migration_only_moves_owned_uapi_state() {
        let temp = tempfile::tempdir().unwrap();
        let isolated_path = temp.path().join("isolated").join("settings.json");
        let legacy_path = temp.path().join("shared").join("settings.json");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let store = SettingsStore::new(isolated_path.clone());
        let mut preexisting_isolated = BackendSettings::default();
        preexisting_isolated.codex_extra_args = vec!["--isolated-setting".to_string()];
        store.save(&preexisting_isolated).unwrap();
        let mut managed =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        managed
            .config_contents
            .push_str("experimental_bearer_token = \"test-uapi-key-shared-legacy\"\n");
        let unrelated = RelayProfile {
            id: "upstream-profile".to_string(),
            name: "Upstream profile".to_string(),
            ..RelayProfile::default()
        };
        let legacy = json!({
            "codexAppPath": "/Applications/Upstream Codex.app",
            "upstreamOnly": {"keep": true},
            "relayProfiles": [
                serde_json::to_value(unrelated).unwrap(),
                serde_json::to_value(managed).unwrap()
            ],
            "activeRelayId": distribution::FIXED_PROVIDER_ID,
        });
        std::fs::write(&legacy_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
        let vault = MemoryCredentialVault::default();

        migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
            .unwrap();

        let isolated = store.load().unwrap();
        assert_eq!(isolated.relay_profiles.len(), 1);
        assert_eq!(
            isolated.relay_profiles[0].id,
            distribution::FIXED_PROVIDER_ID
        );
        assert!(isolated.relay_profiles[0].auth_contents.is_empty());
        assert!(
            !isolated.relay_profiles[0]
                .config_contents
                .contains("experimental_bearer_token")
        );
        assert_ne!(isolated.codex_app_path, "/Applications/Upstream Codex.app");
        assert_eq!(isolated.codex_extra_args, ["--isolated-setting"]);
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-uapi-key-shared-legacy")
        );
        let legacy_after: Value =
            serde_json::from_str(&std::fs::read_to_string(legacy_path).unwrap()).unwrap();
        assert_eq!(legacy_after["upstreamOnly"]["keep"], true);
        assert_eq!(
            legacy_after["codexAppPath"],
            "/Applications/Upstream Codex.app"
        );
        assert_eq!(legacy_after["relayProfiles"].as_array().unwrap().len(), 1);
        assert_eq!(legacy_after["relayProfiles"][0]["id"], "upstream-profile");
        assert_eq!(legacy_after["activeRelayId"], "");
    }

    #[test]
    fn isolated_profile_migrates_legacy_key_before_shared_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let isolated_path = temp.path().join("isolated").join("settings.json");
        let legacy_path = temp.path().join("shared").join("settings.json");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let store = SettingsStore::new(isolated_path.clone());
        let isolated_profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(
                isolated_profile,
                UapiConnectionMode::Uapi,
            ))
            .unwrap();
        let mut legacy_profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        legacy_profile.auth_contents =
            r#"{"OPENAI_API_KEY":"test-shared-legacy-key-012345"}"#.to_string();
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&json!({
                "relayProfiles": [serde_json::to_value(legacy_profile).unwrap()],
                "activeRelayId": distribution::FIXED_PROVIDER_ID,
                "unrelated": "keep"
            }))
            .unwrap(),
        )
        .unwrap();
        let vault = MemoryCredentialVault::default();

        migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
            .unwrap();

        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-shared-legacy-key-012345")
        );
        let legacy_after: Value =
            serde_json::from_slice(&std::fs::read(&legacy_path).unwrap()).unwrap();
        assert!(legacy_after["relayProfiles"].as_array().unwrap().is_empty());
        assert_eq!(legacy_after["unrelated"], "keep");
    }

    #[test]
    fn distribution_migration_waits_for_the_cross_process_transaction_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated").join("settings.json");
        let legacy_path = temp.path().join("shared").join("settings.json");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let mut legacy_profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        legacy_profile.auth_contents =
            r#"{"OPENAI_API_KEY":"test-serialized-migration-key"}"#.to_string();
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&json!({
                "relayProfiles": [serde_json::to_value(legacy_profile).unwrap()],
                "activeRelayId": distribution::FIXED_PROVIDER_ID,
            }))
            .unwrap(),
        )
        .unwrap();

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let held_home = home.clone();
        let holder = std::thread::spawn(move || {
            crate::relay_config::with_live_files_transaction(&held_home, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        });
        entered_rx.recv().unwrap();

        let (finished_tx, finished_rx) = mpsc::channel();
        let worker_home = home.clone();
        let worker_isolated = isolated_path.clone();
        let worker_legacy = legacy_path.clone();
        let worker = std::thread::spawn(move || {
            let store = SettingsStore::new(worker_isolated.clone());
            let vault = MemoryCredentialVault::default();
            let result = prepare_distribution_state_with(
                &worker_home,
                &store,
                &worker_isolated,
                &worker_legacy,
                &vault,
            );
            finished_tx.send((result, vault)).unwrap();
        });

        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(!isolated_path.exists());
        assert!(legacy_path.exists());
        release_tx.send(()).unwrap();
        holder.join().unwrap().unwrap();
        let (result, vault) = finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        result.unwrap();
        worker.join().unwrap();

        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-serialized-migration-key")
        );
        assert!(isolated_path.exists());
        let legacy_after: Value =
            serde_json::from_slice(&std::fs::read(legacy_path).unwrap()).unwrap();
        assert!(legacy_after["relayProfiles"].as_array().unwrap().is_empty());
    }

    #[test]
    fn conflicting_vault_and_legacy_keys_preserve_shared_profile() {
        let temp = tempfile::tempdir().unwrap();
        let isolated_path = temp.path().join("isolated").join("settings.json");
        let legacy_path = temp.path().join("shared").join("settings.json");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let store = SettingsStore::new(isolated_path.clone());
        let isolated_profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(
                isolated_profile,
                UapiConnectionMode::Uapi,
            ))
            .unwrap();
        let isolated_before = std::fs::read(&isolated_path).unwrap();
        let mut legacy_profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        legacy_profile.auth_contents = r#"{"OPENAI_API_KEY":"test-shared-key-012345"}"#.to_string();
        let legacy_before = serde_json::to_vec_pretty(&json!({
            "relayProfiles": [serde_json::to_value(legacy_profile).unwrap()],
            "activeRelayId": distribution::FIXED_PROVIDER_ID,
        }))
        .unwrap();
        std::fs::write(&legacy_path, &legacy_before).unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-isolated-key-987654")
            .unwrap();

        let error =
            migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
                .unwrap_err();

        assert!(error.to_string().contains("不一致"));
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_before);
        assert_eq!(std::fs::read(&isolated_path).unwrap(), isolated_before);
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-isolated-key-987654")
        );
    }

    #[test]
    fn different_keys_in_duplicate_legacy_profiles_block_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let isolated_path = temp.path().join("isolated").join("settings.json");
        let legacy_path = temp.path().join("shared").join("settings.json");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let store = SettingsStore::new(isolated_path.clone());
        let mut first = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        first.auth_contents = r#"{"OPENAI_API_KEY":"test-first-key-012345"}"#.to_string();
        let mut second =
            build_managed_profile("gpt-5.6-sol", &["gpt-5.6-sol".to_string()]).unwrap();
        second.auth_contents = r#"{"OPENAI_API_KEY":"test-second-key-987654"}"#.to_string();
        let legacy_before = serde_json::to_vec_pretty(&json!({
            "relayProfiles": [
                serde_json::to_value(first).unwrap(),
                serde_json::to_value(second).unwrap()
            ],
            "activeRelayId": distribution::FIXED_PROVIDER_ID,
        }))
        .unwrap();
        std::fs::write(&legacy_path, &legacy_before).unwrap();
        let vault = MemoryCredentialVault::default();

        let error =
            migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
                .unwrap_err();

        assert!(error.to_string().contains("不同的 U-API 密钥"));
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_before);
        assert!(!isolated_path.exists());
        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());
    }

    #[test]
    fn typed_corruption_in_owned_legacy_profile_blocks_migration_without_hiding_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let isolated_path = temp.path().join("isolated").join("settings.json");
        let legacy_path = temp.path().join("shared").join("settings.json");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let store = SettingsStore::new(isolated_path.clone());
        let mut profile = serde_json::to_value(
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap(),
        )
        .unwrap();
        profile["authContents"] =
            Value::String(r#"{"OPENAI_API_KEY":"test-corrupt-profile-key-012345"}"#.into());
        profile["hideOfficialUsageAlert"] = json!({"invalid": true});
        let legacy_before = serde_json::to_vec_pretty(&json!({
            "relayProfiles": [profile],
            "activeRelayId": distribution::FIXED_PROVIDER_ID,
        }))
        .unwrap();
        std::fs::write(&legacy_path, &legacy_before).unwrap();
        let vault = MemoryCredentialVault::default();

        assert!(legacy_settings_has_owned_marker(&legacy_path).unwrap());
        let error =
            migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
                .unwrap_err();

        assert!(error.to_string().contains("无法迁移"));
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_before);
        assert!(!isolated_path.exists());
        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());
    }

    #[test]
    fn uninstall_removes_owned_legacy_secret_even_when_unrelated_field_type_is_corrupt() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let mut legacy_profile = serde_json::to_value(managed).unwrap();
        legacy_profile["authContents"] =
            Value::String(r#"{"OPENAI_API_KEY":"test-corrupt-profile-key-012345"}"#.into());
        legacy_profile["modelWindows"] = json!({"invalid": true});
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&json!({
                "relayProfiles": [legacy_profile],
                "activeRelayId": distribution::FIXED_PROVIDER_ID,
                "unrelated": "keep"
            }))
            .unwrap(),
        )
        .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-vault-key-012345")
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        let legacy_after: Value =
            serde_json::from_slice(&std::fs::read(&legacy_path).unwrap()).unwrap();
        assert!(legacy_after["relayProfiles"].as_array().unwrap().is_empty());
        assert_eq!(legacy_after["unrelated"], "keep");
        assert!(
            !std::fs::read_to_string(&legacy_path)
                .unwrap()
                .contains("test-corrupt-profile-key-012345")
        );
    }

    #[test]
    fn shared_upstream_official_mode_without_managed_profile_is_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let original = format!(
            "{{\n  \"upstreamOnly\": true,\n  \"activeRelayId\": \"{OFFICIAL_RELAY_ID}\",\n  \"relayProfiles\": [{{\"id\":\"upstream-profile\"}}]\n}}\n"
        );
        std::fs::write(&path, &original).unwrap();

        assert!(!remove_owned_settings_state(&path, false).unwrap());

        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn isolated_managed_settings_ignore_unrelated_damaged_legacy_file() {
        let temp = tempfile::tempdir().unwrap();
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed, UapiConnectionMode::Uapi))
            .unwrap();
        let damaged = b"{damaged upstream settings without an owned provider";
        std::fs::write(&legacy_path, damaged).unwrap();
        let vault = MemoryCredentialVault::default();

        migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
            .unwrap();

        assert!(
            store
                .load()
                .unwrap()
                .relay_profiles
                .iter()
                .any(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        );
        assert_eq!(std::fs::read(legacy_path).unwrap(), damaged);
    }

    #[test]
    fn unavailable_official_snapshot_does_not_block_uapi_settings_migration() {
        let temp = tempfile::tempdir().unwrap();
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&json!({
                "relayProfiles": [serde_json::to_value(managed).unwrap()],
                "activeRelayId": distribution::FIXED_PROVIDER_ID,
            }))
            .unwrap(),
        )
        .unwrap();
        let vault = MemoryCredentialVault::default();
        vault.fail_get(CredentialSlot::OfficialAuthJson);

        migrate_legacy_distribution_state_with(&store, &isolated_path, &legacy_path, &vault)
            .unwrap();

        assert!(
            store
                .load()
                .unwrap()
                .relay_profiles
                .iter()
                .any(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        );
        let legacy_after: Value =
            serde_json::from_str(&std::fs::read_to_string(legacy_path).unwrap()).unwrap();
        assert!(legacy_after["relayProfiles"].as_array().unwrap().is_empty());
    }

    #[test]
    fn failed_legacy_key_migration_keeps_plaintext_and_reports_truthful_status() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let mut settings = BackendSettings::default();
        let mut profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        profile.auth_contents = r#"{"OPENAI_API_KEY":"sk-legacy-uapi-0123456789"}"#.to_string();
        settings.relay_profiles.push(profile);
        store.save(&settings).unwrap();
        let vault = MemoryCredentialVault::default();
        vault.fail_set(CredentialSlot::UapiApiKey);

        let status = status_from_home_with_vault(&home, &store, &vault);

        assert!(!status.credential_store_available);
        assert!(status.credential_store_message.contains("原配置已保留"));
        assert_eq!(status.api_key_masked, "****6789");
        let persisted = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
        assert!(persisted.contains("sk-legacy-uapi-0123456789"));
    }

    #[test]
    fn failed_legacy_migration_does_not_block_active_uapi_launch_apply() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let mut profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        profile.api_key = "sk-legacy-uapi-0123456789".to_string();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let settings_path = temp.path().join("settings.json");
        let mut legacy_json =
            serde_json::from_str::<Value>(&std::fs::read_to_string(&settings_path).unwrap())
                .unwrap();
        legacy_json["relayProfiles"][0]["apiKey"] =
            Value::String("sk-legacy-uapi-0123456789".to_string());
        std::fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&legacy_json).unwrap(),
        )
        .unwrap();
        write_live_uapi(&home, &profile, "sk-legacy-uapi-0123456789");
        let vault = MemoryCredentialVault::default();
        vault.fail_set(CredentialSlot::UapiApiKey);

        let status = status_from_home_with_vault(&home, &store, &vault);
        assert!(status.configured);
        assert!(!status.credential_store_available);

        apply_active_connection_profile_with(&store, &home, &vault).unwrap();

        assert!(
            std::fs::read_to_string(settings_path)
                .unwrap()
                .contains("sk-legacy-uapi-0123456789")
        );
        assert!(
            std::fs::read_to_string(home.join("auth.json"))
                .unwrap()
                .contains("sk-legacy-uapi-0123456789")
        );
    }

    #[test]
    fn failed_legacy_migration_switches_to_uapi_without_rewriting_vault() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let mut profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        profile.auth_contents = r#"{"OPENAI_API_KEY":"sk-legacy-uapi-0123456789"}"#.to_string();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Official))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault.fail_set(CredentialSlot::UapiApiKey);

        let switched =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Uapi).unwrap();

        assert!(switched.configured);
        assert_eq!(switched.connection_mode, UapiConnectionMode::Uapi);
        assert!(
            std::fs::read_to_string(temp.path().join("settings.json"))
                .unwrap()
                .contains("sk-legacy-uapi-0123456789")
        );
    }

    #[test]
    fn failed_settings_cleanup_rolls_back_newly_migrated_credential() {
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let store = SettingsStore::new(settings_path.clone());
        let mut settings = BackendSettings::default();
        let mut profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        profile.auth_contents = r#"{"OPENAI_API_KEY":"sk-legacy-uapi-0123456789"}"#.to_string();
        settings.relay_profiles.push(profile);
        store.save(&settings).unwrap();
        let original_settings_path = temp.path().join("settings-before-failure.json");
        std::fs::rename(&settings_path, &original_settings_path).unwrap();
        // A directory at the destination makes the final atomic replace fail,
        // regardless of the unique temporary filename selected by atomic_write.
        std::fs::create_dir(&settings_path).unwrap();
        let vault = MemoryCredentialVault::default();

        assert!(migrate_legacy_managed_api_key(&store, &mut settings, &vault).is_err());

        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());
        assert!(
            std::fs::read_to_string(original_settings_path)
                .unwrap()
                .contains("sk-legacy-uapi-0123456789")
        );
        assert!(
            settings
                .relay_profiles
                .iter()
                .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
                .unwrap()
                .auth_contents
                .contains("sk-legacy-uapi-0123456789")
        );
    }

    #[test]
    fn live_uapi_key_is_last_resort_for_status_launch_and_refresh_apply() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        write_live_uapi(&home, &profile, "sk-live-uapi-0123456789");
        let vault = MemoryCredentialVault::default();
        vault.fail_get(CredentialSlot::UapiApiKey);

        let status = status_from_home_with_vault(&home, &store, &vault);
        assert!(status.configured);
        assert_eq!(status.api_key_masked, "****6789");
        apply_active_connection_profile_with(&store, &home, &vault).unwrap();

        let api_key = managed_api_key(&store.load().unwrap(), &vault, &home).unwrap();
        let refreshed = apply_discovery_with_options(
            &store,
            &home,
            &vault,
            &api_key,
            discovery(&["gpt-5.6-sol", "gpt-5-codex"]),
            false,
            false,
        )
        .unwrap();

        assert_eq!(refreshed.current_model, "gpt-5.6-sol");
        assert!(status_from_home_with_vault(&home, &store, &vault).configured);
        assert!(
            !std::fs::read_to_string(temp.path().join("settings.json"))
                .unwrap()
                .contains("sk-live-uapi-0123456789")
        );
    }

    #[test]
    fn older_configure_with_a_different_key_cannot_beat_the_latest_request() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        store.save(&BackendSettings::default()).unwrap();
        let vault = MemoryCredentialVault::default();
        let older = configure_request_guard(&store, &home);
        let newer = configure_request_guard(&store, &home);
        let prepare_called = std::cell::Cell::new(false);

        let error = apply_configured_discovery_with_guard_and_prepare(
            &store,
            &home,
            &vault,
            "test-configure-key-a-012345",
            &older,
            discovery(&["gpt-5-codex"]),
            || {
                prepare_called.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("更新的操作取代"));
        assert!(!prepare_called.get());
        apply_configured_discovery_with_guard_and_prepare(
            &store,
            &home,
            &vault,
            "test-configure-key-b-987654",
            &newer,
            discovery(&["gpt-5.6-sol", "gpt-5-codex"]),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-configure-key-b-987654")
        );
    }

    #[test]
    fn older_same_key_configure_response_cannot_overwrite_the_newer_response() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        store.save(&BackendSettings::default()).unwrap();
        let vault = MemoryCredentialVault::default();
        let older = configure_request_guard(&store, &home);
        let newer = configure_request_guard(&store, &home);

        apply_configured_discovery_with_guard_and_prepare(
            &store,
            &home,
            &vault,
            "test-configure-same-key-012345",
            &newer,
            discovery(&["gpt-5.6-sol", "gpt-5-codex"]),
            || Ok(()),
        )
        .unwrap();
        let state_after_newer = ModelRefreshStateSnapshot::capture(&store, &home).unwrap();

        let error = apply_configured_discovery_with_guard_and_prepare(
            &store,
            &home,
            &vault,
            "test-configure-same-key-012345",
            &older,
            discovery(&["gpt-5-codex"]),
            || Ok(()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("更新的操作取代"));
        assert_eq!(
            ModelRefreshStateSnapshot::capture(&store, &home).unwrap(),
            state_after_newer
        );
        assert!(
            managed_profile(&store.load().unwrap())
                .unwrap()
                .model_list
                .contains("gpt-5.6-sol")
        );
    }

    #[test]
    fn configure_saves_clean_fresh_tokens_from_mixed_live_auth_before_uapi_apply() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
        let configure_key = "test-configure-mixed-key-012345";
        std::fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": configure_key,
                "tokens": {
                    "access_token": "fresh-configure-live-token",
                    "refresh_token": "fresh-configure-live-refresh"
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        store.save(&BackendSettings::default()).unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(
                CredentialSlot::OfficialAuthJson,
                &official_auth_json_with_access_token("stale-configure-vault-token"),
            )
            .unwrap();
        let request_guard = configure_request_guard(&store, &home);

        apply_configured_discovery_with_guard_and_prepare(
            &store,
            &home,
            &vault,
            configure_key,
            &request_guard,
            discovery(&["gpt-5.6-sol"]),
            || Ok(()),
        )
        .unwrap();

        let stored: Value = serde_json::from_str(
            &vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(stored.get("OPENAI_API_KEY").is_none());
        assert_eq!(
            stored["tokens"]["access_token"],
            "fresh-configure-live-token"
        );
        let live: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).unwrap())
                .unwrap();
        assert_eq!(live["OPENAI_API_KEY"], configure_key);
    }

    #[test]
    fn configure_key_rotation_preserves_tokens_mixed_with_the_previous_owned_key() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let old_key = "test-configure-old-owned-012345";
        let new_key = "test-configure-new-owned-987654";
        write_live_uapi(&home, &profile, old_key);
        std::fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": old_key,
                "tokens": {
                    "access_token": "fresh-rotation-live-token",
                    "refresh_token": "fresh-rotation-live-refresh"
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let vault = MemoryCredentialVault::default();
        vault.set(CredentialSlot::UapiApiKey, old_key).unwrap();
        let request_guard = configure_request_guard(&store, &home);

        apply_configured_discovery_with_guard_and_prepare(
            &store,
            &home,
            &vault,
            new_key,
            &request_guard,
            discovery(&["gpt-5.6-sol"]),
            || Ok(()),
        )
        .unwrap();

        let stored_official: Value = serde_json::from_str(
            &vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(stored_official.get("OPENAI_API_KEY").is_none());
        assert_eq!(
            stored_official["tokens"]["access_token"],
            "fresh-rotation-live-token"
        );
        let live: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).unwrap())
                .unwrap();
        assert_eq!(live.as_object().unwrap().len(), 1);
        assert_eq!(live["OPENAI_API_KEY"], new_key);
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some(new_key)
        );
        let persisted_settings =
            std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
        assert!(!persisted_settings.contains(old_key));
        assert!(!persisted_settings.contains(new_key));
    }

    #[test]
    fn configure_response_is_discarded_when_mode_changes_while_waiting() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        write_live_uapi(&home, &profile, "test-configure-old-key-012345");
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-configure-old-key-012345")
            .unwrap();
        let request_guard = configure_request_guard(&store, &home);

        let mut switched = store.load().unwrap();
        switched.active_relay_id = OFFICIAL_RELAY_ID.to_string();
        store.save(&switched).unwrap();
        let settings_after_switch = std::fs::read(temp.path().join("settings.json")).unwrap();
        let config_after_switch = std::fs::read(home.join("config.toml")).unwrap();
        let auth_after_switch = std::fs::read(home.join("auth.json")).unwrap();
        let prepare_called = std::cell::Cell::new(false);

        let error = apply_configured_discovery_with_guard_and_prepare(
            &store,
            &home,
            &vault,
            "test-configure-new-key-987654",
            &request_guard,
            discovery(&["gpt-5.6-sol"]),
            || {
                prepare_called.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("本地连接状态已改变"));
        assert!(!prepare_called.get());
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            settings_after_switch
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_after_switch
        );
        assert_eq!(
            std::fs::read(home.join("auth.json")).unwrap(),
            auth_after_switch
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-configure-old-key-012345")
        );
    }

    #[test]
    fn stale_model_refresh_does_not_switch_back_from_official_mode() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-refresh-old-0123456789")
            .unwrap();
        write_live_uapi(&home, &profile, "sk-refresh-old-0123456789");
        let refresh_guard = model_refresh_guard(&store, &home, "sk-refresh-old-0123456789");

        let mut switched = store.load().unwrap();
        switched.active_relay_id = OFFICIAL_RELAY_ID.to_string();
        store.save(&switched).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();

        let error = apply_refreshed_discovery_with_guard(
            &store,
            &home,
            &vault,
            "sk-refresh-old-0123456789",
            &refresh_guard,
            discovery(&["gpt-5.6-sol"]),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("连接模式已改变"));
        assert_eq!(store.load().unwrap().active_relay_id, OFFICIAL_RELAY_ID);
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
    }

    #[test]
    fn stale_model_refresh_does_not_overwrite_a_newer_api_key() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-refresh-old-0123456789")
            .unwrap();
        write_live_uapi(&home, &profile, "sk-refresh-old-0123456789");
        let refresh_guard = model_refresh_guard(&store, &home, "sk-refresh-old-0123456789");
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();

        vault
            .set(CredentialSlot::UapiApiKey, "sk-refresh-new-9876543210")
            .unwrap();
        let error = apply_refreshed_discovery_with_guard(
            &store,
            &home,
            &vault,
            "sk-refresh-old-0123456789",
            &refresh_guard,
            discovery(&["gpt-5.6-sol"]),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("服务密钥已改变"));
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("sk-refresh-new-9876543210")
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
    }

    #[test]
    fn stale_model_refresh_rejects_a_foreign_active_relay_id() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-refresh-own-0123456789")
            .unwrap();
        write_live_uapi(&home, &profile, "sk-refresh-own-0123456789");
        let refresh_guard = model_refresh_guard(&store, &home, "sk-refresh-own-0123456789");

        let mut changed = store.load().unwrap();
        changed.active_relay_id = "foreign-relay".to_string();
        store.save(&changed).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();

        let error = apply_refreshed_discovery_with_guard(
            &store,
            &home,
            &vault,
            "sk-refresh-own-0123456789",
            &refresh_guard,
            discovery(&["gpt-5.6-sol"]),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("连接模式已改变"));
        assert_eq!(store.load().unwrap().active_relay_id, "foreign-relay");
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
    }

    #[test]
    fn stale_model_refresh_rejects_a_disabled_relay_profile() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-refresh-own-0123456789")
            .unwrap();
        write_live_uapi(&home, &profile, "sk-refresh-own-0123456789");
        let refresh_guard = model_refresh_guard(&store, &home, "sk-refresh-own-0123456789");

        let mut changed = store.load().unwrap();
        changed.relay_profiles_enabled = false;
        store.save(&changed).unwrap();

        let error = apply_refreshed_discovery_with_guard(
            &store,
            &home,
            &vault,
            "sk-refresh-own-0123456789",
            &refresh_guard,
            discovery(&["gpt-5.6-sol"]),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("连接模式已改变"));
        assert!(!store.load().unwrap().relay_profiles_enabled);
    }

    #[test]
    fn stale_model_refresh_preserves_foreign_live_takeover_byte_for_byte() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-refresh-own-0123456789")
            .unwrap();
        write_live_uapi(&home, &profile, "sk-refresh-own-0123456789");
        let refresh_guard = model_refresh_guard(&store, &home, "sk-refresh-own-0123456789");

        let foreign_config = b"model = \"foreign-model\"\nmodel_provider = \"foreign\"\n\n[model_providers.foreign]\nbase_url = \"https://foreign.example/v1\"\nwire_api = \"responses\"\n";
        let foreign_auth = b"{\"OPENAI_API_KEY\":\"sk-foreign-live-0123456789\",\"keep\":true}\n";
        std::fs::write(home.join("config.toml"), foreign_config).unwrap();
        std::fs::write(home.join("auth.json"), foreign_auth).unwrap();

        let error = apply_refreshed_discovery_with_guard(
            &store,
            &home,
            &vault,
            "sk-refresh-own-0123456789",
            &refresh_guard,
            discovery(&["gpt-5.6-sol"]),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("本地连接状态已改变"));
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            foreign_config
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), foreign_auth);
    }

    #[test]
    fn newer_same_key_model_refresh_supersedes_an_older_response() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-refresh-own-0123456789")
            .unwrap();
        write_live_uapi(&home, &profile, "sk-refresh-own-0123456789");

        let older = model_refresh_guard(&store, &home, "sk-refresh-own-0123456789");
        let newer = model_refresh_guard(&store, &home, "sk-refresh-own-0123456789");
        apply_refreshed_discovery_with_guard(
            &store,
            &home,
            &vault,
            "sk-refresh-own-0123456789",
            &newer,
            discovery(&["gpt-5.6-sol", "gpt-5-codex"]),
            false,
        )
        .unwrap();
        let state_after_newer = ModelRefreshStateSnapshot::capture(&store, &home).unwrap();

        let error = apply_refreshed_discovery_with_guard(
            &store,
            &home,
            &vault,
            "sk-refresh-own-0123456789",
            &older,
            discovery(&["gpt-5-codex"]),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("更新的操作取代"));
        assert_eq!(
            ModelRefreshStateSnapshot::capture(&store, &home).unwrap(),
            state_after_newer
        );
        assert!(
            managed_profile(&store.load().unwrap())
                .unwrap()
                .model_list
                .contains("gpt-5.6-sol")
        );
    }

    #[test]
    fn live_official_auth_wins_and_refreshes_stale_vault_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let vault = MemoryCredentialVault::default();
        let stale = official_auth_json_with_access_token("stale-access-token");
        let live = official_auth_json_with_access_token("fresh-access-token");
        vault.set(CredentialSlot::OfficialAuthJson, &stale).unwrap();
        std::fs::write(home.join("auth.json"), &live).unwrap();

        assert_eq!(
            official_auth_for_launch(&home, &vault).unwrap(),
            Some(live.clone())
        );
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(live)
        );
    }

    #[test]
    fn official_auth_with_null_api_key_is_sanitized_before_snapshotting() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let raw = serde_json::to_string_pretty(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "access_token": "fresh-null-compatible-token",
                "refresh_token": "fresh-null-compatible-refresh"
            }
        }))
        .unwrap();
        std::fs::write(home.join("auth.json"), raw).unwrap();
        let vault = MemoryCredentialVault::default();

        let sanitized = official_auth_for_launch(&home, &vault).unwrap().unwrap();
        let value: Value = serde_json::from_str(&sanitized).unwrap();

        assert!(value.get("OPENAI_API_KEY").is_none());
        assert_eq!(
            value["tokens"]["access_token"],
            "fresh-null-compatible-token"
        );
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(sanitized)
        );
    }

    #[test]
    fn uapi_launch_snapshots_fresh_official_auth_before_overwriting_live_auth() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
        let fresh = official_auth_json_with_access_token("fresh-before-uapi-launch");
        std::fs::write(home.join("auth.json"), &fresh).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-launch-uapi-key-012345")
            .unwrap();
        vault
            .set(
                CredentialSlot::OfficialAuthJson,
                &official_auth_json_with_access_token("stale-before-uapi-launch"),
            )
            .unwrap();

        apply_active_connection_profile_with(&store, &home, &vault).unwrap();

        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(fresh)
        );
        let live_auth: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).unwrap())
                .unwrap();
        assert_eq!(live_auth["OPENAI_API_KEY"], "test-launch-uapi-key-012345");
    }

    #[test]
    fn uapi_launch_preserves_fresh_official_auth_when_snapshot_write_fails() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
        let fresh = official_auth_json_with_access_token("fresh-preserved-on-vault-error");
        std::fs::write(home.join("auth.json"), &fresh).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-launch-uapi-key-012345")
            .unwrap();
        let stale = official_auth_json_with_access_token("stale-preserved-on-vault-error");
        vault.set(CredentialSlot::OfficialAuthJson, &stale).unwrap();
        vault.fail_set(CredentialSlot::OfficialAuthJson);
        let settings_before = std::fs::read(temp.path().join("settings.json")).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();

        let error = apply_active_connection_profile_with(&store, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("保存最新官方登录失败"));
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(stale)
        );
    }

    #[test]
    fn uapi_launch_rejects_foreign_key_only_auth_without_mutating_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let owned_key = "test-startup-owned-key-012345";
        write_live_uapi(&home, &profile, owned_key);
        let foreign_auth = b"{\"OPENAI_API_KEY\":\"test-startup-foreign-key-987654\"}\n";
        std::fs::write(home.join("auth.json"), foreign_auth).unwrap();
        let vault = MemoryCredentialVault::default();
        vault.set(CredentialSlot::UapiApiKey, owned_key).unwrap();
        let official = official_auth_json();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official)
            .unwrap();
        let settings_before = std::fs::read(temp.path().join("settings.json")).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();

        let error = apply_active_connection_profile_with(&store, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("拒绝自动覆盖"));
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), foreign_auth);
        assert_eq!(
            std::fs::read(managed_catalog_path(&home)).unwrap(),
            catalog_before
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some(owned_key)
        );
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(official)
        );
    }

    #[test]
    fn uapi_launch_rejects_damaged_or_unknown_auth_without_mutating_state() {
        for auth_bytes in [
            b"{not-valid-json\n".as_slice(),
            b"{\"unknown\":true}\n".as_slice(),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("codex-home");
            let store = SettingsStore::new(temp.path().join("settings.json"));
            let profile =
                build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
            store
                .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
                .unwrap();
            let owned_key = "test-startup-owned-key-012345";
            write_live_uapi(&home, &profile, owned_key);
            std::fs::write(home.join("auth.json"), auth_bytes).unwrap();
            let vault = MemoryCredentialVault::default();
            vault.set(CredentialSlot::UapiApiKey, owned_key).unwrap();
            let official = official_auth_json();
            vault
                .set(CredentialSlot::OfficialAuthJson, &official)
                .unwrap();
            let settings_before = std::fs::read(temp.path().join("settings.json")).unwrap();
            let config_before = std::fs::read(home.join("config.toml")).unwrap();
            let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();

            assert!(apply_active_connection_profile_with(&store, &home, &vault).is_err());

            assert_eq!(
                std::fs::read(temp.path().join("settings.json")).unwrap(),
                settings_before
            );
            assert_eq!(
                std::fs::read(home.join("config.toml")).unwrap(),
                config_before
            );
            assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_bytes);
            assert_eq!(
                std::fs::read(managed_catalog_path(&home)).unwrap(),
                catalog_before
            );
            assert_eq!(
                vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
                Some(owned_key)
            );
            assert_eq!(
                vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
                Some(official)
            );
        }
    }

    #[test]
    fn launch_rejects_unknown_active_relay_without_mutating_any_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        let mut settings = managed_settings(profile.clone(), UapiConnectionMode::Uapi);
        settings.active_relay_id = "foreign-active-relay".to_string();
        store.save(&settings).unwrap();
        write_live_uapi(&home, &profile, "test-launch-owned-key-012345");
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-launch-owned-key-012345")
            .unwrap();
        let official = official_auth_json();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official)
            .unwrap();
        let settings_before = std::fs::read(temp.path().join("settings.json")).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();
        let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();

        let error = apply_active_connection_profile_with(&store, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("不属于 U-API Connect"));
        assert!(!status_from_home_with_vault(&home, &store, &vault).active);
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(
            std::fs::read(managed_catalog_path(&home)).unwrap(),
            catalog_before
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-launch-owned-key-012345")
        );
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(official)
        );
    }

    #[test]
    fn launch_rejects_active_aggregate_without_mutating_any_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        let mut settings = managed_settings(profile.clone(), UapiConnectionMode::Uapi);
        settings.active_aggregate_relay_id = "foreign-aggregate".to_string();
        store.save(&settings).unwrap();
        write_live_uapi(&home, &profile, "test-launch-owned-key-012345");
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-launch-owned-key-012345")
            .unwrap();
        let official = official_auth_json();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official)
            .unwrap();
        let settings_before = std::fs::read(temp.path().join("settings.json")).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();
        let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();

        let error = apply_active_connection_profile_with(&store, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("聚合中转"));
        assert!(!status_from_home_with_vault(&home, &store, &vault).active);
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(
            std::fs::read(managed_catalog_path(&home)).unwrap(),
            catalog_before
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-launch-owned-key-012345")
        );
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(official)
        );
    }

    #[test]
    fn disabled_launch_ignores_stale_aggregate_without_mutating_any_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        let mut settings = managed_settings(profile.clone(), UapiConnectionMode::Uapi);
        settings.relay_profiles_enabled = false;
        settings.active_aggregate_relay_id = "stale-disabled-aggregate".to_string();
        store.save(&settings).unwrap();
        write_live_uapi(&home, &profile, "test-disabled-owned-key-012345");
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-disabled-owned-key-012345")
            .unwrap();
        let official = official_auth_json();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official)
            .unwrap();
        let settings_before = std::fs::read(temp.path().join("settings.json")).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();
        let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();

        apply_active_connection_profile_with(&store, &home, &vault).unwrap();

        assert!(!status_from_home_with_vault(&home, &store, &vault).active);
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(
            std::fs::read(managed_catalog_path(&home)).unwrap(),
            catalog_before
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-disabled-owned-key-012345")
        );
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(official)
        );
    }

    #[test]
    fn official_launch_without_live_auth_ignores_unavailable_saved_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(
                profile.clone(),
                UapiConnectionMode::Official,
            ))
            .unwrap();
        write_live_uapi(&home, &profile, "sk-live-uapi-0123456789");
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-live-uapi-0123456789")
            .unwrap();
        vault.fail_get(CredentialSlot::OfficialAuthJson);

        apply_active_connection_profile_with(&store, &home, &vault).unwrap();

        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(crate::relay_config::root_key_string(&config, "model_provider").is_none());
        assert!(
            !std::fs::read_to_string(home.join("auth.json"))
                .unwrap()
                .contains("OPENAI_API_KEY")
        );
    }

    #[test]
    fn official_launch_rejects_foreign_live_config_without_mutating_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let foreign_config = b"# foreign owner\nmodel = \"foreign-model\"\nmodel_provider = \"foreign\"\nmodel_catalog_json = \"foreign-catalog.json\"\n\n[model_providers.foreign]\nbase_url = \"https://foreign.example/v1\"\nwire_api = \"responses\"\n";
        let foreign_auth =
            b"{\n  \"OPENAI_API_KEY\": \"sk-foreign-live-0123456789\",\n  \"foreign\": true\n}\n";
        std::fs::write(home.join("config.toml"), foreign_config).unwrap();
        std::fs::write(home.join("auth.json"), foreign_auth).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Official))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-uapi-owned-0123456789")
            .unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth_json())
            .unwrap();

        let error = apply_active_connection_profile_with(&store, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("其他供应商"));
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            foreign_config
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), foreign_auth);
    }

    #[test]
    fn official_launch_restores_saved_login_into_an_empty_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Official))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        let official_auth = official_auth_json();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth)
            .unwrap();

        apply_active_connection_profile_with(&store, &home, &vault).unwrap();

        assert!(!home.join("config.toml").exists());
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            official_auth
        );
    }

    #[test]
    fn official_launch_keeps_valid_live_auth_when_vault_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let live = official_auth_json_with_access_token("fresh-live-access-token");
        std::fs::write(home.join("auth.json"), &live).unwrap();
        std::fs::write(home.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        store
            .save(&managed_settings(
                build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap(),
                UapiConnectionMode::Official,
            ))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault.fail_get(CredentialSlot::OfficialAuthJson);
        vault.fail_set(CredentialSlot::OfficialAuthJson);

        apply_active_connection_profile_with(&store, &home, &vault).unwrap();

        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            live
        );
    }

    #[test]
    fn switching_to_official_without_live_auth_ignores_unavailable_snapshot_and_keeps_legacy_key() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let mut profile =
            build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        profile.api_key = "sk-legacy-uapi-0123456789".to_string();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let settings_path = temp.path().join("settings.json");
        let mut legacy_json =
            serde_json::from_str::<Value>(&std::fs::read_to_string(&settings_path).unwrap())
                .unwrap();
        legacy_json["relayProfiles"][0]["apiKey"] =
            Value::String("sk-legacy-uapi-0123456789".to_string());
        std::fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&legacy_json).unwrap(),
        )
        .unwrap();
        write_live_uapi(&home, &profile, "sk-legacy-uapi-0123456789");
        let vault = MemoryCredentialVault::default();
        vault.fail_set(CredentialSlot::UapiApiKey);
        vault.fail_get(CredentialSlot::OfficialAuthJson);

        let switched =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap();

        assert_eq!(switched.connection_mode, UapiConnectionMode::Official);
        assert!(!switched.official_authenticated);
        assert!(
            std::fs::read_to_string(settings_path)
                .unwrap()
                .contains("sk-legacy-uapi-0123456789")
        );
    }

    #[test]
    fn switching_to_official_uses_valid_live_auth_when_saved_snapshot_read_fails() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let live = official_auth_json_with_access_token("fresh-live-access-token");
        std::fs::write(home.join("auth.json"), &live).unwrap();
        std::fs::write(home.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        store.save(&BackendSettings::default()).unwrap();
        let vault = MemoryCredentialVault::default();
        vault.fail_get(CredentialSlot::OfficialAuthJson);

        let switched =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap();

        assert_eq!(switched.connection_mode, UapiConnectionMode::Official);
        assert!(switched.official_authenticated);
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            live
        );
        assert_eq!(store.load().unwrap().active_relay_id, OFFICIAL_RELAY_ID);
    }

    #[test]
    fn switching_to_official_rejects_foreign_live_files_and_rolls_back_mode() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let foreign_config = b"# managed elsewhere\nmodel = \"foreign-model\"\nmodel_provider = \"foreign\"\n\n[model_providers.foreign]\nbase_url = \"https://foreign.example/v1\"\nwire_api = \"responses\"\n";
        let foreign_auth = b"{\"OPENAI_API_KEY\":\"sk-foreign-live-0123456789\",\"keep\":1}\n";
        std::fs::write(home.join("config.toml"), foreign_config).unwrap();
        std::fs::write(home.join("auth.json"), foreign_auth).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-uapi-owned-0123456789")
            .unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth_json())
            .unwrap();

        let error =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap_err();

        assert!(error.to_string().contains("其他供应商"));
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            foreign_config
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), foreign_auth);
        assert_eq!(
            store.load().unwrap().active_relay_id,
            distribution::FIXED_PROVIDER_ID
        );
    }

    #[test]
    fn switching_to_official_strips_owned_key_and_keeps_newest_live_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let owned_key = "test-owned-mixed-key-012345";
        write_live_uapi(&home, &profile, owned_key);
        let mixed = serde_json::to_string_pretty(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": owned_key,
            "tokens": {
                "access_token": "fresh-mixed-live-token",
                "refresh_token": "fresh-mixed-live-refresh"
            },
            "keep": true
        }))
        .unwrap();
        std::fs::write(home.join("auth.json"), mixed).unwrap();
        let vault = MemoryCredentialVault::default();
        vault.set(CredentialSlot::UapiApiKey, owned_key).unwrap();
        vault
            .set(
                CredentialSlot::OfficialAuthJson,
                &official_auth_json_with_access_token("stale-vault-token"),
            )
            .unwrap();

        switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official).unwrap();

        let live: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).unwrap())
                .unwrap();
        let stored: Value = serde_json::from_str(
            &vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(live.get("OPENAI_API_KEY").is_none());
        assert_eq!(live["tokens"]["access_token"], "fresh-mixed-live-token");
        assert_eq!(live["keep"], true);
        assert_eq!(stored, live);
        assert_eq!(store.load().unwrap().active_relay_id, OFFICIAL_RELAY_ID);
    }

    #[test]
    fn switching_to_official_recovers_tokens_from_a_legacy_polluted_vault_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let owned_key = "test-upgrade-owned-key-012345";
        write_live_uapi(&home, &profile, owned_key);
        let vault = MemoryCredentialVault::default();
        vault.set(CredentialSlot::UapiApiKey, owned_key).unwrap();
        vault
            .set(
                CredentialSlot::OfficialAuthJson,
                &serde_json::to_string_pretty(&json!({
                    "auth_mode": "chatgpt",
                    "OPENAI_API_KEY": "legacy-polluted-key-must-not-return",
                    "tokens": {
                        "access_token": "legacy-snapshot-access-token",
                        "refresh_token": "legacy-snapshot-refresh-token"
                    }
                }))
                .unwrap(),
            )
            .unwrap();

        switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official).unwrap();

        let live: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).unwrap())
                .unwrap();
        let stored: Value = serde_json::from_str(
            &vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(live.get("OPENAI_API_KEY").is_none());
        assert!(stored.get("OPENAI_API_KEY").is_none());
        assert_eq!(
            live["tokens"]["access_token"],
            "legacy-snapshot-access-token"
        );
        assert_eq!(stored, live);
    }

    #[test]
    fn switching_to_official_rejects_foreign_key_mixed_with_official_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let owned_key = "test-owned-mixed-key-012345";
        write_live_uapi(&home, &profile, owned_key);
        let mixed_foreign = serde_json::to_vec_pretty(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": "test-foreign-mixed-key-987654",
            "tokens": {
                "access_token": "foreign-mixed-live-token",
                "refresh_token": "foreign-mixed-live-refresh"
            }
        }))
        .unwrap();
        std::fs::write(home.join("auth.json"), &mixed_foreign).unwrap();
        let vault = MemoryCredentialVault::default();
        vault.set(CredentialSlot::UapiApiKey, owned_key).unwrap();
        let saved = official_auth_json_with_access_token("safe-vault-token");
        vault.set(CredentialSlot::OfficialAuthJson, &saved).unwrap();
        let settings_before = std::fs::read(temp.path().join("settings.json")).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();

        let error =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap_err();

        assert!(error.to_string().contains("无法确认归属"));
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(
            std::fs::read(home.join("auth.json")).unwrap(),
            mixed_foreign
        );
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(saved)
        );
    }

    #[test]
    fn switching_to_official_rejects_foreign_auth_and_rolls_back_live_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-uapi-owned-0123456789")
            .unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth_json())
            .unwrap();
        write_live_uapi(&home, &profile, "sk-uapi-owned-0123456789");
        let foreign_auth =
            b"{\n  \"OPENAI_API_KEY\": \"sk-foreign-new-9876543210\",\n  \"keep\": true\n}\n";
        std::fs::write(home.join("auth.json"), foreign_auth).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();

        let error =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap_err();

        assert!(error.to_string().contains("无法确认归属"));
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), foreign_auth);
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(
            store.load().unwrap().active_relay_id,
            distribution::FIXED_PROVIDER_ID
        );
    }

    #[test]
    fn switching_to_official_rejects_damaged_config_and_rolls_back_mode() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let damaged_config = b"model_provider = [not valid toml\n";
        let auth = b"{}\n";
        std::fs::write(home.join("config.toml"), damaged_config).unwrap();
        std::fs::write(home.join("auth.json"), auth).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth_json())
            .unwrap();

        let error =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap_err();

        assert!(error.to_string().contains("config.toml 已损坏"));
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            damaged_config
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth);
        assert_eq!(
            store.load().unwrap().active_relay_id,
            distribution::FIXED_PROVIDER_ID
        );
    }

    #[test]
    fn switching_to_official_rejects_an_unowned_openai_transport_override() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let foreign_config = b"model = \"gpt-5\"\nmodel_provider = \"openai\"\nopenai_base_url = \"https://foreign.example/v1\"\n";
        std::fs::write(home.join("config.toml"), foreign_config).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth_json())
            .unwrap();

        let error =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap_err();

        assert!(error.to_string().contains("官方传输覆盖"));
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            foreign_config
        );
        assert!(!home.join("auth.json").exists());
        assert_eq!(
            store.load().unwrap().active_relay_id,
            distribution::FIXED_PROVIDER_ID
        );
    }

    #[test]
    fn switching_to_official_fails_closed_when_uapi_key_ownership_is_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-uapi-owned-0123456789")
            .unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth_json())
            .unwrap();
        write_live_uapi(&home, &profile, "sk-uapi-owned-0123456789");
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();
        vault.fail_get(CredentialSlot::UapiApiKey);

        let error =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap_err();

        assert!(error.to_string().contains("无法确认归属"));
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(
            store.load().unwrap().active_relay_id,
            distribution::FIXED_PROVIDER_ID
        );
    }

    #[test]
    fn switching_to_official_restores_saved_login_without_persisting_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let vault = MemoryCredentialVault::default();
        let official_auth = official_auth_json();
        std::fs::write(home.join("auth.json"), &official_auth).unwrap();
        std::fs::write(home.join("config.toml"), "model = \"gpt-5\"\n").unwrap();

        apply_discovery_with(
            &store,
            &home,
            &vault,
            "sk-uapi-0123456789",
            discovery(&["gpt-5-codex"]),
        )
        .unwrap();

        let settings_json = serde_json::to_string(&store.load().unwrap()).unwrap();
        assert!(!settings_json.contains("sk-uapi-0123456789"));
        assert!(!settings_json.contains("official-access-token-for-test"));
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(official_auth.clone())
        );

        let switched =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap();
        assert_eq!(switched.connection_mode, UapiConnectionMode::Official);
        assert!(switched.official_authenticated);
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            official_auth
        );

        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(crate::relay_config::root_key_string(&config, "model").is_none());
        assert!(crate::relay_config::root_key_string(&config, "model_provider").is_none());
        assert!(crate::relay_config::root_key_string(&config, "model_catalog_json").is_none());
        assert!(!config.contains("OPENAI_API_KEY"));
        assert_eq!(store.load().unwrap().active_relay_id, OFFICIAL_RELAY_ID);
    }

    #[test]
    fn switching_to_official_without_snapshot_removes_uapi_key_and_requires_login() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let vault = MemoryCredentialVault::default();

        apply_discovery_with(
            &store,
            &home,
            &vault,
            "sk-uapi-0123456789",
            discovery(&["gpt-5-codex"]),
        )
        .unwrap();
        let switched =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Official)
                .unwrap();

        assert!(!switched.official_authenticated);
        let auth = std::fs::read_to_string(home.join("auth.json")).unwrap();
        assert!(!auth.contains("OPENAI_API_KEY"));
        assert!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn switching_back_without_uapi_configuration_keeps_official_files_and_opens_setup() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let config = "model = \"gpt-5\"\n";
        let official_auth = official_auth_json();
        std::fs::write(home.join("config.toml"), config).unwrap();
        std::fs::write(home.join("auth.json"), &official_auth).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let mut settings = BackendSettings::default();
        settings.relay_profiles_enabled = true;
        settings.active_relay_id = OFFICIAL_RELAY_ID.to_string();
        store.save(&settings).unwrap();
        let vault = MemoryCredentialVault::default();

        let switched =
            switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Uapi).unwrap();

        assert_eq!(switched.connection_mode, UapiConnectionMode::Uapi);
        assert!(!switched.configured);
        assert_eq!(
            store.load().unwrap().active_relay_id,
            distribution::FIXED_PROVIDER_ID
        );
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            config
        );
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            official_auth
        );
    }

    #[test]
    fn failed_connection_change_restores_files_settings_and_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), "model = \"before\"\n").unwrap();
        std::fs::write(home.join("auth.json"), "{\"before\":true}").unwrap();
        let catalog_path = managed_catalog_path(&home);
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        std::fs::write(&catalog_path, "{\"catalog\":\"before\"}").unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let original_settings = BackendSettings::default();
        store.save(&original_settings).unwrap();
        let mut next_settings = original_settings.clone();
        next_settings.active_relay_id = distribution::FIXED_PROVIDER_ID.to_string();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-before-0123456789")
            .unwrap();

        let error = commit_connection_change(
            &store,
            &home,
            &vault,
            &next_settings,
            Some("sk-after-0123456789"),
            None,
            || {
                std::fs::write(&catalog_path, "{\"catalog\":\"after\"}").unwrap();
                anyhow::bail!("simulated apply failure")
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("simulated apply failure"));
        assert_eq!(store.load().unwrap(), original_settings);
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            "model = \"before\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            "{\"before\":true}"
        );
        assert_eq!(
            std::fs::read_to_string(catalog_path).unwrap(),
            "{\"catalog\":\"before\"}"
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("sk-before-0123456789")
        );
    }

    #[test]
    fn external_catalog_conflict_preserves_uapi_files_settings_and_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let config = "# user-owned\nmodel = \"before\"\nmodel_catalog_json = \"external.json\"\n";
        let auth = official_auth_json();
        std::fs::write(home.join("config.toml"), config).unwrap();
        std::fs::write(home.join("auth.json"), &auth).unwrap();
        let external_catalog = br#"{"models":[{"slug":"before","custom":true}]}"#;
        std::fs::write(home.join("external.json"), external_catalog).unwrap();
        let catalog_path = managed_catalog_path(&home);
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        let old_catalog = br#"{"models":[{"slug":"cached"}]}"#;
        std::fs::write(&catalog_path, old_catalog).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(profile, UapiConnectionMode::Official))
            .unwrap();
        let settings_before = std::fs::read(temp.path().join("settings.json")).unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-uapi-old-key")
            .unwrap();
        vault.set(CredentialSlot::OfficialAuthJson, &auth).unwrap();

        let error = switch_connection_mode_with(&store, &home, &vault, UapiConnectionMode::Uapi)
            .unwrap_err();

        assert!(
            error.to_string().contains("外部 model_catalog_json"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config.as_bytes()
        );
        assert_eq!(
            std::fs::read(home.join("auth.json")).unwrap(),
            auth.as_bytes()
        );
        assert_eq!(
            std::fs::read(home.join("external.json")).unwrap(),
            external_catalog
        );
        assert_eq!(std::fs::read(catalog_path).unwrap(), old_catalog);
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-uapi-old-key")
        );
        assert_eq!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some(auth.as_str())
        );
    }

    #[test]
    fn failed_connection_change_restores_settings_bytes_with_unknown_fields_and_legacy_key() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let settings_path = temp.path().join("settings.json");
        let store = SettingsStore::new(settings_path.clone());
        let profile = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        let mut raw =
            serde_json::to_value(managed_settings(profile, UapiConnectionMode::Uapi)).unwrap();
        raw["futureTopLevelField"] = json!({"preserve": [1, 2, 3]});
        raw["relayProfiles"][0]["apiKey"] = Value::String("test-legacy-raw-key-012345".into());
        let mut original_bytes = serde_json::to_vec_pretty(&raw).unwrap();
        original_bytes.push(b'\n');
        std::fs::write(&settings_path, &original_bytes).unwrap();
        let mut next_settings = store.load().unwrap();
        next_settings.active_relay_id = OFFICIAL_RELAY_ID.to_string();
        let vault = MemoryCredentialVault::default();

        let error =
            commit_connection_change(&store, &home, &vault, &next_settings, None, None, || {
                anyhow::bail!("simulated raw settings rollback")
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("simulated raw settings rollback")
        );
        assert_eq!(std::fs::read(settings_path).unwrap(), original_bytes);
    }

    #[test]
    fn failed_connection_change_removes_settings_file_created_by_transaction() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let settings_path = temp.path().join("settings.json");
        let store = SettingsStore::new(settings_path.clone());
        let vault = MemoryCredentialVault::default();

        let error = commit_connection_change(
            &store,
            &home,
            &vault,
            &BackendSettings::default(),
            None,
            None,
            || anyhow::bail!("simulated first settings write failure"),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("simulated first settings write failure")
        );
        assert!(!settings_path.exists());
    }

    #[test]
    fn live_snapshot_restore_attempts_config_auth_and_catalog_after_failures() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        std::fs::create_dir(home.join("config.toml")).unwrap();
        std::fs::create_dir(home.join("auth.json")).unwrap();
        let catalog_path = managed_catalog_path(home);
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        std::fs::create_dir(&catalog_path).unwrap();
        std::fs::create_dir(refresh_request_marker_path(home)).unwrap();
        let snapshot = UapiLiveFilesSnapshot {
            config: Some(b"old config".to_vec()),
            auth: Some(b"old auth".to_vec()),
            managed_catalog: Some(b"old catalog".to_vec()),
            refresh_request_marker: Some(b"old refresh marker".to_vec()),
        };

        let error = snapshot.restore(home).unwrap_err().to_string();
        assert!(error.contains("config.toml="));
        assert!(error.contains("auth.json="));
        assert!(error.contains("model catalog="));
        assert!(error.contains("refresh marker="));
        assert!(!error.contains("config.toml=ok"));
        assert!(!error.contains("auth.json=ok"));
        assert!(!error.contains("model catalog=ok"));
        assert!(!error.contains("refresh marker=ok"));
    }

    #[test]
    fn credential_snapshot_restore_attempts_both_slots_after_failures() {
        let vault = MemoryCredentialVault::default();
        vault.fail_set(CredentialSlot::UapiApiKey);
        vault.fail_set(CredentialSlot::OfficialAuthJson);
        let snapshot = CredentialSnapshot {
            uapi_api_key: CapturedCredential::Present("sk-old-uapi".to_string()),
            official_auth_json: CapturedCredential::Present("old official auth".to_string()),
        };

        let error = snapshot.restore(&vault).unwrap_err().to_string();
        assert!(error.contains("U-API Key="));
        assert!(error.contains("官方登录快照="));
        assert!(!error.contains("U-API Key=ok"));
        assert!(!error.contains("官方登录快照=ok"));
    }

    #[test]
    fn failed_connection_change_removes_new_managed_catalog_when_none_existed() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        let settings = BackendSettings::default();
        store.save(&settings).unwrap();
        let vault = MemoryCredentialVault::default();
        let catalog_path = managed_catalog_path(&home);

        let error = commit_connection_change(&store, &home, &vault, &settings, None, None, || {
            std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
            std::fs::write(&catalog_path, "{\"catalog\":\"new\"}").unwrap();
            anyhow::bail!("simulated catalog apply failure")
        })
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("simulated catalog apply failure")
        );
        assert!(!catalog_path.exists());
    }

    #[test]
    fn uninstall_cleanup_restores_official_auth_and_removes_only_owned_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        let unrelated = RelayProfile {
            id: "user-provider".to_string(),
            name: "User provider".to_string(),
            ..RelayProfile::default()
        };
        let mut settings = managed_settings(managed.clone(), UapiConnectionMode::Uapi);
        settings.relay_profiles.push(unrelated.clone());
        store.save(&settings).unwrap();
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&json!({
                "upstreamOnly": "preserve-me",
                "relayProfiles": [
                    serde_json::to_value(unrelated).unwrap(),
                    serde_json::to_value(managed.clone()).unwrap()
                ],
                "activeRelayId": distribution::FIXED_PROVIDER_ID,
            }))
            .unwrap(),
        )
        .unwrap();
        write_live_uapi(&home, &managed, "sk-live-uapi-0123456789");
        let mut live_config = std::fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        live_config["approval_policy"] = toml_edit::value("never");
        live_config["model_providers"]["user_custom"]["base_url"] =
            toml_edit::value("https://user.example/v1");
        std::fs::write(home.join("config.toml"), live_config.to_string()).unwrap();
        std::fs::write(
            refresh_request_marker_path(&home),
            "refresh-before-uninstall",
        )
        .unwrap();
        let vault = MemoryCredentialVault::default();
        let official_auth = official_auth_json();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-live-uapi-0123456789")
            .unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth)
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains("approval_policy = \"never\""));
        assert!(config.contains("https://user.example/v1"));
        assert!(!config.contains(distribution::FIXED_PROVIDER_ID));
        assert!(!config.contains("model_catalog_json"));
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            official_auth
        );
        assert!(!managed_catalog_path(&home).exists());
        assert!(!refresh_request_marker_path(&home).exists());
        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());
        assert!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .is_none()
        );
        let isolated_after: Value =
            serde_json::from_str(&std::fs::read_to_string(&isolated_path).unwrap()).unwrap();
        assert_eq!(
            isolated_after["relayProfiles"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|profile| profile["id"] == distribution::FIXED_PROVIDER_ID)
                .count(),
            0
        );
        assert!(
            isolated_after["relayProfiles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|profile| profile["id"] == "user-provider")
        );
        let legacy_after: Value =
            serde_json::from_str(&std::fs::read_to_string(legacy_path).unwrap()).unwrap();
        assert_eq!(legacy_after["upstreamOnly"], "preserve-me");
        assert_eq!(legacy_after["relayProfiles"].as_array().unwrap().len(), 1);
        assert_eq!(legacy_after["relayProfiles"][0]["id"], "user-provider");
    }

    #[test]
    fn uninstall_cleanup_uses_the_captured_credentials_without_reading_twice() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        write_live_uapi(&home, &managed, "sk-live-uapi-0123456789");
        let vault = SingleReadCredentialVault::default();
        let official_auth = official_auth_json();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-live-uapi-0123456789")
            .unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth)
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            official_auth
        );
        assert_eq!(vault.read_count(CredentialSlot::UapiApiKey), 1);
        assert_eq!(vault.read_count(CredentialSlot::OfficialAuthJson), 1);
        assert!(vault.peek(CredentialSlot::UapiApiKey).is_none());
        assert!(vault.peek(CredentialSlot::OfficialAuthJson).is_none());
    }

    #[test]
    fn uninstall_cleanup_preserves_foreign_live_key_without_proven_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        write_live_uapi(&home, &managed, "user-edited-foreign-key-012345");
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-owned-not-live-key-012345")
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(!config.contains(distribution::FIXED_PROVIDER_ID));
        assert!(!config.contains("model_catalog_json"));
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(isolated_path).unwrap()).unwrap();
        assert!(
            settings["relayProfiles"]
                .as_array()
                .unwrap()
                .iter()
                .all(|profile| profile["id"] != distribution::FIXED_PROVIDER_ID)
        );
        assert!(!managed_catalog_path(&home).exists());
        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());
        assert!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn uninstall_cleanup_rejects_foreign_key_mixed_with_official_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let owned_key = "test-uninstall-owned-key-012345";
        write_live_uapi(&home, &managed, owned_key);
        let mixed_foreign = serde_json::to_vec_pretty(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": "test-uninstall-foreign-key-987654",
            "tokens": {
                "access_token": "foreign-uninstall-live-token",
                "refresh_token": "foreign-uninstall-live-refresh"
            }
        }))
        .unwrap();
        std::fs::write(home.join("auth.json"), &mixed_foreign).unwrap();
        let vault = MemoryCredentialVault::default();
        vault.set(CredentialSlot::UapiApiKey, owned_key).unwrap();
        let official = official_auth_json();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official)
            .unwrap();
        let settings_before = std::fs::read(&isolated_path).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();

        let error =
            uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("无法确认归属"));
        assert_eq!(std::fs::read(&isolated_path).unwrap(), settings_before);
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(
            std::fs::read(home.join("auth.json")).unwrap(),
            mixed_foreign
        );
        assert_eq!(
            std::fs::read(managed_catalog_path(&home)).unwrap(),
            catalog_before
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some(owned_key)
        );
        assert_eq!(
            vault.get(CredentialSlot::OfficialAuthJson).unwrap(),
            Some(official)
        );
    }

    #[test]
    fn uninstall_cleanup_recognizes_keys_from_every_duplicate_owned_profile() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        let mut profile_a = serde_json::to_value(&managed).unwrap();
        profile_a["authContents"] = json!({"OPENAI_API_KEY": "sk-duplicate-a-012345"})
            .to_string()
            .into();
        let mut profile_b = serde_json::to_value(&managed).unwrap();
        let mut profile_b_config = profile_b["configContents"]
            .as_str()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        profile_b_config["experimental_bearer_token"] = toml_edit::value("sk-duplicate-b-012345");
        profile_b["configContents"] = profile_b_config.to_string().into();
        profile_b["modelList"] = json!({"futureShape": true});
        std::fs::write(
            &isolated_path,
            serde_json::to_vec_pretty(&json!({
                "relayProfiles": [profile_a, profile_b],
                "activeRelayId": distribution::FIXED_PROVIDER_ID,
            }))
            .unwrap(),
        )
        .unwrap();
        write_live_uapi(&home, &managed, "sk-duplicate-b-012345");
        std::fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&json!({
                "OPENAI_API_KEY": "sk-duplicate-b-012345",
                "keep": true,
            }))
            .unwrap(),
        )
        .unwrap();
        let vault = MemoryCredentialVault::default();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        let auth: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).unwrap())
                .unwrap();
        assert!(auth.get("OPENAI_API_KEY").is_none());
        assert_eq!(auth["keep"], true);
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(isolated_path).unwrap()).unwrap();
        assert!(settings["relayProfiles"].as_array().unwrap().is_empty());
    }

    #[test]
    fn uninstall_cleanup_does_not_replace_foreign_live_key_with_official_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        write_live_uapi(&home, &managed, "user-edited-foreign-key-012345");
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "test-owned-not-live-key-012345")
            .unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth_json())
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());
        assert!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn uninstall_cleanup_strips_owned_key_and_preserves_live_official_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        let owned_key = "test-uninstall-mixed-key-012345";
        write_live_uapi(&home, &managed, owned_key);
        std::fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": owned_key,
                "tokens": {
                    "access_token": "fresh-uninstall-live-token",
                    "refresh_token": "fresh-uninstall-live-refresh"
                },
                "keep": "preserved"
            }))
            .unwrap(),
        )
        .unwrap();
        let vault = MemoryCredentialVault::default();
        vault.set(CredentialSlot::UapiApiKey, owned_key).unwrap();
        vault
            .set(
                CredentialSlot::OfficialAuthJson,
                &official_auth_json_with_access_token("stale-uninstall-vault-token"),
            )
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        let live: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("auth.json")).unwrap())
                .unwrap();
        assert!(live.get("OPENAI_API_KEY").is_none());
        assert_eq!(live["tokens"]["access_token"], "fresh-uninstall-live-token");
        assert_eq!(live["keep"], "preserved");
        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());
        assert!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn uninstall_cleanup_preserves_newer_live_official_auth() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        write_live_uapi(&home, &managed, "sk-owned-live-uapi-012345");
        let newer_live_auth = official_auth_json_with_access_token("newer-live-access-token");
        crate::settings::atomic_write_private(&home.join("auth.json"), newer_live_auth.as_bytes())
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-owned-live-uapi-012345")
            .unwrap();
        vault
            .set(
                CredentialSlot::OfficialAuthJson,
                &official_auth_json_with_access_token("stale-snapshot-access-token"),
            )
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            newer_live_auth
        );
    }

    #[test]
    fn uninstall_cleanup_rolls_back_when_second_credential_delete_fails() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
            .unwrap();
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&json!({
                "upstreamOnly": "keep",
                "relayProfiles": [serde_json::to_value(managed.clone()).unwrap()],
                "activeRelayId": distribution::FIXED_PROVIDER_ID,
            }))
            .unwrap(),
        )
        .unwrap();
        write_live_uapi(&home, &managed, "test-uapi-live-key-012345");
        let vault = MemoryCredentialVault::default();
        let official_auth = official_auth_json();
        vault
            .set(CredentialSlot::UapiApiKey, "test-uapi-vault-key-012345")
            .unwrap();
        vault
            .set(CredentialSlot::OfficialAuthJson, &official_auth)
            .unwrap();
        vault.fail_delete(CredentialSlot::OfficialAuthJson);
        let refresh_marker_before = b"refresh-before-uninstall";
        std::fs::write(refresh_request_marker_path(&home), refresh_marker_before).unwrap();
        let isolated_before = std::fs::read(&isolated_path).unwrap();
        let legacy_before = std::fs::read(&legacy_path).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();
        let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();

        let error =
            uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("官方登录快照"));
        assert_eq!(std::fs::read(&isolated_path).unwrap(), isolated_before);
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_before);
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(
            std::fs::read(managed_catalog_path(&home)).unwrap(),
            catalog_before
        );
        assert_eq!(
            std::fs::read(refresh_request_marker_path(&home)).unwrap(),
            refresh_marker_before
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("test-uapi-vault-key-012345")
        );
        assert_eq!(
            vault
                .get(CredentialSlot::OfficialAuthJson)
                .unwrap()
                .as_deref(),
            Some(official_auth.as_str())
        );
    }

    #[test]
    fn uninstall_cleanup_rolls_back_injected_settings_and_catalog_failures() {
        for failure_point in [
            UninstallFailurePoint::AfterIsolatedSettingsCleanup,
            UninstallFailurePoint::AfterCatalogCleanup,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("codex-home");
            let isolated_path = temp.path().join("isolated-settings.json");
            let legacy_path = temp.path().join("legacy-settings.json");
            let store = SettingsStore::new(isolated_path.clone());
            let managed =
                build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
            store
                .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
                .unwrap();
            write_live_uapi(&home, &managed, "test-uapi-live-key-012345");
            let vault = MemoryCredentialVault::default();
            let official_auth = official_auth_json();
            vault
                .set(CredentialSlot::UapiApiKey, "test-uapi-vault-key-012345")
                .unwrap();
            vault
                .set(CredentialSlot::OfficialAuthJson, &official_auth)
                .unwrap();
            let isolated_before = std::fs::read(&isolated_path).unwrap();
            let config_before = std::fs::read(home.join("config.toml")).unwrap();
            let auth_before = std::fs::read(home.join("auth.json")).unwrap();
            let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();

            let error = uninstall_cleanup_with_failure(
                &isolated_path,
                &legacy_path,
                &home,
                &vault,
                Some(failure_point),
            )
            .unwrap_err();

            assert!(error.to_string().contains("simulated uninstall"));
            assert_eq!(
                std::fs::read(&isolated_path).unwrap(),
                isolated_before,
                "{failure_point:?}"
            );
            assert_eq!(
                std::fs::read(home.join("config.toml")).unwrap(),
                config_before,
                "{failure_point:?}"
            );
            assert_eq!(
                std::fs::read(home.join("auth.json")).unwrap(),
                auth_before,
                "{failure_point:?}"
            );
            assert_eq!(
                std::fs::read(managed_catalog_path(&home)).unwrap(),
                catalog_before,
                "{failure_point:?}"
            );
            assert_eq!(
                vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
                Some("test-uapi-vault-key-012345"),
                "{failure_point:?}"
            );
            assert_eq!(
                vault
                    .get(CredentialSlot::OfficialAuthJson)
                    .unwrap()
                    .as_deref(),
                Some(official_auth.as_str()),
                "{failure_point:?}"
            );
        }
    }

    #[test]
    fn uninstall_cleanup_preserves_non_uapi_live_config_and_auth_byte_for_byte() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let config = "model = \"user-model\"\nmodel_provider = \"user_custom\"\n\n[model_providers.user_custom]\nbase_url = \"https://user.example/v1\"\n";
        let auth = r#"{"OPENAI_API_KEY":"user-owned-key","extra":"keep"}"#;
        std::fs::write(home.join("config.toml"), config).unwrap();
        std::fs::write(home.join("auth.json"), auth).unwrap();
        let catalog_path = managed_catalog_path(&home);
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        std::fs::write(&catalog_path, "{\"owned\":true}").unwrap();
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed, UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-vault-uapi-0123456789")
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            config
        );
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            auth
        );
        assert!(!catalog_path.exists());
    }

    #[test]
    fn uninstall_preserves_same_id_and_endpoint_with_foreign_transport_contract() {
        for (name, from, to) in [
            ("wire", "wire_api = \"responses\"", "wire_api = \"chat\""),
            (
                "auth",
                "requires_openai_auth = true",
                "requires_openai_auth = \"true\"",
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("codex-home");
            std::fs::create_dir_all(&home).unwrap();
            let isolated_path = temp.path().join("isolated-settings.json");
            let legacy_path = temp.path().join("legacy-settings.json");
            let store = SettingsStore::new(isolated_path.clone());
            let managed =
                build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
            store
                .save(&managed_settings(managed.clone(), UapiConnectionMode::Uapi))
                .unwrap();
            let config = managed.config_contents.replace(from, to);
            let auth = r#"{"OPENAI_API_KEY":"test-shared-live-key-012345","keep":true}"#;
            std::fs::write(home.join("config.toml"), &config).unwrap();
            std::fs::write(home.join("auth.json"), auth).unwrap();
            let vault = MemoryCredentialVault::default();
            vault
                .set(CredentialSlot::UapiApiKey, "test-shared-live-key-012345")
                .unwrap();

            assert!(!read_live_managed_state(&home).provider_matches, "{name}");
            uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

            assert_eq!(
                std::fs::read_to_string(home.join("config.toml")).unwrap(),
                config,
                "{name}"
            );
            assert_eq!(
                std::fs::read_to_string(home.join("auth.json")).unwrap(),
                auth,
                "{name}"
            );
        }
    }

    #[test]
    fn uninstall_cleanup_refuses_damaged_settings_before_mutating_live_or_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        write_live_uapi(&home, &managed, "sk-live-uapi-0123456789");
        let live_config = std::fs::read(home.join("config.toml")).unwrap();
        let live_auth = std::fs::read(home.join("auth.json")).unwrap();
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        std::fs::write(&isolated_path, "{damaged json").unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-vault-uapi-0123456789")
            .unwrap();

        let error =
            uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("有效 JSON"));
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            live_config
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), live_auth);
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("sk-vault-uapi-0123456789")
        );
        assert_eq!(
            std::fs::read_to_string(isolated_path).unwrap(),
            "{damaged json"
        );
    }

    #[test]
    fn uninstall_cleanup_ignores_unrelated_damaged_legacy_settings() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let config = "model = \"user-model\"\n";
        std::fs::write(home.join("config.toml"), config).unwrap();
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        store
            .save(&managed_settings(managed, UapiConnectionMode::Uapi))
            .unwrap();
        let damaged = b"{damaged upstream settings without an owned provider";
        std::fs::write(&legacy_path, damaged).unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-vault-uapi-0123456789")
            .unwrap();

        uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap();

        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            config
        );
        assert_eq!(std::fs::read(legacy_path).unwrap(), damaged);
        assert!(vault.get(CredentialSlot::UapiApiKey).unwrap().is_none());
    }

    #[test]
    fn uninstall_cleanup_is_fail_closed_when_official_snapshot_is_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let managed = build_managed_profile("gpt-5-codex", &["gpt-5-codex".to_string()]).unwrap();
        write_live_uapi(&home, &managed, "sk-live-uapi-0123456789");
        let isolated_path = temp.path().join("isolated-settings.json");
        let legacy_path = temp.path().join("legacy-settings.json");
        let store = SettingsStore::new(isolated_path.clone());
        store
            .save(&managed_settings(managed, UapiConnectionMode::Uapi))
            .unwrap();
        let vault = MemoryCredentialVault::default();
        vault
            .set(CredentialSlot::UapiApiKey, "sk-live-uapi-0123456789")
            .unwrap();
        let isolated_before = std::fs::read(&isolated_path).unwrap();
        let config_before = std::fs::read(home.join("config.toml")).unwrap();
        let auth_before = std::fs::read(home.join("auth.json")).unwrap();
        let catalog_before = std::fs::read(managed_catalog_path(&home)).unwrap();
        vault.fail_get(CredentialSlot::OfficialAuthJson);

        let error =
            uninstall_cleanup_with(&isolated_path, &legacy_path, &home, &vault).unwrap_err();

        assert!(error.to_string().contains("凭证快照"));
        assert_eq!(std::fs::read(&isolated_path).unwrap(), isolated_before);
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            config_before
        );
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(
            std::fs::read(managed_catalog_path(&home)).unwrap(),
            catalog_before
        );
        assert_eq!(
            vault.get(CredentialSlot::UapiApiKey).unwrap().as_deref(),
            Some("sk-live-uapi-0123456789")
        );
    }

    #[test]
    fn distribution_defaults_disable_hidden_runtime_features() {
        let mut settings = BackendSettings::default();
        settings.codex_app_answer_outline_enabled = true;
        apply_distribution_feature_defaults(&mut settings);
        assert!(!settings.enhancements_enabled);
        assert!(!settings.provider_sync_enabled);
        assert!(!settings.codex_app_plugin_marketplace_unlock);
        assert!(settings.codex_app_model_whitelist_unlock);
        assert!(!settings.codex_app_session_delete);
        assert!(!settings.codex_app_markdown_export);
        assert!(!settings.codex_app_zed_remote_open);
        assert!(!settings.codex_app_upstream_worktree_create);
        assert!(!settings.codex_app_stepwise_enabled);
        assert!(!settings.codex_app_answer_outline_enabled);
        assert!(!settings.codex_app_dream_skin_enabled);
        assert!(!settings.weixin_connect_enabled);
    }
}
