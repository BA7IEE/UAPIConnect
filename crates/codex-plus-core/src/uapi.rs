//! Managed NewAPI integration for the U-API Connect distribution.
//!
//! The module deliberately sits beside the generic relay implementation instead
//! of changing it.  The distribution owns one relay profile while the upstream
//! relay, model-catalog, backup and rollback code remains reusable.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::Context;
use serde::Serialize;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item};

use crate::distribution;
use crate::settings::{
    BackendSettings, RelayContextSelection, RelayMode, RelayModelInsertMode, RelayProfile,
    RelayProtocol, SettingsStore,
};

const DEFAULT_CONTEXT_WINDOW: &str = "128000";
const LARGE_CONTEXT_WINDOW: &str = "272000";
const MAX_MODEL_ID_LEN: usize = 200;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelCandidate {
    id: String,
    supported_endpoint_types: Vec<String>,
}

pub fn status() -> UapiStatus {
    status_from_home(
        &crate::codex_home::default_codex_home_dir(),
        &SettingsStore::default(),
    )
}

pub fn status_from_home(home: &Path, store: &SettingsStore) -> UapiStatus {
    let settings = store.load().unwrap_or_default();
    let profile = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID);
    let api_key = profile
        .map(crate::relay_config::relay_profile_api_key)
        .unwrap_or_default();
    let compatible_models = profile
        .map(profile_model_ids)
        .unwrap_or_default();
    let live = read_live_managed_state(home);
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
    UapiStatus {
        configured,
        active: settings.active_relay_id == distribution::FIXED_PROVIDER_ID,
        provider_id: distribution::FIXED_PROVIDER_ID.to_string(),
        base_url: distribution::FIXED_BASE_URL.to_string(),
        current_model,
        model_count: compatible_models.len(),
        compatible_models,
        api_key_masked: mask_api_key(&api_key),
        config_path: home.join("config.toml").to_string_lossy().to_string(),
    }
}

pub fn enforce_distribution_defaults() -> anyhow::Result<()> {
    let store = SettingsStore::default();
    let mut settings = store.load().unwrap_or_default();
    apply_distribution_feature_defaults(&mut settings);
    store
        .save(&settings)
        .context("保存 U-API Connect 发行版设置失败")
}

pub async fn validate_key(api_key: &str) -> anyhow::Result<UapiModelDiscovery> {
    discover_models(api_key).await
}

pub async fn configure(api_key: &str) -> anyhow::Result<UapiApplyResult> {
    let api_key = normalize_api_key(api_key)?;
    let discovery = discover_models(&api_key).await?;
    apply_discovery(&SettingsStore::default(), &api_key, discovery)
}

pub async fn refresh_models() -> anyhow::Result<UapiApplyResult> {
    let store = SettingsStore::default();
    let settings = store.load().context("读取本地连接配置失败")?;
    let profile = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        .ok_or_else(|| anyhow::anyhow!("尚未配置服务密钥"))?;
    let api_key = crate::relay_config::relay_profile_api_key(profile);
    let api_key = normalize_api_key(&api_key)?;
    let discovery = discover_models(&api_key).await?;
    apply_discovery(&store, &api_key, discovery)
}

pub async fn discover_models(api_key: &str) -> anyhow::Result<UapiModelDiscovery> {
    let api_key = normalize_api_key(api_key)?;
    let endpoint = format!("{}/models", distribution::FIXED_BASE_URL.trim_end_matches('/'));
    let client = crate::http_client::proxied_client(distribution::PRODUCT_NAME)?;
    let response = client
        .get(&endpoint)
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
    models.sort_by_key(|model| model.id.to_ascii_lowercase());

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
        anyhow::bail!("当前密钥暂未开通明确支持 Responses 的 Codex 模型");
    }

    Ok(UapiModelDiscovery {
        endpoint: endpoint.to_string(),
        models,
        compatible_models,
        filtered_models,
    })
}

