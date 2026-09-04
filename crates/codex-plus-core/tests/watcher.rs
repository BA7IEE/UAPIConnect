use codex_plus_core::watcher::{
    LauncherProcessInfo, build_spawn_launcher_command, build_watcher_install_plan, cdp_listening,
    codex_process_ids, disable_watcher_at, enable_watcher_at, filter_owned_launcher_processes,
    macos_launcher_process_names, process_id_is_running, process_ids_still_running,
    should_recover_stale_launcher, terminate_revalidated_launcher_processes, watcher_disabled_flag,
};

#[cfg(windows)]
use codex_plus_core::watcher::{
    WindowsProcessInfo, find_codex_processes_from_snapshot,
    find_session_index_cleanup_blocking_processes_from_snapshot,
};

#[test]
fn cdp_listening_returns_true_for_bound_loopback_port() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();

    assert!(cdp_listening(port));
}

#[test]
fn cdp_listening_returns_true_for_bound_ipv6_loopback_port() {
    let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    assert!(cdp_listening(port));
}

#[test]
fn cdp_listening_returns_false_for_closed_port() {
    let port = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };

    assert!(!cdp_listening(port));
}

#[test]
fn watcher_enable_and_disable_toggle_flag() {
    let dir = tempfile::tempdir().unwrap();
    let flag = watcher_disabled_flag(dir.path());

    disable_watcher_at(dir.path()).unwrap();
    assert!(flag.exists());

    enable_watcher_at(dir.path()).unwrap();
    assert!(!flag.exists());
}

#[test]
fn watcher_install_plan_registers_rust_launcher_at_logon() {
    let plan = build_watcher_install_plan("C:/Tools/codex-plus-plus.exe".into(), 9333);

    assert_eq!(plan.run_value_name, "CodexPlusPlusWatcher");
    assert_eq!(
        plan.run_value,
        "\"C:/Tools/codex-plus-plus.exe\" --debug-port 9333"
    );
    assert_eq!(plan.shortcut_name, "CodexPlusPlusWatcher.lnk");
    assert_eq!(plan.shortcut_target, "C:/Tools/codex-plus-plus.exe");
    assert_eq!(plan.shortcut_arguments, "--debug-port 9333");
}

#[test]
fn spawn_launcher_command_points_to_silent_binary_only() {
    let command = build_spawn_launcher_command("C:/Tools/codex-plus-plus.exe", 9444);

    assert_eq!(command[0], "C:/Tools/codex-plus-plus.exe");
    assert!(command.contains(&"--debug-port".to_string()));
    assert!(command.contains(&"9444".to_string()));
    assert!(!command.iter().any(|part| part.contains("manager")));
}

#[test]
fn codex_process_filter_keeps_only_windowsapps_codex_processes() {
    let processes = [
        (
            11,
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0.0_x64__abc\app\Codex.exe",
        ),
        (12, r"C:\Tools\Codex.exe"),
        (
            13,
            r"C:\Program Files\WindowsApps\Other.App_1.0.0.0_x64__abc\app\Codex.exe",
        ),
    ];

    assert_eq!(codex_process_ids(processes), vec![11]);
}

#[test]
fn codex_process_filter_keeps_chatgpt_desktop_package_processes() {
    let processes = [
        (
            21,
            r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.2026.133.0_x64__abc\app\ChatGPT.exe",
        ),
        (
            22,
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.707.3748.0_x64__abc\app\ChatGPT.exe",
        ),
        (
            23,
            r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.2026.133.0_x64__abc\app\resources\ChatGPT.exe",
        ),
        (
            24,
            r"C:\Program Files\WindowsApps\Other.ChatGPT_1.0.0.0_x64__abc\app\ChatGPT.exe",
        ),
    ];

    assert_eq!(codex_process_ids(processes), vec![21, 22]);
}

