use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use serde::{Deserialize, Serialize};

pub mod macos;
pub mod windows;

pub const SILENT_NAME: &str = crate::distribution::PRODUCT_NAME;
pub const MANAGER_NAME: &str = crate::distribution::MANAGER_DISPLAY_NAME;
pub const SILENT_BINARY: &str = "codex-plus-plus";
pub const MANAGER_BINARY: &str = "codex-plus-plus-manager";
pub const SILENT_BUNDLE_ID: &str = crate::distribution::SILENT_BUNDLE_ID;
pub const MANAGER_BUNDLE_ID: &str = crate::distribution::MANAGER_BUNDLE_ID;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstallOptions {
    #[serde(default)]
    pub install_root: Option<PathBuf>,
    #[serde(default)]
    pub launcher_path: Option<PathBuf>,
    #[serde(default)]
    pub manager_path: Option<PathBuf>,
    #[serde(default)]
    pub remove_owned_data: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortcutState {
    pub installed: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryPointState {
    pub silent_shortcut: ShortcutState,
    pub management_shortcut: ShortcutState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallActionResult {
    pub status: String,
    pub message: String,
    pub silent_shortcut: ShortcutState,
    pub management_shortcut: ShortcutState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosAppBundle {
    pub app_path: PathBuf,
    pub info_plist: String,
    pub launch_script: String,
    pub binary_source: Option<PathBuf>,
    pub binary_target_name: Option<String>,
}

impl ShortcutState {
    pub fn missing(path: Option<PathBuf>) -> Self {
        Self {
            installed: false,
            path: path.map(|path| path.to_string_lossy().to_string()),
        }
    }

    pub fn from_candidates(candidates: Vec<PathBuf>) -> Self {
        if let Some(path) = candidates.iter().find(|path| path.exists()) {
            return Self {
                installed: true,
                path: Some(path.to_string_lossy().to_string()),
            };
        }
        Self::missing(candidates.into_iter().next())
    }
}

pub fn shortcut_names() -> (&'static str, &'static str) {
    ("U-API Connect.lnk", "U-API Connect 设置.lnk")
}

pub fn app_bundle_names() -> (&'static str, &'static str) {
    ("U-API Connect.app", "U-API Connect 设置.app")
}

pub fn inspect_entrypoints() -> EntryPointState {
    let root = default_install_root();
    EntryPointState {
        silent_shortcut: ShortcutState::from_candidates(entrypoint_candidates(&root, false)),
        management_shortcut: ShortcutState::from_candidates(entrypoint_candidates(&root, true)),
    }
}

pub fn install_entrypoints(options: &InstallOptions) -> InstallActionResult {
    let result = platform_install(options);
    action_result(result, "入口已安装。")
}

pub fn uninstall_entrypoints(options: &InstallOptions) -> InstallActionResult {
    let result = platform_uninstall(options);
    if result.is_ok() && options.remove_owned_data {
        let _ = remove_owned_data();
    }
    action_result(result, "入口已卸载。")
}

pub fn repair_entrypoints(options: &InstallOptions) -> InstallActionResult {
    #[cfg(target_os = "macos")]
    {
        let result = macos::repair_app_bundles(options);
        let success_message = match result.as_ref() {
            Ok(summary) if summary.repaired.is_empty() => {
                "入口检查完成，现有应用入口完整，未改写任何 bundle 文件。".to_string()
            }
            Ok(summary) => format!(
                "入口检查完成，已修复无签名开发/测试入口：{}。",
                summary.repaired.join("、")
            ),
            Err(_) => "入口修复失败。".to_string(),
        };
        return action_result(result.map(|_| ()), &success_message);
    }

    #[cfg(not(target_os = "macos"))]
    let result = platform_install(options);
    #[cfg(not(target_os = "macos"))]
    {
        action_result(result, "入口已修复。")
    }
}

pub fn build_windows_entrypoint_plan(options: &InstallOptions) -> windows::WindowsEntrypointPlan {
    windows::build_windows_entrypoint_plan(options)
}

pub fn build_macos_app_bundle(options: &InstallOptions, manager: bool) -> MacosAppBundle {
    macos::build_app_bundle(options, manager)
}

pub fn remove_owned_data() -> std::io::Result<()> {
    let dir = crate::paths::default_app_state_dir();
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

pub fn default_install_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        return crate::windows_integration::desktop_dir().or_else(|| {
            directories::UserDirs::new().and_then(|dirs| dirs.desktop_dir().map(PathBuf::from))
        });
    }

    #[cfg(target_os = "macos")]
    {
        let sys_apps = PathBuf::from("/Applications");
        if sys_apps.join(format!("{SILENT_NAME}.app")).exists()
            || sys_apps.join(format!("{MANAGER_NAME}.app")).exists()
        {
            return Some(sys_apps);
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = macos_applications_dir_from_exe(&exe) {
                if is_macos_applications_dir(&dir) {
                    return Some(dir);
                }
            }
        }
        return Some(sys_apps);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        directories::UserDirs::new().and_then(|dirs| dirs.desktop_dir().map(PathBuf::from))
    }
}

pub fn default_install_root_strategy() -> &'static str {
    if cfg!(windows) {
        "windows-known-folder"
    } else if cfg!(target_os = "macos") {
        "macos-applications"
    } else {
        "user-dirs-desktop"
    }
}

fn platform_install(options: &InstallOptions) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows::install_shortcuts(options)
    }

    #[cfg(target_os = "macos")]
    {
        macos::install_app_bundles(options)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = options;
        anyhow::bail!("当前平台暂不支持安装 Codex++ 入口")
    }
}