fn apply_discovery(
    store: &SettingsStore,
    api_key: &str,
    discovery: UapiModelDiscovery,
) -> anyhow::Result<UapiApplyResult> {
    let home = crate::codex_home::default_codex_home_dir();
    let mut settings = store.load().unwrap_or_default();
    let previous_active_relay_id = settings
        .relay_profiles
        .iter()
        .any(|profile| profile.id == settings.active_relay_id)
        .then(|| settings.active_relay_id.clone())
        .unwrap_or_default();

    let existing_managed_model = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == distribution::FIXED_PROVIDER_ID)
        .map(crate::relay_config::relay_profile_model)
        .unwrap_or_default();
    let live_model = read_live_managed_state(&home).model.unwrap_or_default();
    let selected_model = choose_model(
        &discovery.compatible_models,
        [&live_model, &existing_managed_model],
    )
    .ok_or_else(|| anyhow::anyhow!("没有可用于 Codex 的模型"))?;

    let profile = build_managed_profile(
        api_key,
        &selected_model,
        &discovery.compatible_models,
    )?;
    upsert_managed_profile(&mut settings, profile);
    apply_distribution_feature_defaults(&mut settings);
    settings.relay_profiles_enabled = true;
    settings.active_relay_id = distribution::FIXED_PROVIDER_ID.to_string();
    settings.active_aggregate_relay_id.clear();
    settings.relay_test_model = selected_model.clone();

    let switched = crate::relay_switch::switch_relay_profile_in_home(
        store,
        &home,
        settings,
        &previous_active_relay_id,
    )?;

    Ok(UapiApplyResult {
        configured: switched.configured,
        current_model: selected_model,
        compatible_models: discovery.compatible_models,
        filtered_models: discovery.filtered_models,
        backup_path: switched.backup_path,
        config_path: home.join("config.toml").to_string_lossy().to_string(),
    })
}

