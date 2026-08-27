#[cfg(target_os = "macos")]
use codex_plus_core::install::macos::{install_app_bundles, repair_app_bundles};
use codex_plus_core::install::windows::{
    uapiconnect_url_protocol_command, windows_uninstall_registration_values,
};
use codex_plus_core::install::{
    InstallOptions, MANAGER_BUNDLE_ID, SILENT_BINARY, SILENT_BUNDLE_ID, app_bundle_names,
    build_macos_app_bundle, build_windows_entrypoint_plan, companion_binary_path_from_exe,
    default_install_root_strategy, macos_companion_bundle_identifier_from_exe, shortcut_names,
};
#[cfg(target_os = "macos")]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[cfg(target_os = "macos")]
fn write_fake_macho(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(target_os = "macos")]
fn packaged_uapi_plist(
    display_name: &str,
    executable_name: &str,
    bundle_id: &str,
    manager: bool,
) -> String {
    let url_types = if manager {
        r#"<key>CFBundleURLTypes</key><array><dict>
<key>CFBundleURLName</key><string>U-API Connect Links</string>
<key>CFBundleURLSchemes</key><array><string>uapiconnect</string></array>
</dict></array>"#
    } else {
        ""
    };
    let lsui_element = if manager { "false" } else { "true" };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleName</key><string>{display_name}</string>
<key>CFBundleDisplayName</key><string>{display_name}</string>
<key>CFBundleIdentifier</key><string>{bundle_id}</string>
<key>CFBundleVersion</key><string>99.0.0</string>
<key>CFBundleShortVersionString</key><string>99.0.0</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleExecutable</key><string>{executable_name}</string>
<key>CFBundleIconFile</key><string>uapi-connect.icns</string>
{url_types}
<key>LSUIElement</key><{lsui_element}/>
</dict></plist>"#
    )
}

#[cfg(target_os = "macos")]
fn write_packaged_uapi_bundle(
    bundle: &codex_plus_core::install::MacosAppBundle,
    manager: bool,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let executable_name = if manager {
        "CodexPlusPlusManager"
    } else {
        "CodexPlusPlus"
    };
    let bundle_id = if manager {
        MANAGER_BUNDLE_ID
    } else {
        SILENT_BUNDLE_ID
    };
    let display_name = if manager {
        "U-API Connect 设置"
    } else {
        "U-API Connect"
    };
    let contents = bundle.app_path.join("Contents");
    let info_plist = contents.join("Info.plist");
    let executable = contents.join("MacOS").join(executable_name);
    std::fs::create_dir_all(contents.join("_CodeSignature")).unwrap();
    std::fs::write(
        &info_plist,
        packaged_uapi_plist(display_name, executable_name, bundle_id, manager),
    )
    .unwrap();
    std::fs::write(
        contents.join("_CodeSignature/CodeResources"),
        b"signed bundle marker",
    )
    .unwrap();
    write_fake_macho(&executable);
    (info_plist, executable)
}

#[test]
fn windows_entrypoint_plan_contains_silent_and_manager_entrypoints() {
    let options = InstallOptions {
        install_root: Some("C:/Users/A/Desktop".into()),
        launcher_path: Some("C:/Tools/codex-plus-plus.exe".into()),
        manager_path: Some("C:/Tools/codex-plus-plus-manager.exe".into()),
        remove_owned_data: false,
    };

    let plan = build_windows_entrypoint_plan(&options);

    let (silent_shortcut, manager_shortcut) = shortcut_names();
    assert!(plan.silent_shortcut.ends_with(silent_shortcut));
    assert!(plan.manager_shortcut.ends_with(manager_shortcut));
    assert_eq!(plan.launcher_path, "C:/Tools/codex-plus-plus.exe");
    assert_eq!(plan.manager_path, "C:/Tools/codex-plus-plus-manager.exe");
    assert_eq!(plan.silent_icon_path, "C:/Tools/codex-plus-plus.exe");
    assert_eq!(
        plan.manager_icon_path,
        "C:/Tools/codex-plus-plus-manager.exe"
    );
    assert_eq!(plan.uninstall_key, "UAPIConnect");
    assert_eq!(plan.url_protocol_key, "uapiconnect");
    assert_eq!(
        plan.uninstaller_path.replace('\\', "/"),
        "C:/Tools/uninstall.exe"
    );
    assert_eq!(
        plan.quiet_uninstall_bootstrapper_path.replace('\\', "/"),
        "C:/Tools/quiet-uninstall-bootstrap.ps1"
    );
    assert_eq!(
        plan.uninstall_command.replace('\\', "/"),
        "\"C:/Tools/uninstall.exe\""
    );
    let quiet_uninstall_command = plan.quiet_uninstall_command.replace('\\', "/");
    assert!(
        quiet_uninstall_command
            .starts_with("\"C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe\"")
    );
    assert!(quiet_uninstall_command.contains("-NoLogo -NoProfile -NonInteractive"));
    assert!(quiet_uninstall_command.contains("-ExecutionPolicy Bypass -WindowStyle Hidden"));
    assert!(
        quiet_uninstall_command
            .contains("-File \"C:/Tools/quiet-uninstall-bootstrap.ps1\" -InstallDir \"C:/Tools\"")
    );
    assert!(!quiet_uninstall_command.contains("uninstall.exe\" /S"));
    assert_ne!(
        plan.uninstall_command,
        "\"C:/Tools/codex-plus-plus-manager.exe\""
    );
}

