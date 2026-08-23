use std::io::ErrorKind;
use std::path::Path;

use anyhow::Context;

use crate::relay_config::{
    backfill_relay_profile_from_home_with_common, relay_config_status_from_home,
};
use crate::settings::{BackendSettings, RelayMode, SettingsStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySwitchResult {
    pub settings: BackendSettings,
    pub configured: bool,
    pub backup_path: Option<String>,
}

pub fn switch_relay_profile_in_home(
    store: &SettingsStore,
    home: &Path,
    next_settings: BackendSettings,
    previous_active_relay_id: &str,
) -> anyhow::Result<RelaySwitchResult> {
    crate::relay_config::with_live_files_transaction(home, || {
        switch_relay_profile_in_home_locked(store, home, next_settings, previous_active_relay_id)
    })
}

fn switch_relay_profile_in_home_locked(
    store: &SettingsStore,
    home: &Path,
    next_settings: BackendSettings,
    previous_active_relay_id: &str,
) -> anyhow::Result<RelaySwitchResult> {
    let mut selected_settings = next_settings;
    if !selected_settings.relay_profiles_enabled {
        anyhow::bail!("供应商配置总开关已关闭，未写入 config.toml / auth.json。");
    }
    crate::codex_app_state::capture_app_state_snapshot_nonfatal(home, "relay_switch.before");

    let original_settings = store.load().unwrap_or_default();
    let target_profile = selected_settings.active_relay_profile();
    let live_snapshot = LiveFilesSnapshot::capture(home, &target_profile.id)
        .context("读取当前 Codex 实时配置失败")?;
    if !previous_active_relay_id.trim().is_empty()
        && previous_active_relay_id != selected_settings.active_relay_id
    {
        backfill_profile_before_switch(home, &mut selected_settings, previous_active_relay_id)?;
    }

    store
        .save(&selected_settings)
        .context("保存供应商设置失败")?;
    let selected_settings = store.load().context("读取供应商设置失败")?;

    match apply_selected_relay_profile(home, &selected_settings) {
        Ok(result) => {
            crate::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
                home,
                "relay_switch.after",
            );
            Ok(result)
        }
        Err(error) => {
            let settings_restore_error = store.save(&original_settings).err();
            let live_restore_error = live_snapshot.restore(home).err();
            if settings_restore_error.is_some() || live_restore_error.is_some() {
                anyhow::bail!(
                    "切换供应商失败：{error}；同时回滚配置失败：settings.json={}，Codex 实时文件={}",
                    settings_restore_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "ok".to_string()),
                    live_restore_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "ok".to_string())
                );
            }
            Err(error)
        }
    }
}

#[derive(Debug, Clone)]
struct LiveFilesSnapshot {
    config: Option<Vec<u8>>,
    auth: Option<Vec<u8>>,
    managed_catalog_path: std::path::PathBuf,
    managed_catalog: Option<Vec<u8>>,
}

impl LiveFilesSnapshot {
    fn capture(home: &Path, target_profile_id: &str) -> anyhow::Result<Self> {
        let managed_catalog_path = home.join(
            crate::relay_config::managed_model_catalog_relative_path(target_profile_id),
        );
        Ok(Self {
            config: read_optional_bytes(&home.join("config.toml"))?,
            auth: read_optional_bytes(&home.join("auth.json"))?,
            managed_catalog: read_optional_bytes(&managed_catalog_path)?,
            managed_catalog_path,
        })
    }

    fn restore(&self, home: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(home)?;
        let config_error =
            restore_optional_file(&home.join("config.toml"), self.config.as_deref()).err();
        let auth_error =
            restore_optional_private_file(&home.join("auth.json"), self.auth.as_deref()).err();
        let catalog_error =
            restore_optional_file(&self.managed_catalog_path, self.managed_catalog.as_deref())
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

fn read_optional_bytes(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_optional_file(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    match contents {
        Some(contents) => crate::settings::atomic_write(path, contents).map_err(Into::into),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn restore_optional_private_file(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    match contents {
        Some(contents) => crate::settings::atomic_write_private(path, contents).map_err(Into::into),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn rollback_status(error: Option<anyhow::Error>) -> String {
    error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "ok".to_string())
}

fn backfill_profile_before_switch(
    home: &Path,
    settings: &mut BackendSettings,
    previous_active_relay_id: &str,
) -> anyhow::Result<()> {
    let profile = settings
        .relay_profiles
        .iter_mut()
        .find(|profile| profile.id == previous_active_relay_id)
        .with_context(|| "当前供应商已不在配置列表中，已停止切换以避免覆盖用户改动。")?;
    backfill_relay_profile_from_home_with_common(
        home,
        profile,
        &mut settings.relay_context_config_contents,
    )
    .with_context(|| "回填当前供应商配置失败")
}

fn apply_selected_relay_profile(
    home: &Path,
    settings: &BackendSettings,
) -> anyhow::Result<RelaySwitchResult> {
    let relay = settings.active_relay_profile();
    let common_config = relay_combined_common_config(settings);
    let result = if relay.relay_mode == RelayMode::Official && !relay.official_mix_api_key {
        let auth_contents =
            (!relay.auth_contents.trim().is_empty()).then_some(relay.auth_contents.as_str());
        crate::relay_config::clear_relay_config_to_home_with_auth(home, auth_contents)?
    } else {
        validate_switch_profile_files(&relay)?;
        crate::relay_config::apply_relay_profile_to_home_with_switch_rules(
            home,
            &relay,
            &common_config,
        )?
    };
    let status = relay_config_status_from_home(home);
    if relay.relay_mode == RelayMode::PureApi && !status.configured {
        anyhow::bail!(
            "纯 API 配置写入后未检测到完整 custom provider，请检查 config.toml 和供应商 API Key。"
        );
    }
    Ok(RelaySwitchResult {
        settings: settings.clone(),
        configured: status.configured,
        backup_path: result.backup_path,
    })
}

fn validate_switch_profile_files(profile: &crate::settings::RelayProfile) -> anyhow::Result<()> {
    if profile.relay_mode != RelayMode::Aggregate && profile.config_contents.trim().is_empty() {
        anyhow::bail!(
            "供应商「{}」缺少独立 config.toml，已停止切换，避免继续显示上一套配置文件。",
            if profile.name.trim().is_empty() {
                profile.id.as_str()
            } else {
                profile.name.as_str()
            }
        );
    }
    if profile.relay_mode == RelayMode::Official
        && serde_json::from_str::<serde_json::Value>(&profile.auth_contents)
            .ok()
            .and_then(|value| {
                value
                    .get("OPENAI_API_KEY")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .map(str::is_empty)
            })
            == Some(false)
    {
        anyhow::bail!(
            "官方混合 API 不应在 auth.json 中保存 OPENAI_API_KEY。请清理此供应商的 auth.json 后再切换。"
        );
    }
    Ok(())
}

fn relay_combined_common_config(settings: &BackendSettings) -> String {
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
