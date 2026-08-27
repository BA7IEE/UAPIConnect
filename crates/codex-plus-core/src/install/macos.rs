#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[cfg(target_os = "macos")]
use anyhow::Context;

use super::{
    InstallOptions, MANAGER_BINARY, MANAGER_BUNDLE_ID, MANAGER_NAME, MacosAppBundle, SILENT_BINARY,
    SILENT_BUNDLE_ID, SILENT_NAME, install_root_or_default, option_or_current_exe,
};

pub fn build_app_bundle(options: &InstallOptions, manager: bool) -> MacosAppBundle {
    let install_root = install_root_or_default(options);
    let display_name = if manager { MANAGER_NAME } else { SILENT_NAME };
    let executable_name = if manager {
        "CodexPlusPlusManager"
    } else {
        "CodexPlusPlus"
    };
    let binary = if manager {
        MANAGER_BINARY
    } else {
        SILENT_BINARY
    };
    let binary_source = install_binary_source(
        option_or_current_exe(
            if manager {
                &options.manager_path
            } else {
                &options.launcher_path
            },
            binary,
        ),
        binary,
    );
    let bundle_id = if manager {
        MANAGER_BUNDLE_ID
    } else {
        SILENT_BUNDLE_ID
    };
    MacosAppBundle {
        app_path: install_root.join(format!("{display_name}.app")),
        info_plist: info_plist(display_name, executable_name, bundle_id, manager),
        launch_script: launch_script(binary),
        binary_source: Some(binary_source),
        binary_target_name: Some(binary.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MacosBundleRepairSummary {
    pub repaired: Vec<String>,
    pub unchanged: Vec<String>,
}

fn launch_script(binary: &str) -> String {
    format!(
        "#!/bin/sh\nDIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\nexec \"$DIR/{binary}\" \"$@\"\n"
    )
}

fn install_binary_source(target: std::path::PathBuf, binary: &str) -> std::path::PathBuf {
    if is_bundle_macos_target(&target) {
        let sidecar = target
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(binary);
        if sidecar.exists() {
            return sidecar;
        }
    }
    target
}

fn is_bundle_macos_target(target: &Path) -> bool {
    target
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("MacOS")
        && target
            .parent()
            .and_then(|parent| parent.parent())
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some("Contents")
}

#[cfg(target_os = "macos")]
pub fn install_app_bundles(options: &InstallOptions) -> anyhow::Result<()> {
    let bundles = [
        build_app_bundle(options, false),
        build_app_bundle(options, true),
    ];

    // “安装入口”保留原有的首次安装语义：两个目标都不存在时可以从当前发行
    // 二进制创建应用。只要已经存在任一 bundle，就转入签名安全的 repair 流程，
    // 避免安装动作覆盖一个已签名但损坏的应用。
    if bundles
        .iter()
        .all(|bundle| !bundle_target_exists(&bundle.app_path))
    {
        let installs = bundles
            .into_iter()
            .map(|bundle| {
                let display_name = bundle_display_name(&bundle);
                Ok(PreparedBundleRepair {
                    binary: read_repair_binary(&bundle).with_context(|| {
                        format!("{display_name} 首次安装缺少可执行的应用二进制")
                    })?,
                    bundle,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        for install in installs {
            let display_name = bundle_display_name(&install.bundle);
            write_prepared_bundle(&install)
                .with_context(|| format!("安装 {display_name} 应用入口失败"))?;
            if !installed_bundle_is_usable(&install.bundle) {
                anyhow::bail!("安装 {display_name} 后完整性检查未通过");
            }
        }
        return Ok(());
    }

    repair_app_bundles(options).map(|_| ())
}

#[cfg(target_os = "macos")]
fn bundle_target_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[cfg(target_os = "macos")]
pub fn repair_app_bundles(options: &InstallOptions) -> anyhow::Result<MacosBundleRepairSummary> {
    let bundles = [
        build_app_bundle(options, false),
        build_app_bundle(options, true),
    ];
    let mut summary = MacosBundleRepairSummary::default();
    let mut repairs = Vec::new();
    let mut installed_set_has_signature = false;

    for bundle in bundles {
        installed_set_has_signature |= installed_bundle_has_signature(&bundle);
        let display_name = bundle_display_name(&bundle);
        if installed_bundle_is_usable(&bundle) {
            summary.unchanged.push(display_name);
            continue;
        }
        repairs.push(bundle);
    }

    if repairs.is_empty() {
        return Ok(summary);
    }

    let damaged_names = repairs
        .iter()
        .map(bundle_display_name)
        .collect::<Vec<_>>()
        .join("、");
    if !allows_unsigned_bundle_repair(options) || installed_set_has_signature {
        anyhow::bail!(
            "检测到 {damaged_names} 缺失或损坏。为避免破坏 macOS 应用签名，未改写应用内容；请从 U-API Connect DMG 重新安装。"
        );
    }

    // 仅显式指定的非生产目录允许生成无签名入口。先把两个待修复
    // bundle 的二进制全部读入内存，避免因一个来源缺失留下半修复状态。
    let repairs = repairs
        .into_iter()
        .map(|bundle| {
            let display_name = bundle_display_name(&bundle);
            Ok(PreparedBundleRepair {
                binary: read_repair_binary(&bundle).with_context(|| {
                    format!("{display_name} 缺失或损坏，且找不到可用于安全修复的应用二进制")
                })?,
                bundle,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    for repair in repairs {
        let display_name = bundle_display_name(&repair.bundle);
        write_prepared_bundle(&repair)
            .with_context(|| format!("修复 {display_name} 开发/测试应用入口失败"))?;
        if !installed_bundle_is_usable(&repair.bundle) {
            anyhow::bail!("修复 {display_name} 后完整性检查仍未通过，请重新安装应用");
        }
        summary.repaired.push(display_name);
    }

    Ok(summary)
}

#[cfg(target_os = "macos")]
pub fn uninstall_app_bundles(options: &InstallOptions) -> anyhow::Result<()> {
    let install_root = install_root_or_default(options);
    for name in [SILENT_NAME, MANAGER_NAME] {
        let app = install_root.join(format!("{name}.app"));
        if app.exists() {
            fs::remove_dir_all(app)?;
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn install_app_bundles(_options: &InstallOptions) -> anyhow::Result<()> {
    anyhow::bail!("macOS app bundles are only supported on macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn repair_app_bundles(_options: &InstallOptions) -> anyhow::Result<MacosBundleRepairSummary> {
    anyhow::bail!("macOS app bundles are only supported on macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn uninstall_app_bundles(_options: &InstallOptions) -> anyhow::Result<()> {
    anyhow::bail!("macOS app bundles are only supported on macOS")
}

#[cfg(target_os = "macos")]
struct PreparedBundleRepair {
    bundle: MacosAppBundle,
    binary: Vec<u8>,
}

#[cfg(target_os = "macos")]
fn write_prepared_bundle(repair: &PreparedBundleRepair) -> anyhow::Result<()> {
    let bundle = &repair.bundle;
    let contents = bundle.app_path.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;

    let target_name = bundle
        .binary_target_name
        .as_deref()
        .context("应用入口缺少 sidecar 二进制名称")?;
    let target = macos.join(target_name);
    crate::settings::atomic_write(&target, &repair.binary)?;
    set_unix_mode(&target, 0o755)?;

    let info_plist = contents.join("Info.plist");
    crate::settings::atomic_write(&info_plist, bundle.info_plist.as_bytes())?;
    set_unix_mode(&info_plist, 0o644)?;

    let executable = macos.join(executable_name_from_plist(&bundle.info_plist));
    crate::settings::atomic_write(&executable, bundle.launch_script.as_bytes())?;
    set_unix_mode(&executable, 0o755)?;
    copy_icon(&resources)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_unix_mode(path: &Path, mode: u32) -> anyhow::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn installed_bundle_is_usable(bundle: &MacosAppBundle) -> bool {
    let contents = bundle.app_path.join("Contents");
    let Ok(installed_plist) = fs::read_to_string(contents.join("Info.plist")) else {
        return false;
    };
    let expected_plist = &bundle.info_plist;
    for key in [
        "CFBundleName",
        "CFBundleDisplayName",
        "CFBundleIdentifier",
        "CFBundlePackageType",
        "CFBundleExecutable",
    ] {
        if plist_string_value(&installed_plist, key) != plist_string_value(expected_plist, key) {
            return false;
        }
    }
    if plist_bool_value(&installed_plist, "LSUIElement")
        != plist_bool_value(expected_plist, "LSUIElement")
    {
        return false;
    }
    let expects_url_scheme = expected_plist.contains("<string>uapiconnect</string>");
    if installed_plist.contains("<string>uapiconnect</string>") != expects_url_scheme {
        return false;
    }

    let executable = contents
        .join("MacOS")
        .join(executable_name_from_plist(expected_plist));
    let Ok(executable_bytes) = fs::read(&executable) else {
        return false;
    };
    if !path_is_executable(&executable) {
        return false;
    }
    if executable_bytes.starts_with(b"#!") {
        let Some(target_name) = bundle.binary_target_name.as_deref() else {
            return false;
        };
        let sidecar = contents.join("MacOS").join(target_name);
        executable_bytes == bundle.launch_script.as_bytes()
            && path_is_executable(&sidecar)
            && fs::read(sidecar)
                .ok()
                .is_some_and(|bytes| looks_like_macho_binary(&bytes))
    } else {
        looks_like_macho_binary(&executable_bytes)
    }
}

#[cfg(target_os = "macos")]
fn installed_bundle_has_signature(bundle: &MacosAppBundle) -> bool {
    bundle
        .app_path
        .join("Contents/_CodeSignature/CodeResources")
        .is_file()
}

#[cfg(target_os = "macos")]
fn allows_unsigned_bundle_repair(options: &InstallOptions) -> bool {
    let Some(root) = options.install_root.as_deref() else {
        return false;
    };
    !is_standard_applications_root(root)
}

#[cfg(target_os = "macos")]
fn is_standard_applications_root(root: &Path) -> bool {
    if paths_resolve_to_same_location(root, Path::new("/Applications")) {
        return true;
    }
    directories::BaseDirs::new().is_some_and(|dirs| {
        paths_resolve_to_same_location(root, &dirs.home_dir().join("Applications"))
    })
}

#[cfg(target_os = "macos")]
fn paths_resolve_to_same_location(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(target_os = "macos")]
fn read_repair_binary(bundle: &MacosAppBundle) -> anyhow::Result<Vec<u8>> {
    let contents = bundle.app_path.join("Contents");
    let sidecar = bundle
        .binary_target_name
        .as_deref()
        .map(|name| contents.join("MacOS").join(name));
    let executable = contents
        .join("MacOS")
        .join(executable_name_from_plist(&bundle.info_plist));
    let candidates = [
        sidecar.as_ref(),
        bundle.binary_source.as_ref(),
        Some(&executable),
    ];
    for candidate in candidates.into_iter().flatten() {
        let Ok(bytes) = fs::read(candidate) else {
            continue;
        };
        if path_is_executable(candidate) && looks_like_macho_binary(&bytes) {
            return Ok(bytes);
        }
    }
    anyhow::bail!("没有找到完整且可执行的 Mach-O 二进制")
}

#[cfg(target_os = "macos")]
fn path_is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .is_some_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(target_os = "macos")]
fn looks_like_macho_binary(bytes: &[u8]) -> bool {
    let Some(magic) = bytes.get(..4) else {
        return false;
    };
    matches!(
        magic,
        [0xce, 0xfa, 0xed, 0xfe]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xce]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    )
}

fn plist_string_value<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let tail = plist.split_once(&format!("<key>{key}</key>"))?.1;
    let tail = tail.split_once("<string>")?.1;
    tail.split_once("</string>").map(|(value, _)| value.trim())
}

fn plist_bool_value(plist: &str, key: &str) -> Option<bool> {
    let tail = plist
        .split_once(&format!("<key>{key}</key>"))?
        .1
        .trim_start();
    if tail.starts_with("<true/>") {
        Some(true)
    } else if tail.starts_with("<false/>") {
        Some(false)
    } else {
        None
    }
}

fn bundle_display_name(bundle: &MacosAppBundle) -> String {
    bundle
        .app_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("macOS 应用")
        .to_string()
}

#[cfg(target_os = "macos")]
fn copy_icon(resources: &Path) -> anyhow::Result<()> {
    let source = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|path| path.join("codex-plus-plus.png"));
    if let Some(source) = source.filter(|path| path.exists()) {
        fs::copy(source, resources.join("codex-plus-plus.png"))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn executable_name_from_plist(plist: &str) -> String {
    plist
        .split("<key>CFBundleExecutable</key>")
        .nth(1)
        .and_then(|tail| tail.split("<string>").nth(1))
        .and_then(|tail| tail.split("</string>").next())
        .unwrap_or("CodexPlusPlus")
        .to_string()
}

fn info_plist(display_name: &str, executable_name: &str, bundle_id: &str, manager: bool) -> String {
    let version = crate::version::VERSION;
    let lsui_element = if manager { "false" } else { "true" };
    let url_types = if manager {
        r#"  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key>
      <string>U-API Connect Links</string>
      <key>CFBundleURLSchemes</key>
      <array>
        <string>uapiconnect</string>
      </array>
    </dict>
  </array>
"#
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>{display_name}</string>
  <key>CFBundleDisplayName</key>
  <string>{display_name}</string>
  <key>CFBundleIdentifier</key>
  <string>{bundle_id}</string>
  <key>CFBundleVersion</key>
  <string>{version}</string>
  <key>CFBundleShortVersionString</key>
  <string>{version}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleExecutable</key>
  <string>{executable_name}</string>
  <key>CFBundleIconFile</key>
  <string>codex-plus-plus.png</string>
{url_types}  <key>LSUIElement</key>
  <{lsui_element}/>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
</dict>
</plist>"#
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn unsigned_runtime_repair_requires_an_explicit_non_applications_root() {
        assert!(!allows_unsigned_bundle_repair(&InstallOptions::default()));
        assert!(!allows_unsigned_bundle_repair(&InstallOptions {
            install_root: Some("/Applications".into()),
            ..InstallOptions::default()
        }));
        assert!(allows_unsigned_bundle_repair(&InstallOptions {
            install_root: Some("/tmp/uapi-connect-unsigned-test-apps".into()),
            ..InstallOptions::default()
        }));
    }
}