#[test]
fn windows_entrypoint_maintenance_is_scoped_to_uapi_owned_registry_keys() {
    let source = include_str!("../src/install/windows.rs");

    assert!(!source.contains("LEGACY_UNINSTALL_SUBKEY"));
    assert!(!source.contains("DREAM_SKIN_URL_PROTOCOL_SUBKEY"));
    assert!(!source.contains(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Codex++"));
    assert!(!source.contains(r"Software\Classes\dreamskin"));
}

#[test]
fn windows_repair_only_registers_the_quiet_bootstrap_after_it_exists() {
    let temp = tempfile::tempdir().unwrap();
    let install_dir = temp.path().join("U-API Connect");
    std::fs::create_dir(&install_dir).unwrap();
    let options = InstallOptions {
        install_root: Some(temp.path().join("Desktop")),
        launcher_path: Some(install_dir.join("codex-plus-plus.exe")),
        manager_path: Some(install_dir.join("codex-plus-plus-manager.exe")),
        remove_owned_data: false,
    };
    let plan = build_windows_entrypoint_plan(&options);

    let before = windows_uninstall_registration_values(&plan);
    assert!(
        !before
            .iter()
            .any(|(name, _)| *name == "QuietUninstallString")
    );

    std::fs::write(&plan.quiet_uninstall_bootstrapper_path, b"bootstrap").unwrap();
    let after = windows_uninstall_registration_values(&plan);
    assert_eq!(
        after
            .iter()
            .find(|(name, _)| *name == "QuietUninstallString")
            .map(|(_, value)| value),
        Some(&plan.quiet_uninstall_command)
    );
}

#[test]
fn windows_url_protocol_command_quotes_the_manager_and_full_url() {
    assert_eq!(
        uapiconnect_url_protocol_command(
            "C:\\Program Files\\U-API Connect\\codex-plus-plus-manager.exe"
        ),
        "\"C:\\Program Files\\U-API Connect\\codex-plus-plus-manager.exe\" \"%1\""
    );
}

#[test]
fn windows_entrypoint_plan_can_request_owned_data_removal_without_shell_script() {
    let options = InstallOptions {
        install_root: Some("C:/Users/A/Desktop".into()),
        launcher_path: None,
        manager_path: None,
        remove_owned_data: true,
    };

    let plan = build_windows_entrypoint_plan(&options);

    let (silent_shortcut, manager_shortcut) = shortcut_names();
    assert!(plan.silent_shortcut.ends_with(silent_shortcut));
    assert!(plan.manager_shortcut.ends_with(manager_shortcut));
    assert!(plan.remove_owned_data);
}

#[test]
fn macos_bundle_metadata_contains_silent_and_manager_apps() {
    let options = InstallOptions {
        install_root: Some("/Applications".into()),
        launcher_path: Some("/opt/Codex++/codex-plus-plus".into()),
        manager_path: Some("/opt/Codex++/codex-plus-plus-manager".into()),
        remove_owned_data: false,
    };

    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);

    let (silent_app, manager_app) = app_bundle_names();
    assert!(silent.app_path.ends_with(silent_app));
    assert!(manager.app_path.ends_with(manager_app));
    assert!(silent.info_plist.contains("<string>U-API Connect</string>"));
    assert!(
        manager
            .info_plist
            .contains("<string>U-API Connect 设置</string>")
    );
    assert!(manager.info_plist.contains("<string>uapiconnect</string>"));
    assert!(!manager.info_plist.contains("<string>dreamskin</string>"));
    assert!(!silent.info_plist.contains("<string>uapiconnect</string>"));
    assert!(
        silent
            .info_plist
            .contains("<key>LSUIElement</key>\n  <true/>")
    );
    assert!(
        manager
            .info_plist
            .contains("<key>LSUIElement</key>\n  <false/>")
    );
    assert_eq!(
        silent.binary_target_name.as_deref(),
        Some("codex-plus-plus")
    );
    assert_eq!(
        manager.binary_target_name.as_deref(),
        Some("codex-plus-plus-manager")
    );
    assert!(silent.launch_script.contains("$DIR/codex-plus-plus"));
    assert!(
        manager
            .launch_script
            .contains("$DIR/codex-plus-plus-manager")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_repair_leaves_complete_signed_bundles_byte_for_byte_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let install_root = temp.path().join("signed-apps");
    let options = InstallOptions {
        install_root: Some(install_root),
        launcher_path: None,
        manager_path: None,
        remove_owned_data: false,
    };
    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);
    let (silent_info, silent_executable) = write_packaged_uapi_bundle(&silent, false);
    let (manager_info, manager_executable) = write_packaged_uapi_bundle(&manager, true);
    let tracked = [
        silent_info,
        silent_executable,
        manager_info,
        manager_executable,
    ];
    let before = tracked
        .iter()
        .map(|path| {
            (
                std::fs::read(path).unwrap(),
                std::fs::metadata(path).unwrap().ino(),
            )
        })
        .collect::<Vec<_>>();

    let summary = repair_app_bundles(&options).unwrap();

    assert!(summary.repaired.is_empty());
    assert_eq!(summary.unchanged, ["U-API Connect", "U-API Connect 设置"]);
    for (path, (bytes, inode)) in tracked.iter().zip(before) {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
    }
    assert!(
        !silent
            .app_path
            .join("Contents/MacOS/codex-plus-plus")
            .exists()
    );
    assert!(
        !manager
            .app_path
            .join("Contents/MacOS/codex-plus-plus-manager")
            .exists()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_repair_refuses_to_rewrite_a_damaged_signed_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let install_root = temp.path().join("signed-apps");
    let options = InstallOptions {
        install_root: Some(install_root),
        launcher_path: None,
        manager_path: None,
        remove_owned_data: false,
    };
    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);
    let (silent_info, silent_executable) = write_packaged_uapi_bundle(&silent, false);
    let (manager_info, manager_executable) = write_packaged_uapi_bundle(&manager, true);
    let damaged_plist = std::fs::read_to_string(&manager_info)
        .unwrap()
        .replace("uapiconnect", "broken-scheme");
    std::fs::write(&manager_info, damaged_plist).unwrap();
    let tracked = [
        silent_info,
        silent_executable,
        manager_info,
        manager_executable,
    ];
    let before = tracked
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect::<Vec<_>>();

    let error = repair_app_bundles(&options).unwrap_err().to_string();

    assert!(error.contains("为避免破坏 macOS 应用签名"));
    assert!(error.contains("未改写应用内容"));
    assert!(error.contains("U-API Connect DMG"));
    for (path, bytes) in tracked.iter().zip(before) {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert!(
        !silent
            .app_path
            .join("Contents/MacOS/codex-plus-plus")
            .exists()
    );
    assert!(
        !manager
            .app_path
            .join("Contents/MacOS/codex-plus-plus-manager")
            .exists()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_install_does_not_add_an_unsigned_companion_next_to_a_signed_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let install_root = temp.path().join("signed-apps");
    let manager_source = temp.path().join("codex-plus-plus-manager");
    write_fake_macho(&manager_source);
    let options = InstallOptions {
        install_root: Some(install_root),
        launcher_path: None,
        manager_path: Some(manager_source),
        remove_owned_data: false,
    };
    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);
    let (silent_info, silent_executable) = write_packaged_uapi_bundle(&silent, false);
    let before = [
        std::fs::read(&silent_info).unwrap(),
        std::fs::read(&silent_executable).unwrap(),
    ];

    let error = install_app_bundles(&options).unwrap_err().to_string();

    assert!(error.contains("U-API Connect 设置"));
    assert!(error.contains("U-API Connect DMG"));
    assert_eq!(std::fs::read(silent_info).unwrap(), before[0]);
    assert_eq!(std::fs::read(silent_executable).unwrap(), before[1]);
    assert!(!manager.app_path.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_install_preserves_fresh_bundle_creation_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let install_root = temp.path().join("fresh-install-root");
    let launcher_source = temp.path().join("codex-plus-plus");
    let manager_source = temp.path().join("codex-plus-plus-manager");
    write_fake_macho(&launcher_source);
    write_fake_macho(&manager_source);
    let options = InstallOptions {
        install_root: Some(install_root),
        launcher_path: Some(launcher_source),
        manager_path: Some(manager_source),
        remove_owned_data: false,
    };
    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);

    install_app_bundles(&options).unwrap();

    assert!(silent.app_path.join("Contents/Info.plist").is_file());
    assert!(
        silent
            .app_path
            .join("Contents/MacOS/CodexPlusPlus")
            .is_file()
    );
    assert!(
        manager
            .app_path
            .join("Contents/MacOS/CodexPlusPlusManager")
            .is_file()
    );
    assert!(
        manager
            .app_path
            .join("Contents/MacOS/codex-plus-plus-manager")
            .is_file()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_repair_can_build_unsigned_entrypoints_in_an_explicit_test_root() {
    let temp = tempfile::tempdir().unwrap();
    let install_root = temp.path().join("unsigned-test-apps");
    let sources = temp.path().join("sources");
    let launcher_source = sources.join("codex-plus-plus");
    let manager_source = sources.join("codex-plus-plus-manager");
    write_fake_macho(&launcher_source);
    write_fake_macho(&manager_source);
    let options = InstallOptions {
        install_root: Some(install_root),
        launcher_path: Some(launcher_source.clone()),
        manager_path: Some(manager_source.clone()),
        remove_owned_data: false,
    };
    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);

    let first = repair_app_bundles(&options).unwrap();

    assert_eq!(first.repaired, ["U-API Connect", "U-API Connect 设置"]);
    assert!(first.unchanged.is_empty());
    let silent_executable = silent.app_path.join("Contents/MacOS/CodexPlusPlus");
    let manager_executable = manager.app_path.join("Contents/MacOS/CodexPlusPlusManager");
    assert_eq!(
        std::fs::read(silent.app_path.join("Contents/MacOS/codex-plus-plus")).unwrap(),
        std::fs::read(launcher_source).unwrap()
    );
    assert_eq!(
        std::fs::read(
            manager
                .app_path
                .join("Contents/MacOS/codex-plus-plus-manager")
        )
        .unwrap(),
        std::fs::read(manager_source).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&silent_executable).unwrap(),
        silent.launch_script
    );
    assert_eq!(
        std::fs::read_to_string(&manager_executable).unwrap(),
        manager.launch_script
    );
    assert!(
        std::fs::read_to_string(silent.app_path.join("Contents/Info.plist"))
            .unwrap()
            .contains("<key>LSUIElement</key>\n  <true/>")
    );
    let manager_plist =
        std::fs::read_to_string(manager.app_path.join("Contents/Info.plist")).unwrap();
    assert!(manager_plist.contains("<key>LSUIElement</key>\n  <false/>"));
    assert!(manager_plist.contains("<string>uapiconnect</string>"));

    let tracked = [silent_executable, manager_executable];
    let before = tracked
        .iter()
        .map(|path| {
            (
                std::fs::read(path).unwrap(),
                std::fs::metadata(path).unwrap().ino(),
            )
        })
        .collect::<Vec<_>>();
    let second = repair_app_bundles(&options).unwrap();
    assert!(second.repaired.is_empty());
    assert_eq!(second.unchanged, ["U-API Connect", "U-API Connect 设置"]);
    for (path, (bytes, inode)) in tracked.iter().zip(before) {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_repair_preflights_all_unsigned_sources_before_writing() {
    let temp = tempfile::tempdir().unwrap();
    let install_root = temp.path().join("unsigned-test-apps");
    let manager_source = temp.path().join("codex-plus-plus-manager");
    write_fake_macho(&manager_source);
    let options = InstallOptions {
        install_root: Some(install_root.clone()),
        launcher_path: Some(temp.path().join("missing-launcher")),
        manager_path: Some(manager_source),
        remove_owned_data: false,
    };

    let error = repair_app_bundles(&options).unwrap_err().to_string();

    assert!(error.contains("找不到可用于安全修复的应用二进制"));
    assert!(!install_root.join("U-API Connect.app").exists());
    assert!(!install_root.join("U-API Connect 设置.app").exists());
}

#[test]
fn installer_exports_expected_two_entrypoint_names() {
    assert_eq!(
        shortcut_names(),
        ("U-API Connect.lnk", "U-API Connect 设置.lnk")
    );
    assert_eq!(
        app_bundle_names(),
        ("U-API Connect.app", "U-API Connect 设置.app")
    );
}

#[test]
fn macos_dmg_includes_applications_shortcut_for_drag_install() {
    let script = std::fs::read_to_string("../../scripts/installer/macos/package-dmg.sh")
        .expect("read macOS DMG packaging script");

    assert!(script.contains("ln -s /Applications \"$STAGE/Applications\""));
}

#[test]
fn companion_binary_path_resolves_macos_silent_app_next_to_manager_app() {
    let temp = tempfile::tempdir().unwrap();
    let applications = temp.path().join("Applications");
    let manager_exe = applications
        .join("U-API Connect 设置.app")
        .join("Contents/MacOS/CodexPlusPlusManager");

    let companion = companion_binary_path_from_exe(&manager_exe, SILENT_BINARY);

    assert_eq!(
        companion,
        applications
            .join("U-API Connect.app")
            .join("Contents/MacOS/CodexPlusPlus")
    );
    assert_ne!(
        companion,
        std::path::PathBuf::from(
            "/Applications/Codex++ 管理工具.app/Contents/MacOS/codex-plus-plus"
        )
    );
}

#[test]
fn companion_binary_path_resolves_macos_manager_app_next_to_silent_app() {
    let temp = tempfile::tempdir().unwrap();
    let applications = temp.path().join("Applications");
    let silent_exe = applications
        .join("U-API Connect.app")
        .join("Contents/MacOS/CodexPlusPlus");

    let companion =
        companion_binary_path_from_exe(&silent_exe, codex_plus_core::install::MANAGER_BINARY);

    assert_eq!(
        companion,
        applications
            .join("U-API Connect 设置.app")
            .join("Contents/MacOS/CodexPlusPlusManager")
    );
}

#[test]
fn macos_companion_launch_uses_bundle_ids_from_app_translocation() {
    let manager_exe = std::path::Path::new(
        "/private/var/folders/x/AppTranslocation/manager-id/d/U-API Connect 设置.app/Contents/MacOS/CodexPlusPlusManager",
    );
    let silent_exe = std::path::Path::new(
        "/private/var/folders/x/AppTranslocation/silent-id/d/U-API Connect.app/Contents/MacOS/CodexPlusPlus",
    );

    assert_eq!(
        macos_companion_bundle_identifier_from_exe(manager_exe, SILENT_BINARY),
        Some(SILENT_BUNDLE_ID)
    );
    assert_eq!(
        macos_companion_bundle_identifier_from_exe(
            silent_exe,
            codex_plus_core::install::MANAGER_BINARY,
        ),
        Some(MANAGER_BUNDLE_ID)
    );
}

#[test]
fn macos_companion_launch_keeps_bare_binary_development_mode() {
    let manager_exe = std::path::Path::new("/tmp/target/debug/codex-plus-plus-manager");

    assert_eq!(
        macos_companion_bundle_identifier_from_exe(manager_exe, SILENT_BINARY),
        None
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_companion_path_falls_back_to_workspace_release_launcher() {
    let root = tempfile::tempdir().unwrap();
    let bundle_exe = root.path().join(
        "target/release/bundle/macos/Codex++ Manager.app/Contents/MacOS/codex-plus-plus-manager",
    );
    let release_launcher = root.path().join("target/release/codex-plus-plus");
    std::fs::create_dir_all(bundle_exe.parent().unwrap()).unwrap();
    std::fs::create_dir_all(release_launcher.parent().unwrap()).unwrap();
    std::fs::write(&bundle_exe, b"manager").unwrap();
    std::fs::write(&release_launcher, b"launcher").unwrap();

    assert_eq!(
        companion_binary_path_from_exe(&bundle_exe, SILENT_BINARY),
        release_launcher
    );
}

#[test]
fn macos_bundle_does_not_wrap_the_bundle_executable_in_itself() {
    let temp = tempfile::tempdir().unwrap();
    let applications = temp.path().join("Applications");
    let launcher_path = applications
        .join("U-API Connect.app")
        .join("Contents/MacOS/CodexPlusPlus");
    let manager_path = applications
        .join("U-API Connect 设置.app")
        .join("Contents/MacOS/CodexPlusPlusManager");
    let options = InstallOptions {
        install_root: Some(applications),
        launcher_path: Some(launcher_path.clone()),
        manager_path: Some(manager_path.clone()),
        remove_owned_data: false,
    };

    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);

    assert_eq!(silent.binary_source, Some(launcher_path));
    assert_eq!(manager.binary_source, Some(manager_path));
    assert!(silent.launch_script.contains("$DIR/codex-plus-plus"));
    assert!(
        manager
            .launch_script
            .contains("$DIR/codex-plus-plus-manager")
    );
}

#[test]
fn windows_default_install_root_uses_known_folder_before_userprofile_desktop() {
    let strategy = default_install_root_strategy();

    if cfg!(windows) {
        assert_eq!(strategy, "windows-known-folder");
    } else if cfg!(target_os = "macos") {
        assert_eq!(strategy, "macos-applications");
    } else {
        assert_eq!(strategy, "user-dirs-desktop");
    }
}