#[test]
fn launcher_process_filter_requires_the_owned_path_and_protects_ancestry() {
    let temp = tempfile::tempdir().unwrap();
    let owned_dir = temp.path().join("owned");
    let foreign_dir = temp.path().join("foreign");
    std::fs::create_dir_all(&owned_dir).unwrap();
    std::fs::create_dir_all(&foreign_dir).unwrap();
    let launcher_name = "codex-plus-plus-test";
    let owned_launcher = owned_dir.join(launcher_name);
    let foreign_launcher = foreign_dir.join(launcher_name);
    std::fs::write(&owned_launcher, b"owned").unwrap();
    std::fs::write(&foreign_launcher, b"foreign").unwrap();
    let process = |process_id, parent_process_id, executable_name: &str, executable_path| {
        LauncherProcessInfo {
            process_id,
            parent_process_id,
            executable_name: executable_name.to_string(),
            executable_path,
        }
    };
    let processes = vec![
        process(10, 0, launcher_name, Some(owned_launcher.clone())),
        process(20, 10, launcher_name, Some(owned_launcher.clone())),
        process(30, 20, launcher_name, Some(owned_launcher.clone())),
        process(40, 10, launcher_name, Some(owned_launcher.clone())),
        process(50, 10, "codex-plus-plus-manager-test", None),
        process(60, 10, launcher_name, Some(foreign_launcher)),
    ];

    assert_eq!(
        filter_owned_launcher_processes(&processes, 30, &owned_launcher).unwrap(),
        vec![40]
    );
}

#[test]
fn launcher_process_filter_fails_closed_for_an_unresolved_same_name_path() {
    let temp = tempfile::tempdir().unwrap();
    let launcher = temp.path().join("codex-plus-plus-test");
    std::fs::write(&launcher, b"owned").unwrap();
    let processes = vec![LauncherProcessInfo {
        process_id: 40,
        parent_process_id: 10,
        executable_name: "codex-plus-plus-test".to_string(),
        executable_path: None,
    }];

    let error = filter_owned_launcher_processes(&processes, 30, &launcher).unwrap_err();

    assert!(error.to_string().contains("无法确认同名启动器进程 40"));
}

#[test]
fn launcher_process_filter_fails_closed_when_the_companion_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let missing_launcher = temp.path().join("codex-plus-plus-test");

    let error = filter_owned_launcher_processes(&[], 30, &missing_launcher).unwrap_err();

    assert!(error.to_string().contains("无法确认启动器可执行文件"));
}

#[test]
fn launcher_termination_rejects_a_reused_pid_before_calling_kill() {
    let temp = tempfile::tempdir().unwrap();
    let owned_dir = temp.path().join("owned");
    let foreign_dir = temp.path().join("foreign");
    std::fs::create_dir_all(&owned_dir).unwrap();
    std::fs::create_dir_all(&foreign_dir).unwrap();
    let launcher_name = "codex-plus-plus-test";
    let owned_launcher = owned_dir.join(launcher_name);
    let foreign_launcher = foreign_dir.join(launcher_name);
    std::fs::write(&owned_launcher, b"owned").unwrap();
    std::fs::write(&foreign_launcher, b"foreign").unwrap();
    let terminated = std::cell::RefCell::new(Vec::new());

    let error = terminate_revalidated_launcher_processes(
        &[40],
        &owned_launcher,
        |_| {
            Ok(Some(LauncherProcessInfo {
                process_id: 40,
                parent_process_id: 1,
                executable_name: launcher_name.to_string(),
                executable_path: Some(foreign_launcher.clone()),
            }))
        },
        |process_id| {
            terminated.borrow_mut().push(process_id);
            Ok(())
        },
    )
    .unwrap_err();

    assert!(terminated.borrow().is_empty());
    assert!(error.to_string().contains("身份已变化"));
    assert!(error.to_string().contains("已拒绝终止"));
}

#[test]
fn launcher_termination_accepts_a_pid_that_already_exited() {
    let temp = tempfile::tempdir().unwrap();
    let launcher = temp.path().join("codex-plus-plus-test");
    std::fs::write(&launcher, b"owned").unwrap();
    let terminated = std::cell::Cell::new(false);

    terminate_revalidated_launcher_processes(
        &[40],
        &launcher,
        |_| Ok(None),
        |_| {
            terminated.set(true);
            Ok(())
        },
    )
    .unwrap();

    assert!(!terminated.get());
}

#[test]
fn macos_launcher_process_names_cover_development_and_packaged_binaries() {
    assert_eq!(
        macos_launcher_process_names(),
        ["codex-plus-plus", "CodexPlusPlus"]
    );
}