fn build_managed_profile(
    api_key: &str,
    selected_model: &str,
    models: &[String],
) -> anyhow::Result<RelayProfile> {
    let selected_model = selected_model.trim();
    if selected_model.is_empty() {
        anyhow::bail!("默认模型不能为空");
    }
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
    let auth_contents = serde_json::to_string_pretty(&json!({
        "OPENAI_API_KEY": api_key.trim()
    }))?;

    let mut profile = RelayProfile {
        id: distribution::FIXED_PROVIDER_ID.to_string(),
        name: distribution::FIXED_PROVIDER_NAME.to_string(),
        model: selected_model.to_string(),
        base_url: distribution::FIXED_BASE_URL.to_string(),
        upstream_base_url: String::new(),
        api_key: api_key.trim().to_string(),
        protocol: RelayProtocol::Responses,
        relay_mode: RelayMode::PureApi,
        official_mix_api_key: false,
        hide_official_usage_alert: true,
        test_model: selected_model.to_string(),
        config_contents,
        auth_contents,
        use_common_config: true,
        context_selection: RelayContextSelection::default(),
        context_selection_initialized: false,
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
    profile
        .model_list
        .split(['\r', '\n', ','])
        .chain(std::iter::once(profile.model.as_str()))
        .map(str::trim)
        .filter(|model| valid_model_id(model))
        .filter(|model| seen.insert(model.to_ascii_lowercase()))
        .map(ToString::to_string)
        .collect()
}

#[derive(Debug, Default)]
struct LiveManagedState {
    provider_matches: bool,
    base_url_matches: bool,
    model: Option<String>,
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
    if candidate.supported_endpoint_types.is_empty() && is_known_openai_family(&candidate.id) {
        return (true, "旧版服务未返回端点元数据，按 GPT/Codex 兼容兜底".to_string());
    }
    if candidate.supported_endpoint_types.is_empty() {
        return (false, "服务未声明 Responses API 能力".to_string());
    }
    (false, "仅支持 Chat 等非 Responses 端点".to_string())
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
    models
        .iter()
        .find(|model| model.to_ascii_lowercase().contains("codex"))
        .cloned()
        .or_else(|| {
            models
                .iter()
                .find(|model| model.to_ascii_lowercase().starts_with("gpt-"))
                .cloned()
        })
        .or_else(|| {
            models
                .iter()
                .find(|model| {
                    let lower = model.to_ascii_lowercase();
                    lower.starts_with("o1-")
                        || lower.starts_with("o3-")
                        || lower.starts_with("o4-")
                })
                .cloned()
        })
        .or_else(|| models.first().cloned())
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
        && !model.chars().any(|character| character.is_control() || character.is_whitespace())
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
    use super::*;

    fn candidate(id: &str, endpoints: &[&str]) -> ModelCandidate {
        ModelCandidate {
            id: id.to_string(),
            supported_endpoint_types: endpoints.iter().map(|item| item.to_string()).collect(),
        }
    }

    #[test]
    fn filters_chat_only_domestic_models() {
        assert!(!compatibility(&candidate("deepseek-chat", &["openai"])).0);
        assert!(!compatibility(&candidate("glm-4", &["openai"])).0);
    }

    #[test]
    fn includes_explicit_responses_models() {
        assert!(compatibility(&candidate(
            "deepseek-v3-codex",
            &["openai", "openai-response"]
        ))
        .0);
    }

    #[test]
    fn old_server_fallback_is_restricted_to_known_openai_families() {
        assert!(compatibility(&candidate("gpt-5.5", &[])).0);
        assert!(compatibility(&candidate("custom-codex", &[])).0);
        assert!(!compatibility(&candidate("unknown-chat-model", &[])).0);
    }

    #[test]
    fn current_model_is_preserved_without_a_hardcoded_version() {
        let models = vec![
            "gpt-5.5".to_string(),
            "gpt-5.6".to_string(),
            "domestic-codex".to_string(),
        ];
        let current = "gpt-5.6".to_string();
        assert_eq!(
            choose_model(&models, [&current]).as_deref(),
            Some("gpt-5.6")
        );
        assert_eq!(
            choose_model(&models, std::iter::empty::<&String>()).as_deref(),
            Some("domestic-codex")
        );
    }

    #[test]
    fn managed_profile_uses_matching_root_and_provider_table() {
        let profile = build_managed_profile(
            "sk-test-0123456789",
            "gpt-custom-codex",
            &["gpt-custom-codex".to_string(), "domestic-codex".to_string()],
        )
        .unwrap();
        assert_eq!(
            crate::relay_config::root_key_string(&profile.config_contents, "model_provider")
                .as_deref(),
            Some(distribution::FIXED_PROVIDER_ID)
        );
        assert!(profile.config_contents.contains("[model_providers.uapi_connect]"));
        assert!(!profile.config_contents.contains("[model_providers.custom]"));
    }

    #[test]
    fn managed_profile_serialization_does_not_expose_api_key_field() {
        let profile = build_managed_profile(
            "sk-test-0123456789",
            "gpt-custom-codex",
            &["gpt-custom-codex".to_string()],
        )
        .unwrap();
        let value = serde_json::to_value(profile).unwrap();
        assert!(value.get("apiKey").is_none());
    }

    #[test]
    fn upsert_preserves_unrelated_profiles() {
        let mut settings = BackendSettings::default();
        settings.relay_profiles.push(RelayProfile {
            id: "unrelated".to_string(),
            name: "Unrelated".to_string(),
            ..RelayProfile::default()
        });
        let managed = build_managed_profile(
            "sk-test-0123456789",
            "gpt-custom-codex",
            &["gpt-custom-codex".to_string()],
        )
        .unwrap();
        upsert_managed_profile(&mut settings, managed);
        assert!(settings.relay_profiles.iter().any(|item| item.id == "unrelated"));
        assert!(settings
            .relay_profiles
            .iter()
            .any(|item| item.id == distribution::FIXED_PROVIDER_ID));
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