fn platform_uninstall(options: &InstallOptions) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows::uninstall_shortcuts(options)
    }

    #[cfg(target_os = "macos")]
    {
        macos::uninstall_app_bundles(options)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = options;
        anyhow::bail!("当前平台暂不支持卸载 Codex++ 入口")
    }
}

fn action_result(result: anyhow::Result<()>, success_message: &str) -> InstallActionResult {
    let state = inspect_entrypoints();
    match result {
        Ok(()) => InstallActionResult {
            status: "ok".to_string(),
            message: success_message.to_string(),
            silent_shortcut: state.silent_shortcut,
            management_shortcut: state.management_shortcut,
        },
        Err(error) => InstallActionResult {
            status: "failed".to_string(),
            message: error.to_string(),
            silent_shortcut: state.silent_shortcut,
            management_shortcut: state.management_shortcut,
        },
    }
}

fn entrypoint_candidates(root: &Option<PathBuf>, manager: bool) -> Vec<PathBuf> {
    let Some(root) = root else {
        return Vec::new();
    };
    let name = if manager { MANAGER_NAME } else { SILENT_NAME };
    if cfg!(windows) {
        vec![root.join(format!("{name}.lnk"))]
    } else if cfg!(target_os = "macos") {
        vec![root.join(format!("{name}.app"))]
    } else {
        vec![root.join(format!("{name}.desktop"))]
    }
}

pub fn option_or_current_exe(value: &Option<PathBuf>, binary: &str) -> PathBuf {
    if let Some(value) = value {
        return value.clone();
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    companion_binary_path_from_exe(&exe, binary)
}

pub fn companion_binary_path(binary: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    companion_binary_path_from_exe(&exe, binary)
}

pub fn spawn_companion<I, S>(binary: &str, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<OsString>>();

    if companion_requests_manager_configuration(binary, &args) {
        crate::manager_activation::request_configure()
            .context("无法通知已运行的 U-API Connect 设置窗口")?;
    }

    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        if let Some(bundle_id) = macos_companion_bundle_identifier_from_exe(&exe, binary) {
            let open_args = macos_open_bundle_arguments(bundle_id, &args);
            let launch_result = Command::new("/usr/bin/open")
                // Reuse and reactivate an existing bundle. `-n` forced a second
                // manager process which immediately lost the single-instance
                // guard, leaving the original hidden window untouched.
                .args(open_args)
                .status();
            if launch_result.as_ref().is_ok_and(|status| status.success()) {
                return Ok(format!("bundle:{bundle_id}"));
            }
            let fallback = companion_binary_path_from_exe(&exe, binary);
            if !fallback.exists() {
                let detail = launch_result
                    .map(|status| status.to_string())
                    .unwrap_or_else(|error| error.to_string());
                anyhow::bail!("macOS Launch Services 无法启动 bundle {bundle_id}：{detail}");
            }
        }
    }

    let path = companion_binary_path(binary);
    let mut command = Command::new(&path);
    command.args(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::windows_create_no_window());
    }
    command
        .spawn()
        .map_err(|error| anyhow::anyhow!("无法启动 {}：{error}", path.to_string_lossy()))?;
    Ok(path.to_string_lossy().to_string())
}