#[test]
fn stale_launcher_recovery_only_runs_when_codex_and_cdp_are_absent() {
    assert!(should_recover_stale_launcher(false, false));
    assert!(!should_recover_stale_launcher(true, false));
    assert!(!should_recover_stale_launcher(false, true));
    assert!(!should_recover_stale_launcher(true, true));
}

#[test]
fn stop_wait_tracks_only_expected_process_ids() {
    assert_eq!(
        process_ids_still_running(&[10, 20, 30], [5, 20, 40, 30]),
        vec![20, 30]
    );
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn process_liveness_distinguishes_current_and_missing_processes() {
    assert_eq!(process_id_is_running(std::process::id()), Some(true));
    assert_eq!(process_id_is_running(u32::MAX), Some(false));
}

#[cfg(windows)]
#[test]
fn find_codex_processes_finds_local_install_with_capitial_c() {
    let processes = [WindowsProcessInfo {
        process_id: 42,
        parent_process_id: 0,
        exe_file: "Codex.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"D:\360Downloads\codexapp\app\Codex.exe",
        )),
    }];

    assert_eq!(find_codex_processes_from_snapshot(&processes), vec![42]);
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_lowercase_local_cli_binary() {
    let processes = [WindowsProcessInfo {
        process_id: 43,
        parent_process_id: 0,
        exe_file: "codex.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"D:\360Downloads\codexapp\app\codex.exe",
        )),
    }];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_npm_cli_binary() {
    let processes = [WindowsProcessInfo {
        process_id: 44,
        parent_process_id: 0,
        exe_file: "codex.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe",
        )),
    }];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_packaged_resource_cli_binary() {
    let processes = [WindowsProcessInfo {
        process_id: 45,
        parent_process_id: 0,
        exe_file: "codex.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0.0_x64__abc\app\resources\codex.exe",
        )),
    }];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}

#[cfg(windows)]
#[test]
fn find_codex_processes_combines_store_and_local_installs() {
    let processes = [
        WindowsProcessInfo {
            process_id: 11,
            parent_process_id: 0,
            exe_file: "ChatGPT.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.2026.133.0_x64__abc\app\ChatGPT.exe",
            )),
        },
        WindowsProcessInfo {
            process_id: 42,
            parent_process_id: 0,
            exe_file: "Codex.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"D:\360Downloads\codexapp\app\Codex.exe",
            )),
        },
    ];

    assert_eq!(find_codex_processes_from_snapshot(&processes), vec![11, 42]);
}

#[cfg(windows)]
#[test]
fn session_index_cleanup_process_guard_blocks_desktop_apps_but_not_cli() {
    let processes = [
        WindowsProcessInfo {
            process_id: 11,
            parent_process_id: 0,
            exe_file: "ChatGPT.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_1.2026.133.0_x64__abc\app\ChatGPT.exe",
            )),
        },
        WindowsProcessInfo {
            process_id: 12,
            parent_process_id: 0,
            exe_file: "ChatGPT.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(r"D:\Portable\ChatGPT\ChatGPT.exe")),
        },
        WindowsProcessInfo {
            process_id: 13,
            parent_process_id: 0,
            exe_file: "Codex.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(r"D:\Portable\Codex\Codex.exe")),
        },
        WindowsProcessInfo {
            process_id: 14,
            parent_process_id: 0,
            exe_file: "codex.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.exe",
            )),
        },
    ];

    assert_eq!(
        find_session_index_cleanup_blocking_processes_from_snapshot(&processes),
        vec![11, 12, 13]
    );
    assert_eq!(find_codex_processes_from_snapshot(&processes), vec![11, 13]);
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_unrelated_processes() {
    let processes = [
        WindowsProcessInfo {
            process_id: 10,
            parent_process_id: 0,
            exe_file: "notepad.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(r"C:\Windows\notepad.exe")),
        },
        WindowsProcessInfo {
            process_id: 20,
            parent_process_id: 0,
            exe_file: "codex-plus-plus.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"D:\Programs\Codex++\codex-plus-plus.exe",
            )),
        },
    ];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}
