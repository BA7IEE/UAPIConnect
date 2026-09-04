use crate::settings::BackendSettings;
use serde_json::{Value, json};
use std::sync::Arc;

/// 复用已有 CDP 生命周期，但只开放健康检查，不开放配置、删除或脚本执行路由。
pub async fn install_desktop_compatibility(
    websocket_url: &str,
    settings: &BackendSettings,
) -> anyhow::Result<()> {
    let script = desktop_compatibility_script(settings)
        .ok_or_else(|| anyhow::anyhow!("U-API compatibility is not enabled"))?;
    crate::bridge::install_bridge(
        websocket_url,
        crate::bridge::BRIDGE_BINDING_NAME,
        Arc::new(|path, _| {
            Box::pin(async move {
                anyhow::ensure!(
                    path == "/backend/status",
                    "U-API compatibility bridge is read-only"
                );
                Ok(json!({ "status": "ok" }))
            })
        }),
        &[script],
    )
    .await?;
    let result = crate::bridge::evaluate_script_with_await_promise(
        websocket_url,
        r#"Promise.resolve(window.__UAPI_DESKTOP_COMPAT_READY__).then(() => {
          const state = window.__UAPI_DESKTOP_COMPATIBILITY__;
          return !!state && ["ready", "off"].includes(state.locale)
            && ["ready", "off"].includes(state.reasoning) && state.gates === "ready";
        })"#,
        true,
    )
    .await?;
    anyhow::ensure!(
        result["result"]["result"]["value"] == true,
        "Codex 中文或推理菜单兼容处理尚未就绪；可能需要重新启动或适配客户端版本"
    );
    Ok(())
}

/// 仅为已经配置过本发行版的用户启用；不改变通用版“关闭增强”的行为。
pub fn desktop_compatibility_enabled(settings: &BackendSettings) -> bool {
    crate::distribution::FIXED_PROVIDER_EDITION
        && !settings.enhancements_enabled
        && settings
            .relay_profiles
            .iter()
            .any(super::managed_profile_is_owned)
}

pub fn desktop_compatibility_script(settings: &BackendSettings) -> Option<String> {
    if !desktop_compatibility_enabled(settings) {
        return None;
    }
    let active = settings.relay_profiles_enabled
        && settings.active_relay_id == crate::distribution::FIXED_PROVIDER_ID
        && settings.active_aggregate_relay_id.is_empty();
    let mut efforts = Vec::new();
    if active {
        for profile in settings
            .relay_profiles
            .iter()
            .filter(|p| super::managed_profile_is_owned(p))
        {
            for model in super::profile_model_ids(profile) {
                if let Some(metadata) = crate::model_suffix::model_ui_metadata(&model) {
                    for entry in metadata["supportedReasoningEfforts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if let Some(effort @ ("max" | "ultra")) = entry["reasoningEffort"].as_str()
                            && !efforts.contains(&effort.to_string())
                        {
                            efforts.push(effort.to_string());
                        }
                    }
                }
            }
        }
    }
    let config: Value = json!({
        "forceChinese": settings.codex_app_force_chinese_locale,
        "reasoningEfforts": efforts,
    });
    Some(format!(
        "window.__UAPI_DESKTOP_COMPAT_CONFIG__ = {config};\n{}",
        include_str!("desktop-compat.js")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(model: &str) -> BackendSettings {
        let profile = super::super::build_managed_profile(model, &[model.to_string()]).unwrap();
        let mut settings = BackendSettings {
            relay_profiles_enabled: true,
            active_relay_id: crate::distribution::FIXED_PROVIDER_ID.to_string(),
            relay_profiles: vec![profile],
            ..BackendSettings::default()
        };
        super::super::apply_distribution_feature_defaults(&mut settings);
        settings
    }

    #[test]
    fn compat_is_isolated_and_only_enables_declared_efforts() {
        assert!(desktop_compatibility_script(&BackendSettings::default()).is_none());
        for (model, expected) in [
            ("gpt-5.6", r#""reasoningEfforts":["max","ultra"]"#),
            ("gpt-5.6-luna", r#""reasoningEfforts":["max"]"#),
            ("custom-model", r#""reasoningEfforts":[]"#),
        ] {
            let script = desktop_compatibility_script(&settings(model)).unwrap();
            assert!(script.lines().next().unwrap().contains(expected));
            for forbidden in [
                "renderCodexPlusMenu",
                "dream-skin",
                "discord.gg",
                "build_enabled_bundle",
                "OPENAI_API_KEY",
            ] {
                assert!(!script.contains(forbidden), "{forbidden}");
            }
        }
        let mut official = settings("gpt-5.6");
        official.active_relay_id = super::super::OFFICIAL_RELAY_ID.to_string();
        assert!(
            desktop_compatibility_script(&official)
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .contains(r#""reasoningEfforts":[]"#)
        );
        official.enhancements_enabled = true;
        assert!(!desktop_compatibility_enabled(&official));
    }

    #[test]
    fn desktop_compatibility_runtime_regressions() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("node")
            .arg("--input-type=commonjs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Node is required for renderer regression tests");
        let input = format!(
            "const source = {};\n{}",
            serde_json::to_string(include_str!("desktop-compat.js")).unwrap(),
            include_str!("desktop-compat.test.cjs")
        );
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