fn companion_requests_manager_configuration(binary: &str, args: &[OsString]) -> bool {
    crate::distribution::FIXED_PROVIDER_EDITION
        && binary == MANAGER_BINARY
        && args.iter().any(|arg| arg == "--configure")
}

#[cfg(target_os = "macos")]
fn macos_open_bundle_arguments(bundle_id: &str, args: &[OsString]) -> Vec<OsString> {
    let mut open_args = vec![
        OsString::from("-b"),
        OsString::from(bundle_id),
        OsString::from("--args"),
    ];
    open_args.extend(args.iter().cloned());
    open_args
}

pub fn macos_companion_bundle_identifier_from_exe(
    exe: &Path,
    binary: &str,
) -> Option<&'static str> {
    let (_, app_name) = macos_applications_dir_and_app_name_from_exe(exe)?;
    let known_bundle =
        app_name == format!("{SILENT_NAME}.app") || app_name == format!("{MANAGER_NAME}.app");
    if !known_bundle {
        return None;
    }
    match binary {
        SILENT_BINARY => Some(SILENT_BUNDLE_ID),
        MANAGER_BINARY => Some(MANAGER_BUNDLE_ID),
        _ => None,
    }
}

pub fn companion_binary_path_from_exe(exe: &Path, binary: &str) -> PathBuf {
    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    if let Some(bundle_binary) = macos_companion_binary_from_exe(exe, binary) {
        // A local Tauri bundle contains the manager only. Prefer the freshly
        // built launcher beside `target/release` when the sibling app is not
        // present, while keeping the installed /Applications layout intact.
        if bundle_binary.exists() || !is_macos_development_bundle(exe) {
            return bundle_binary;
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(development_binary) = macos_development_companion_binary(exe, binary) {
        return development_binary;
    }
    let same_bundle = dir.join(binary);
    if same_bundle.exists() {
        return same_bundle;
    }
    dir.join(format!("{binary}{suffix}"))
}

fn is_macos_development_bundle(exe: &Path) -> bool {
    exe.components()
        .any(|component| component.as_os_str() == "target")
        && exe
            .components()
            .any(|component| component.as_os_str() == "bundle")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn macos_companion_open_reuses_the_existing_bundle() {
        let args = macos_open_bundle_arguments(MANAGER_BUNDLE_ID, &[OsString::from("--configure")]);

        assert_eq!(
            args,
            vec![
                OsString::from("-b"),
                OsString::from(MANAGER_BUNDLE_ID),
                OsString::from("--args"),
                OsString::from("--configure"),
            ]
        );
        assert!(!args.iter().any(|arg| arg == "-n"));
    }
}

#[cfg(test)]
mod manager_activation_tests {
    use super::*;

    #[test]
    fn only_manager_configure_launches_request_an_activation() {
        assert!(companion_requests_manager_configuration(
            MANAGER_BINARY,
            &[OsString::from("--configure")]
        ));
        assert!(!companion_requests_manager_configuration(
            MANAGER_BINARY,
            &[]
        ));
        assert!(!companion_requests_manager_configuration(
            SILENT_BINARY,
            &[OsString::from("--configure")]
        ));
        assert!(!companion_requests_manager_configuration(
            MANAGER_BINARY,
            &[OsString::from("--show-update")]
        ));
    }

    #[test]
    fn activation_is_requested_before_any_platform_launch() {
        let source = include_str!("mod.rs");
        let activation = source
            .find("companion_requests_manager_configuration(binary, &args)")
            .expect("configure activation hook");
        let macos_launch = source
            .find("Command::new(\"/usr/bin/open\")")
            .expect("macOS bundle launch");
        let process_launch = source
            .find("let mut command = Command::new(&path)")
            .expect("direct companion launch");

        assert!(activation < macos_launch);
        assert!(activation < process_launch);
    }
}

#[cfg(target_os = "macos")]
fn macos_development_companion_binary(exe: &Path, binary: &str) -> Option<PathBuf> {
    let mut path = exe.parent()?;
    while let Some(parent) = path.parent() {
        if matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("release" | "debug")
        ) {
            let candidate = path.join(binary);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        path = parent;
    }
    None
}

fn macos_companion_binary_from_exe(exe: &Path, binary: &str) -> Option<PathBuf> {
    let (applications_dir, app_name) = macos_applications_dir_and_app_name_from_exe(exe)?;
    if binary == SILENT_BINARY {
        if app_name == format!("{SILENT_NAME}.app") {
            return Some(macos_preferred_bundle_binary(
                exe,
                SILENT_BINARY,
                "CodexPlusPlus",
            ));
        }
        let macos = applications_dir
            .join(format!("{SILENT_NAME}.app"))
            .join("Contents")
            .join("MacOS");
        return Some(
            macos
                .join(SILENT_BINARY)
                .exists()
                .then(|| macos.join(SILENT_BINARY))
                .unwrap_or_else(|| macos.join("CodexPlusPlus")),
        );
    }
    if binary == MANAGER_BINARY {
        if app_name == format!("{MANAGER_NAME}.app") {
            return Some(macos_preferred_bundle_binary(
                exe,
                MANAGER_BINARY,
                "CodexPlusPlusManager",
            ));
        }
        let macos = applications_dir
            .join(format!("{MANAGER_NAME}.app"))
            .join("Contents")
            .join("MacOS");
        return Some(
            macos
                .join(MANAGER_BINARY)
                .exists()
                .then(|| macos.join(MANAGER_BINARY))
                .unwrap_or_else(|| macos.join("CodexPlusPlusManager")),
        );
    }
    None
}

fn macos_preferred_bundle_binary(
    exe: &Path,
    sidecar_name: &str,
    bundle_executable_name: &str,
) -> PathBuf {
    let macos = exe.parent().unwrap_or_else(|| Path::new("."));
    let sidecar = macos.join(sidecar_name);
    if sidecar.exists() {
        return sidecar;
    }
    let bundle_executable = macos.join(bundle_executable_name);
    if bundle_executable.exists() {
        return bundle_executable;
    }
    exe.to_path_buf()
}

#[cfg(target_os = "macos")]
fn macos_applications_dir_from_exe(exe: &Path) -> Option<PathBuf> {
    macos_applications_dir_and_app_name_from_exe(exe).map(|(dir, _)| dir)
}

fn macos_applications_dir_and_app_name_from_exe(exe: &Path) -> Option<(PathBuf, String)> {
    let mut path = exe;
    while let Some(parent) = path.parent() {
        if path.extension().and_then(|extension| extension.to_str()) == Some("app") {
            let app_name = path.file_name()?.to_string_lossy().to_string();
            return Some((parent.to_path_buf(), app_name));
        }
        path = parent;
    }
    None
}

#[cfg(target_os = "macos")]
fn is_macos_applications_dir(path: &Path) -> bool {
    if path == Path::new("/Applications") {
        return true;
    }
    directories::BaseDirs::new()
        .map(|dirs| path == dirs.home_dir().join("Applications"))
        .unwrap_or(false)
}

pub(crate) fn install_root_or_default(options: &InstallOptions) -> PathBuf {
    options
        .install_root
        .clone()
        .or_else(default_install_root)
        .unwrap_or_else(|| PathBuf::from("."))
}
