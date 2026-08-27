//! Managed NewAPI integration for the U-API Connect distribution.
//!
//! The module deliberately sits beside the generic relay implementation instead
//! of changing it.  The distribution owns one relay profile while the upstream
//! relay, model-catalog, backup and rollback code remains reusable.

mod credentials;

use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::path::Path;

use anyhow::Context;
use credentials::{CredentialSlot, CredentialVault, SystemCredentialVault};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item};

use crate::distribution;
use crate::settings::{
    BackendSettings, RelayMode, RelayModelInsertMode, RelayProfile, RelayProtocol, SettingsStore,
};

const DEFAULT_CONTEXT_WINDOW: &str = "128000";
const LARGE_CONTEXT_WINDOW: &str = "272000";
const MAX_MODEL_ID_LEN: usize = 200;
const OFFICIAL_RELAY_ID: &str = "uapi_official";

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
    status_from_home_with_vault(
        &crate::codex_home::default_codex_home_dir(),
        &SettingsStore::default(),
        &vault,
    )
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
    let has_legacy_key = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        .map(crate::relay_config::relay_profile_api_key)
        .is_some_and(|key| !key.trim().is_empty());
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
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID);
    let live = read_live_managed_state(home);
    let api_key = stored_api_key
        .filter(|api_key| !api_key.trim().is_empty())
        .or_else(|| {
            profile
                .map(crate::relay_config::relay_profile_api_key)
                .filter(|api_key| !api_key.trim().is_empty())
        })
        .or_else(|| live_managed_api_key(home, &live))
        .unwrap_or_default();
    let compatible_models = profile.map(profile_model_ids).unwrap_or_default();
    let official_auth = crate::relay_config::chatgpt_auth_status_from_home(home);
    let official_login_saved = stored_official_auth
        .as_deref()
        .is_some_and(official_auth_contents_are_valid);
    let current_model = live
        .model
        .filter(|model| contains_model(&compatible_models, model))
        .or_else(|| {
            profile
                .map(crate::relay_config::relay_profile_model)
                .filter(|model| contains_model(&compatible_models, model))
        })
        .unwrap_or_default();
    let configured = live.provider_matches
        && live.base_url_matches
        && !api_key.trim().is_empty()
        && !compatible_models.is_empty();
    let connection_mode = connection_mode(&settings);
    UapiStatus {
        configured,
        active: connection_mode == UapiConnectionMode::Uapi,
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

fn official_auth_contents_are_valid(contents: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(contents) else {
        return false;
    };
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

pub fn enforce_distribution_defaults() -> anyhow::Result<()> {
    let store = SettingsStore::default();
    let home = crate::codex_home::default_codex_home_dir();
    let vault = SystemCredentialVault::default();
    crate::relay_config::with_live_files_transaction(&home, || {
        let mut settings = store.load().unwrap_or_default();
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
    let discovery = discover_models(&api_key).await?;
    let vault = SystemCredentialVault::default();
    apply_discovery_with(
        &SettingsStore::default(),
        &crate::codex_home::default_codex_home_dir(),
        &vault,
        &api_key,
        discovery,
    )
}

pub async fn refresh_models() -> anyhow::Result<UapiApplyResult> {
    let store = SettingsStore::default();
    let vault = SystemCredentialVault::default();
    let home = crate::codex_home::default_codex_home_dir();
    let (api_key, migration_succeeded) =
        crate::relay_config::with_live_files_transaction(&home, || {
            let mut settings = store.load().context("读取本地连接配置失败")?;
            let migration_succeeded = migrate_legacy_managed_api_key_best_effort(
                &store,
                &mut settings,
                &vault,
                "uapi.legacy_credential_migration_deferred_before_refresh",
            );
            let api_key = managed_api_key(&settings, &vault, &home)?;
            Ok((api_key, migration_succeeded))
        })?;
    let discovery = discover_models(&api_key).await?;
    apply_discovery_with_options(
        &store,
        &home,
        &vault,
        &api_key,
        discovery,
        false,
        !migration_succeeded,
    )
}

pub fn switch_connection_mode(mode: UapiConnectionMode) -> anyhow::Result<UapiModeSwitchResult> {
    let store = SettingsStore::default();
    let home = crate::codex_home::default_codex_home_dir();
    let vault = SystemCredentialVault::default();
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
    apply_active_connection_profile_with(&store, &home, &vault)
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
    migrate_legacy_managed_api_key_best_effort(
        store,
        &mut settings,
        vault,
        "uapi.legacy_credential_migration_deferred_before_launch",
    );
    if !settings.relay_profiles_enabled {
        return Ok(());
    }

    match connection_mode(&settings) {
        UapiConnectionMode::Uapi => {
            let api_key = managed_api_key(&settings, vault, home)?;
            let mut profile = managed_profile(&settings)?.clone();
            prioritize_profile_models(&mut profile);
            let profile = hydrate_managed_profile(&profile, &api_key)?;
            crate::relay_config::apply_relay_profile_to_home_with_switch_rules(
                home,
                &profile,
                &managed_common_config(&settings),
            )?;
        }
        UapiConnectionMode::Official => {
            let auth_contents = official_auth_for_launch(home, vault)?;
            clear_managed_config_for_official(home, auth_contents.as_deref())?;
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

fn apply_discovery_with_options_locked(
    store: &SettingsStore,
    home: &Path,
    vault: &impl CredentialVault,
    api_key: &str,
    discovery: UapiModelDiscovery,
    persist_api_key: bool,
    preserve_legacy_api_key: bool,
) -> anyhow::Result<UapiApplyResult> {
    let mut settings = store.load().unwrap_or_default();
    let existing_managed_profile = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
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
    let live_official_auth = capture_live_official_auth(home)?;
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
        "model = {}\nmodel_provider = {}\n\n[model_providers.{}]\nname = {}\nbase_url = {}\nwire_api = \"responses\"\nrequires_openai_auth = false\n",
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
        .cloned()
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
    let live_official_auth = capture_live_official_auth(home)?;
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
    let live_official_auth = capture_live_official_auth(home)?;
    // 实时 auth.json 可能刚被 Codex 刷新；它有效时就是唯一权威来源，
    // 不应再让损坏或过期的存储快照阻断切换。
    let saved_official_auth = if live_official_auth.is_none() {
        stored_official_auth_best_effort(
            vault,
            "uapi.official_auth_snapshot_unavailable_before_official_switch",
        )
    } else {
        None
    };
    let auth_to_restore = live_official_auth
        .as_deref()
        .or(saved_official_auth.as_deref());

    if migration_succeeded {
        if let Some(profile) = settings
            .relay_profiles
            .iter_mut()
            .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        {
            *profile = sanitize_managed_profile(profile.clone())?;
        }
    } else if let Some(profile) = settings
        .relay_profiles
        .iter_mut()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
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
        clear_managed_config_for_official(home, auth_to_restore)
    })?;
    // 实时登录已经留在 auth.json 中，存储同步失败不能让可用的官方模式
    // 回滚。后续状态检查和启动还会继续尝试同步。
    if let Some(contents) = live_official_auth.as_deref() {
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

fn managed_profile(settings: &BackendSettings) -> anyhow::Result<&RelayProfile> {
    settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        .ok_or_else(|| anyhow::anyhow!("尚未配置 U-API 服务密钥"))
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
    let Some(profile_index) = settings
        .relay_profiles
        .iter()
        .position(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
    else {
        return Ok(false);
    };
    let legacy_key =
        crate::relay_config::relay_profile_api_key(&settings.relay_profiles[profile_index]);
    if legacy_key.trim().is_empty() {
        return Ok(false);
    }

    let sanitized = sanitize_managed_profile(settings.relay_profiles[profile_index].clone())?;
    let previous_credential = vault
        .get(CredentialSlot::UapiApiKey)
        .context("读取系统凭证库中的 U-API 密钥失败")?;
    let needs_secure_write = previous_credential
        .as_deref()
        .is_none_or(|value| value.trim().is_empty());
    if needs_secure_write {
        vault
            .set(CredentialSlot::UapiApiKey, legacy_key.trim())
            .context("迁移旧版 U-API 密钥到系统凭证库失败")?;
    }

    let mut sanitized_settings = settings.clone();
    sanitized_settings.relay_profiles[profile_index] = sanitized;
    if let Err(save_error) = store.save(&sanitized_settings) {
        if needs_secure_write {
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
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
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
    Ok(vault
        .get(CredentialSlot::OfficialAuthJson)?
        .filter(|contents| official_auth_contents_are_valid(contents)))
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
    if !crate::relay_config::chatgpt_auth_status_from_home(home).authenticated {
        return Ok(None);
    }
    let contents =
        std::fs::read_to_string(home.join("auth.json")).context("读取当前官方登录信息失败")?;
    Ok(official_auth_contents_are_valid(&contents).then_some(contents))
}

fn clear_managed_config_for_official(
    home: &Path,
    auth_contents: Option<&str>,
) -> anyhow::Result<crate::relay_config::RelayApplyResult> {
    crate::relay_config::with_live_files_transaction(home, || {
        clear_managed_config_for_official_locked(home, auth_contents)
    })
}

fn clear_managed_config_for_official_locked(
    home: &Path,
    auth_contents: Option<&str>,
) -> anyhow::Result<crate::relay_config::RelayApplyResult> {
    let should_clear_model = read_live_managed_state(home).provider_matches;
    let result = crate::relay_config::clear_relay_config_to_home_with_auth(home, auth_contents)?;
    if should_clear_model {
        let config_path = home.join("config.toml");
        let contents = std::fs::read_to_string(&config_path).context("读取官方模式配置失败")?;
        let mut document = contents
            .parse::<DocumentMut>()
            .context("官方模式配置格式无效")?;
        document.as_table_mut().remove("model");
        crate::settings::atomic_write(&config_path, document.to_string().as_bytes())
            .context("清除中转默认模型失败")?;
    }
    Ok(result)
}

fn sanitize_managed_profile(mut profile: RelayProfile) -> anyhow::Result<RelayProfile> {
    profile.api_key.clear();
    profile.auth_contents.clear();
    profile.vlm_api_key.clear();
    crate::relay_config::normalize_relay_profile_for_storage(&mut profile)?;
    Ok(profile)
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
    let original_settings = store.load().context("读取原始本地设置失败")?;
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
            let settings_restore_error = store.save(&original_settings).err();
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
}

impl UapiLiveFilesSnapshot {
    fn capture(home: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            config: read_optional_bytes(&home.join("config.toml"))?,
            auth: read_optional_bytes(&home.join("auth.json"))?,
            managed_catalog: read_optional_bytes(&managed_catalog_path(home))?,
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
        if config_error.is_none() && auth_error.is_none() && catalog_error.is_none() {
            return Ok(());
        }
        anyhow::bail!(
            "Codex 实时文件回滚不完整：config.toml={}，auth.json={}，model catalog={}",
            rollback_status(config_error),
            rollback_status(auth_error),
            rollback_status(catalog_error),
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
    // Disabling page injection also removes the upstream ads, scripts, themes and
    // advanced Codex++ menu from the customer-facing Codex window.
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
    LiveManagedState {
        provider_matches: provider_id == distribution::FIXED_PROVIDER_ID && provider.is_some(),
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
    fn live_snapshot_restore_attempts_config_auth_and_catalog_after_failures() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        std::fs::create_dir(home.join("config.toml")).unwrap();
        std::fs::create_dir(home.join("auth.json")).unwrap();
        let catalog_path = managed_catalog_path(home);
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        std::fs::create_dir(&catalog_path).unwrap();
        let snapshot = UapiLiveFilesSnapshot {
            config: Some(b"old config".to_vec()),
            auth: Some(b"old auth".to_vec()),
            managed_catalog: Some(b"old catalog".to_vec()),
        };

        let error = snapshot.restore(home).unwrap_err().to_string();
        assert!(error.contains("config.toml="));
        assert!(error.contains("auth.json="));
        assert!(error.contains("model catalog="));
        assert!(!error.contains("config.toml=ok"));
        assert!(!error.contains("auth.json=ok"));
        assert!(!error.contains("model catalog=ok"));
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
    fn distribution_defaults_disable_hidden_runtime_features() {
        let mut settings = BackendSettings::default();
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
        assert!(!settings.codex_app_dream_skin_enabled);
        assert!(!settings.weixin_connect_enabled);
    }
}
