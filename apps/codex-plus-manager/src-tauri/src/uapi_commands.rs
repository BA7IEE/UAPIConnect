//! Tauri commands for the fixed-provider U-API Connect distribution.
//!
//! Keeping these commands outside the upstream `commands.rs` avoids recurring
//! conflicts when CodexPlusPlus adds or reorganizes its general management UI.

use serde::Serialize;
use serde_json::json;

#[derive(Debug, Clone, Serialize)]
pub struct CommandResult<T>
where
    T: Serialize,
{
    pub status: String,
    pub message: String,
    #[serde(flatten)]
    pub payload: T,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UapiKeyRequest {
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsPayload {
    pub report: String,
}

fn ok<T>(message: &str, payload: T) -> CommandResult<T>
where
    T: Serialize,
{
    CommandResult {
        status: "ok".to_string(),
        message: message.to_string(),
        payload,
    }
}

fn failed<T>(message: &str, payload: T) -> CommandResult<T>
where
    T: Serialize,
{
    CommandResult {
        status: "failed".to_string(),
        message: message.to_string(),
        payload,
    }
}

#[tauri::command]
pub fn uapi_status() -> CommandResult<codex_plus_core::uapi::UapiStatus> {
    ok("连接状态已读取。", codex_plus_core::uapi::status())
}

#[tauri::command]
pub async fn uapi_validate_key(
    request: UapiKeyRequest,
) -> CommandResult<codex_plus_core::uapi::UapiModelDiscovery> {
    match codex_plus_core::uapi::validate_key(&request.api_key).await {
        Ok(discovery) => ok("密钥验证成功。", discovery),
        Err(error) => failed(
            &error.to_string(),
            codex_plus_core::uapi::UapiModelDiscovery {
                endpoint: format!(
                    "{}/models",
                    codex_plus_core::distribution::FIXED_BASE_URL.trim_end_matches('/')
                ),
                models: Vec::new(),
                compatible_models: Vec::new(),
                filtered_models: Vec::new(),
            },
        ),
    }
}

#[tauri::command]
pub async fn uapi_configure(
    request: UapiKeyRequest,
) -> CommandResult<codex_plus_core::uapi::UapiApplyResult> {
    match codex_plus_core::uapi::configure(&request.api_key).await {
        Ok(result) => ok("连接配置已保存。", result),
        Err(error) => failed(&error.to_string(), empty_apply_result()),
    }
}

#[tauri::command]
pub async fn uapi_refresh_models() -> CommandResult<codex_plus_core::uapi::UapiApplyResult> {
    match codex_plus_core::uapi::refresh_models().await {
        Ok(result) => ok("兼容模型已刷新。", result),
        Err(error) => failed(&error.to_string(), empty_apply_result()),
    }
}

#[tauri::command]
pub fn uapi_diagnostics() -> CommandResult<DiagnosticsPayload> {
    let settings = codex_plus_core::settings::SettingsStore::default()
        .load()
        .unwrap_or_default();
    let codex_app_path = codex_plus_core::app_paths::resolve_codex_app_dir_with_saved(
        None,
        Some(settings.codex_app_path.as_str()),
    );
    let codex_version = codex_app_path
        .as_deref()
        .and_then(codex_plus_core::app_paths::codex_app_version);
    let entrypoints = codex_plus_core::install::inspect_entrypoints();
    let latest_launch = codex_plus_core::status::StatusStore::default()
        .load_latest()
        .unwrap_or(None);
    let report = serde_json::to_string_pretty(&json!({
        "product": codex_plus_core::distribution::PRODUCT_NAME,
        "version": codex_plus_core::version::VERSION,
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH
        },
        "codex": {
            "appPath": codex_app_path,
            "version": codex_version,
            "latestLaunch": latest_launch
        },
        "entrypoints": entrypoints,
        "connection": codex_plus_core::uapi::status(),
        "logs": {
            "diagnosticLogPath": codex_plus_core::paths::default_diagnostic_log_path(),
            "latestStatusPath": codex_plus_core::paths::default_latest_status_path()
        }
    }))
    .unwrap_or_else(|error| format!("诊断报告序列化失败：{error}"));
    ok("诊断报告已生成。", DiagnosticsPayload { report })
}

fn empty_apply_result() -> codex_plus_core::uapi::UapiApplyResult {
    codex_plus_core::uapi::UapiApplyResult {
        configured: false,
        current_model: String::new(),
        compatible_models: Vec::new(),
        filtered_models: Vec::new(),
        backup_path: None,
        config_path: codex_plus_core::uapi::status().config_path,
    }
}
