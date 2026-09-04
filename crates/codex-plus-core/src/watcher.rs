use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "macos"))]
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Context;

#[cfg(windows)]
pub use crate::windows_integration::WindowsProcessInfo;

pub const WATCHER_INTERVAL_SECONDS: f64 = 3.0;
pub const CDP_PROBE_TIMEOUT_SECONDS: f64 = 0.5;
pub const TAKEOVER_FAILURE_BACKOFF_SECONDS: f64 = 30.0;
pub const RESTART_STOP_WAIT_TIMEOUT_MS: u64 = 5_000;
const RESTART_STOP_WAIT_INTERVAL_MS: u64 = 100;
pub const WATCHER_RUN_NAME: &str = "CodexPlusPlusWatcher";
pub const WATCHER_RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const WATCHER_STARTUP_SHORTCUT_NAME: &str = "CodexPlusPlusWatcher.lnk";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatcherInstallPlan {
    pub run_value_name: String,
    pub run_value: String,
    pub shortcut_name: String,
    pub shortcut_target: String,
    pub shortcut_arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherProcessInfo {
    pub process_id: u32,
    pub parent_process_id: u32,
    pub executable_name: String,
    pub executable_path: Option<PathBuf>,
}

#[cfg(any(windows, target_os = "macos"))]
struct OwnedLauncherSelection {
    expected_launcher_path: PathBuf,
    process_ids: Vec<u32>,
}

pub fn watcher_disabled_flag(root: &Path) -> PathBuf {
    root.join("watcher.disabled")
}

pub fn default_watcher_disabled_flag() -> PathBuf {
    watcher_disabled_flag(&crate::paths::default_app_state_dir())
}

pub fn enable_watcher_at(root: &Path) -> std::io::Result<()> {
    let flag = watcher_disabled_flag(root);
    if flag.exists() {
        std::fs::remove_file(flag)?;
    }
    Ok(())
}

pub fn disable_watcher_at(root: &Path) -> std::io::Result<()> {
    let flag = watcher_disabled_flag(root);
    if let Some(parent) = flag.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(flag, b"disabled")
}

pub fn enable_watcher() -> std::io::Result<()> {
    enable_watcher_at(&crate::paths::default_app_state_dir())
}

pub fn disable_watcher() -> std::io::Result<()> {
    disable_watcher_at(&crate::paths::default_app_state_dir())
}

pub fn cdp_listening(port: u16) -> bool {
    [
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    ]
    .into_iter()
    .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok())
}

pub fn build_spawn_launcher_command(launcher_path: &str, debug_port: u16) -> Vec<String> {
    vec![
        launcher_path.to_string(),
        "--debug-port".to_string(),
        debug_port.to_string(),
    ]
}

pub fn build_watcher_install_plan(launcher_path: PathBuf, debug_port: u16) -> WatcherInstallPlan {
    let launcher = launcher_path.to_string_lossy().to_string();
    let arguments = format!("--debug-port {debug_port}");
    WatcherInstallPlan {
        run_value_name: WATCHER_RUN_NAME.to_string(),
        run_value: format!("\"{launcher}\" {arguments}"),
        shortcut_name: WATCHER_STARTUP_SHORTCUT_NAME.to_string(),
        shortcut_target: launcher,
        shortcut_arguments: arguments,
    }
}

pub fn codex_process_ids<'a>(processes: impl IntoIterator<Item = (u32, &'a str)>) -> Vec<u32> {
    processes
        .into_iter()
        .filter_map(|(process_id, executable)| {
            is_windowsapps_codex_app_process(executable).then_some(process_id)
        })
        .collect()
}

fn is_windowsapps_codex_app_process(executable: &str) -> bool {
    let executable = executable.replace('/', "\\").to_ascii_lowercase();
    let Some((_, after_windows_apps)) = executable.split_once("\\windowsapps\\") else {
        return false;
    };
    let Some((package_name, after_package)) = after_windows_apps.split_once('\\') else {
        return false;
    };
    let supported_package = crate::app_paths::is_supported_windows_app_package_name(package_name)
        || package_name.starts_with("openai.chatgpt-desktop_");
    supported_package
        && after_package.starts_with("app\\")
        && !after_package.starts_with("app\\resources\\")
        && after_package
            .rsplit('\\')
            .next()
            .is_some_and(crate::app_paths::is_supported_app_executable_name)
}

pub fn filter_owned_launcher_processes(
    processes: &[LauncherProcessInfo],
    current_process_id: u32,
    expected_launcher_path: &Path,
) -> anyhow::Result<Vec<u32>> {
    let expected_launcher_path = canonical_launcher_path(expected_launcher_path)?;
    let parents = processes
        .iter()
        .map(|process| (process.process_id, process.parent_process_id))
        .collect::<HashMap<_, _>>();
    let mut protected = HashSet::new();
    let mut cursor = current_process_id;
    while cursor != 0 && protected.insert(cursor) {
        cursor = parents.get(&cursor).copied().unwrap_or(0);
    }
    let mut owned = Vec::new();
    for process in processes {
        if protected.contains(&process.process_id) {
            continue;
        }
        if launcher_process_matches_owned_path(process, &expected_launcher_path)? {
            owned.push(process.process_id);
        }
    }
    Ok(owned)
}

pub fn terminate_revalidated_launcher_processes<Inspect, Terminate>(
    process_ids: &[u32],
    expected_launcher_path: &Path,
    mut inspect: Inspect,
    mut terminate: Terminate,
) -> anyhow::Result<()>
where
    Inspect: FnMut(u32) -> anyhow::Result<Option<LauncherProcessInfo>>,
    Terminate: FnMut(u32) -> anyhow::Result<()>,
{
    let expected_launcher_path = canonical_launcher_path(expected_launcher_path)?;
    for process_id in process_ids {
        let Some(process) = inspect(*process_id)
            .with_context(|| format!("终止前无法重新确认 launcher 进程 {process_id}"))?
        else {
            // 首次枚举后自行退出，不需要再执行 kill。
            continue;
        };
        if process.process_id != *process_id
            || !launcher_process_matches_owned_path(&process, &expected_launcher_path)?
        {
            anyhow::bail!(
                "launcher 进程 {process_id} 的身份已变化，不再属于当前发行版，已拒绝终止"
            );
        }
        terminate(*process_id)
            .with_context(|| format!("无法停止当前发行版 launcher 进程 {process_id}"))?;
    }
    Ok(())
}

fn launcher_process_matches_owned_path(
    process: &LauncherProcessInfo,
    expected_launcher_path: &Path,
) -> anyhow::Result<bool> {
    let expected_name = expected_launcher_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("当前发行版启动器路径缺少有效文件名")?;
    if !launcher_executable_names_equal(&process.executable_name, expected_name) {
        return Ok(false);
    }
    let executable_path = process.executable_path.as_deref().with_context(|| {
        format!(
            "无法确认同名启动器进程 {} 的可执行路径，已停止操作",
            process.process_id
        )
    })?;
    let executable_path = canonical_launcher_path(executable_path).with_context(|| {
        format!(
            "无法规范化同名启动器进程 {} 的可执行路径，已停止操作",
            process.process_id
        )
    })?;
    Ok(launcher_paths_equal(
        &executable_path,
        expected_launcher_path,
    ))
}

fn canonical_launcher_path(path: &Path) -> anyhow::Result<PathBuf> {
    std::fs::canonicalize(path)
        .with_context(|| format!("无法确认启动器可执行文件 {}", path.to_string_lossy()))
}

#[cfg(windows)]
fn launcher_executable_names_equal(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(not(windows))]
fn launcher_executable_names_equal(left: &str, right: &str) -> bool {
    left == right
}

#[cfg(windows)]
fn launcher_paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn launcher_paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

pub fn should_recover_stale_launcher(has_codex_process: bool, cdp_listening: bool) -> bool {
    !has_codex_process && !cdp_listening
}

pub fn process_ids_still_running(
    expected: &[u32],
    running: impl IntoIterator<Item = u32>,
) -> Vec<u32> {
    let expected = expected.iter().copied().collect::<HashSet<_>>();
    running
        .into_iter()
        .filter(|process_id| expected.contains(process_id))
        .collect()
}

pub fn macos_launcher_process_names() -> [&'static str; 2] {
    [
        crate::install::SILENT_BINARY,
        crate::install::MACOS_SILENT_EXECUTABLE,
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessInstanceState {
    NotRunning,
    Running {
        started_at_secs: Option<u64>,
        birth_id: Option<String>,
    },
    Unknown,
}

#[cfg(windows)]
pub fn inspect_process_instance(process_id: u32) -> ProcessInstanceState {
    if process_id == 0 {
        return ProcessInstanceState::NotRunning;
    }
    let processes = crate::windows_integration::enumerate_processes();
    if processes.is_empty() {
        return ProcessInstanceState::Unknown;
    }
    if !processes
        .iter()
        .any(|process| process.process_id == process_id)
    {
        return ProcessInstanceState::NotRunning;
    }
    let birth_id = crate::windows_integration::process_birth_id(process_id);
    ProcessInstanceState::Running {
        started_at_secs: birth_id
            .and_then(crate::windows_integration::process_started_at_secs_from_birth_id),
        birth_id: birth_id.map(|birth_id| birth_id.to_string()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn inspect_process_instance(process_id: u32) -> ProcessInstanceState {
    match process_id_is_running(process_id) {
        Some(false) => ProcessInstanceState::NotRunning,
        Some(true) => {
            let (started_at_secs, birth_id) = unix_process_identity(process_id);
            ProcessInstanceState::Running {
                started_at_secs,
                birth_id,
            }
        }
        None => ProcessInstanceState::Unknown,
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn inspect_process_instance(process_id: u32) -> ProcessInstanceState {
    if process_id == 0 {
        ProcessInstanceState::NotRunning
    } else {
        ProcessInstanceState::Unknown
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unix_process_identity(process_id: u32) -> (Option<u64>, Option<String>) {
    let process_id_arg = process_id.to_string();
    let output = std::process::Command::new("ps")
        .args([
            "-p",
            process_id_arg.as_str(),
            "-o",
            "etime=",
            "-o",
            "lstart=",
        ])
        .env("LC_ALL", "C")
        .output();
    let Ok(output) = output else {
        return (None, None);
    };
    if !output.status.success() {
        return (None, None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    let Some(split_at) = text.find(char::is_whitespace) else {
        return (None, None);
    };
    let elapsed = parse_ps_elapsed_seconds(&text[..split_at]);
    let birth_id = text[split_at..].trim();
    let started_at_secs = elapsed.and_then(|elapsed| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|now| now.as_secs().saturating_sub(elapsed))
    });
    (
        started_at_secs,
        (!birth_id.is_empty()).then(|| birth_id.to_string()),
    )
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn parse_ps_elapsed_seconds(value: &str) -> Option<u64> {
    let (days, time) = if let Some((days, time)) = value.split_once('-') {
        (days.parse().ok()?, time)
    } else {
        (0, value)
    };
    let parts = time
        .split(':')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    Some(days * 86_400 + hours * 3_600 + minutes * 60 + seconds)
}

#[cfg(test)]
mod process_identity_tests {
    use super::*;

    #[test]
    fn parses_ps_elapsed_time_formats() {
        assert_eq!(parse_ps_elapsed_seconds("03:04"), Some(184));
        assert_eq!(parse_ps_elapsed_seconds("02:03:04"), Some(7_384));
        assert_eq!(parse_ps_elapsed_seconds("2-02:03:04"), Some(180_184));
        assert_eq!(parse_ps_elapsed_seconds("invalid"), None);
    }

    #[cfg(windows)]
    #[test]
    fn current_windows_process_has_a_stable_birth_identity() {
        let ProcessInstanceState::Running {
            started_at_secs,
            birth_id,
        } = inspect_process_instance(std::process::id())
        else {
            panic!("current process should be visible");
        };

        assert!(started_at_secs.is_some());
        assert!(birth_id.is_some());
    }
}

#[cfg(windows)]
pub fn process_id_is_running(process_id: u32) -> Option<bool> {
    match inspect_process_instance(process_id) {
        ProcessInstanceState::NotRunning => Some(false),
        ProcessInstanceState::Running { .. } => Some(true),
        ProcessInstanceState::Unknown => None,
    }
}

#[cfg(target_os = "linux")]
pub fn process_id_is_running(process_id: u32) -> Option<bool> {
    if process_id == 0 {
        return Some(false);
    }
    match std::fs::metadata(Path::new("/proc").join(process_id.to_string())) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

#[cfg(target_os = "macos")]
pub fn process_id_is_running(process_id: u32) -> Option<bool> {
    if process_id == 0 {
        return Some(false);
    }
    let process_id_arg = process_id.to_string();
    let output = Command::new("ps")
        .args(["-p", process_id_arg.as_str(), "-o", "pid="])
        .output()
        .ok()?;
    if !output.status.success() {
        return match output.status.code() {
            Some(1) => Some(false),
            _ => None,
        };
    }
    let process_ids = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().parse::<u32>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(process_ids.contains(&process_id))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn process_id_is_running(_process_id: u32) -> Option<bool> {
    None
}

#[cfg(windows)]
pub fn install_watcher(launcher_path: &Path, debug_port: u16) -> anyhow::Result<()> {
    let plan = build_watcher_install_plan(launcher_path.to_path_buf(), debug_port);
    crate::windows_integration::set_current_user_string_value(
        WATCHER_RUN_KEY,
        &plan.run_value_name,
        &plan.run_value,
    )?;
    create_startup_shortcut(launcher_path, &plan.shortcut_arguments)?;
    spawn_launcher(launcher_path, debug_port);
    Ok(())
}

#[cfg(not(windows))]
pub fn install_watcher(_launcher_path: &Path, _debug_port: u16) -> anyhow::Result<()> {
    anyhow::bail!("watcher install is only supported on Windows")
}

#[cfg(windows)]
pub fn uninstall_watcher() -> anyhow::Result<()> {
    stop_launcher_processes_and_wait()?;
    let _ =
        crate::windows_integration::delete_current_user_value(WATCHER_RUN_KEY, WATCHER_RUN_NAME);
    if let Some(shortcut) = startup_shortcut_path() {
        let _ = std::fs::remove_file(shortcut);
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn uninstall_watcher() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn find_codex_processes() -> Vec<u32> {
    let processes: Vec<_> = crate::windows_integration::enumerate_processes()
        .into_iter()
        .filter(|process| crate::app_paths::is_supported_app_executable_name(&process.exe_file))
        .collect();
    find_codex_processes_from_snapshot(&processes)
}

/// Filter the list of already enumerated Windows processes for Codex processes.
/// Exposed so the Windows-specific logic can be unit-tested without scanning the live system.
#[cfg(windows)]
pub fn find_codex_processes_from_snapshot(
    processes: &[crate::windows_integration::WindowsProcessInfo],
) -> Vec<u32> {
    let mut ids = codex_process_ids(
        processes
            .iter()
            .filter_map(|process| {
                process
                    .executable_path
                    .as_deref()
                    .map(|path| (process.process_id, path.to_string_lossy().to_string()))
            })
            .collect::<Vec<_>>()
            .iter()
            .map(|(pid, path)| (*pid, path.as_str())),
    );

    // Local/portable installs use Codex.exe as the Electron main process. Do not match
    // lowercase codex.exe here; that is commonly the CLI binary. ChatGPT.exe is accepted
    // only for packaged Store apps above, because the standalone ChatGPT app can be a
    // normal ChatGPT session rather than Codex.
    for process in processes {
        if process.exe_file == "Codex.exe" {
            ids.push(process.process_id);
        }
    }

    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Return desktop processes that can write Codex task state while a destructive
/// session-index cleanup is running. This is intentionally stricter than the
/// watcher filter: any supported ChatGPT desktop process blocks deletion,
/// including portable installs outside WindowsApps.
#[cfg(windows)]
pub fn find_session_index_cleanup_blocking_processes() -> Vec<u32> {
    find_session_index_cleanup_blocking_processes_from_snapshot(
        &crate::windows_integration::enumerate_processes(),
    )
}

#[cfg(windows)]
pub fn find_session_index_cleanup_blocking_processes_from_snapshot(
    processes: &[crate::windows_integration::WindowsProcessInfo],
) -> Vec<u32> {
    let mut ids = processes
        .iter()
        .filter(|process| process.exe_file == "Codex.exe" || process.exe_file == "ChatGPT.exe")
        .map(|process| process.process_id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[cfg(target_os = "macos")]
pub fn find_codex_processes() -> Vec<u32> {
    let mut ids = ["Codex", "ChatGPT"]
        .into_iter()
        .flat_map(|name| {
            std::process::Command::new("pgrep")
                .args(["-x", name])
                .output()
                .ok()
                .into_iter()
                .flat_map(|output| {
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
        })
        .filter_map(|value| value.trim().parse::<u32>().ok())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[cfg(target_os = "macos")]
pub fn find_session_index_cleanup_blocking_processes() -> Vec<u32> {
    find_codex_processes()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn find_codex_processes() -> Vec<u32> {
    Vec::new()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn find_session_index_cleanup_blocking_processes() -> Vec<u32> {
    Vec::new()
}

#[cfg(windows)]
pub fn stop_launcher_processes() -> anyhow::Result<()> {
    let killable = find_owned_launcher_processes()?;
    terminate_windows_launcher_processes(&killable)
}

#[cfg(target_os = "macos")]
pub fn stop_launcher_processes() -> anyhow::Result<()> {
    let killable = find_owned_launcher_processes()?;
    terminate_macos_launcher_processes(&killable)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn stop_launcher_processes() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn stop_launcher_processes_and_wait() -> anyhow::Result<()> {
    let killable = find_owned_launcher_processes()?;
    terminate_windows_launcher_processes(&killable)?;
    wait_for_windows_process_exit(
        &killable.process_ids,
        RESTART_STOP_WAIT_TIMEOUT_MS,
        RESTART_STOP_WAIT_INTERVAL_MS,
    )
}

#[cfg(target_os = "macos")]
pub fn stop_launcher_processes_and_wait() -> anyhow::Result<()> {
    let killable = find_owned_launcher_processes()?;
    terminate_macos_launcher_processes(&killable)?;
    wait_for_macos_process_exit(
        &killable.process_ids,
        RESTART_STOP_WAIT_TIMEOUT_MS,
        RESTART_STOP_WAIT_INTERVAL_MS,
    )
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn stop_launcher_processes_and_wait() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn stop_codex_processes() {
    for process_id in find_codex_processes() {
        let _ = crate::windows_integration::terminate_process(process_id);
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn stop_codex_processes() {}

#[cfg(target_os = "macos")]
pub fn stop_codex_processes() {
    for process_id in find_codex_processes() {
        let _ = terminate_macos_process(process_id);
    }
}

#[cfg(windows)]
pub fn stop_codex_processes_and_wait() {
    terminate_and_wait_for_exit(
        find_codex_processes(),
        RESTART_STOP_WAIT_TIMEOUT_MS,
        RESTART_STOP_WAIT_INTERVAL_MS,
    );
}

#[cfg(target_os = "macos")]
pub fn stop_codex_processes_and_wait() {
    terminate_macos_processes_and_wait(
        find_codex_processes(),
        || find_codex_processes(),
        RESTART_STOP_WAIT_TIMEOUT_MS,
        RESTART_STOP_WAIT_INTERVAL_MS,
    );
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn stop_codex_processes_and_wait() {}

#[cfg(target_os = "macos")]
pub fn stop_codex_processes_for_debug_port_and_wait(debug_port: u16) {
    terminate_macos_processes_and_wait(
        find_macos_codex_processes_for_debug_port(debug_port),
        || find_macos_codex_processes_for_debug_port(debug_port),
        RESTART_STOP_WAIT_TIMEOUT_MS,
        RESTART_STOP_WAIT_INTERVAL_MS,
    );
}

#[cfg(not(target_os = "macos"))]
pub fn stop_codex_processes_for_debug_port_and_wait(_debug_port: u16) {
    stop_codex_processes_and_wait();
}

#[cfg(target_os = "macos")]
fn terminate_macos_processes_and_wait<F>(
    process_ids: Vec<u32>,
    mut find_processes: F,
    timeout_ms: u64,
    interval_ms: u64,
) where
    F: FnMut() -> Vec<u32>,
{
    if process_ids.is_empty() {
        return;
    }
    for process_id in &process_ids {
        let _ = terminate_macos_process(*process_id);
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = process_ids_still_running(&process_ids, find_processes());
        if remaining.is_empty() || std::time::Instant::now() >= deadline {
            if !remaining.is_empty() {
                let _ = crate::diagnostic_log::append_diagnostic_log(
                    "watcher.stop_wait_timeout",
                    serde_json::json!({
                        "remaining_process_ids": remaining,
                        "timeout_ms": timeout_ms,
                        "platform": "macos"
                    }),
                );
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

#[cfg(target_os = "macos")]
fn terminate_macos_process(process_id: u32) -> std::io::Result<()> {
    let status = Command::new("kill")
        .arg(process_id.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "kill exited with status {status} for process {process_id}"
        )))
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn current_companion_launcher_path() -> anyhow::Result<PathBuf> {
    let current_exe = std::env::current_exe().context("无法确认当前程序的可执行路径")?;
    let launcher =
        crate::install::companion_binary_path_from_exe(&current_exe, crate::install::SILENT_BINARY);
    canonical_launcher_path(&launcher).with_context(|| {
        format!(
            "无法确认当前发行版 companion launcher {}",
            launcher.to_string_lossy()
        )
    })
}

#[cfg(windows)]
fn find_owned_launcher_processes() -> anyhow::Result<OwnedLauncherSelection> {
    let expected_launcher_path = current_companion_launcher_path()?;
    let processes = crate::windows_integration::enumerate_processes();
    let current_process_id = std::process::id();
    if !processes
        .iter()
        .any(|process| process.process_id == current_process_id)
    {
        anyhow::bail!("无法读取完整的 Windows 进程列表，已停止 launcher 操作");
    }
    let processes = processes
        .into_iter()
        .map(|process| LauncherProcessInfo {
            process_id: process.process_id,
            parent_process_id: process.parent_process_id,
            executable_name: process.exe_file,
            executable_path: process.executable_path,
        })
        .collect::<Vec<_>>();
    let process_ids =
        filter_owned_launcher_processes(&processes, current_process_id, &expected_launcher_path)?;
    Ok(OwnedLauncherSelection {
        expected_launcher_path,
        process_ids,
    })
}

#[cfg(target_os = "macos")]
fn find_owned_launcher_processes() -> anyhow::Result<OwnedLauncherSelection> {
    let expected_launcher_path = current_companion_launcher_path()?;
    let processes = enumerate_macos_processes()?;
    let current_process_id = std::process::id();
    if !processes
        .iter()
        .any(|process| process.process_id == current_process_id)
    {
        anyhow::bail!("无法读取完整的 macOS 进程列表，已停止 launcher 操作");
    }
    let process_ids =
        filter_owned_launcher_processes(&processes, current_process_id, &expected_launcher_path)?;
    Ok(OwnedLauncherSelection {
        expected_launcher_path,
        process_ids,
    })
}

#[cfg(target_os = "macos")]
fn enumerate_macos_processes() -> anyhow::Result<Vec<LauncherProcessInfo>> {
    let output = Command::new("ps")
        .args(["-axww", "-o", "pid=,ppid=,comm="])
        .output()
        .context("无法读取 macOS 进程列表")?;
    if !output.status.success() {
        anyhow::bail!("ps 无法读取 macOS 进程列表：{}", output.status);
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_macos_process_line)
        .collect()
}

#[cfg(target_os = "macos")]
fn parse_macos_process_line(line: &str) -> anyhow::Result<LauncherProcessInfo> {
    let (process_id, rest) = take_process_field(line).context("macOS 进程行缺少 PID")?;
    let (parent_process_id, executable_path) =
        take_process_field(rest).context("macOS 进程行缺少 PPID")?;
    let executable_path = executable_path.trim();
    if executable_path.is_empty() {
        anyhow::bail!("macOS 进程行缺少可执行路径");
    }
    let executable_path = PathBuf::from(executable_path);
    let executable_name = executable_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("macOS 进程行包含无效可执行文件名")?
        .to_string();
    Ok(LauncherProcessInfo {
        process_id: process_id.parse().context("macOS 进程 PID 无效")?,
        parent_process_id: parent_process_id.parse().context("macOS 进程 PPID 无效")?,
        executable_name,
        executable_path: Some(executable_path),
    })
}

#[cfg(target_os = "macos")]
fn take_process_field(value: &str) -> Option<(&str, &str)> {
    let value = value.trim_start();
    let end = value.find(char::is_whitespace)?;
    Some((&value[..end], value[end..].trim_start()))
}

#[cfg(windows)]
fn inspect_windows_launcher_process(
    process_id: u32,
) -> anyhow::Result<Option<LauncherProcessInfo>> {
    let processes = crate::windows_integration::enumerate_processes();
    if !processes
        .iter()
        .any(|process| process.process_id == std::process::id())
    {
        anyhow::bail!("终止前无法读取完整的 Windows 进程列表");
    }
    Ok(processes
        .into_iter()
        .find(|process| process.process_id == process_id)
        .map(|process| LauncherProcessInfo {
            process_id: process.process_id,
            parent_process_id: process.parent_process_id,
            executable_name: process.exe_file,
            executable_path: process.executable_path,
        }))
}

#[cfg(target_os = "macos")]
fn inspect_macos_launcher_process(process_id: u32) -> anyhow::Result<Option<LauncherProcessInfo>> {
    let processes = enumerate_macos_processes()?;
    if !processes
        .iter()
        .any(|process| process.process_id == std::process::id())
    {
        anyhow::bail!("终止前无法读取完整的 macOS 进程列表");
    }
    Ok(processes
        .into_iter()
        .find(|process| process.process_id == process_id))
}

#[cfg(windows)]
fn terminate_windows_launcher_processes(selection: &OwnedLauncherSelection) -> anyhow::Result<()> {
    terminate_revalidated_launcher_processes(
        &selection.process_ids,
        &selection.expected_launcher_path,
        inspect_windows_launcher_process,
        |process_id| {
            if !crate::windows_integration::terminate_process(process_id)
                && process_id_is_running(process_id) != Some(false)
            {
                anyhow::bail!("Windows 拒绝终止进程");
            }
            Ok(())
        },
    )
}

#[cfg(target_os = "macos")]
fn terminate_macos_launcher_processes(selection: &OwnedLauncherSelection) -> anyhow::Result<()> {
    terminate_revalidated_launcher_processes(
        &selection.process_ids,
        &selection.expected_launcher_path,
        inspect_macos_launcher_process,
        |process_id| match terminate_macos_process(process_id) {
            Ok(()) => Ok(()),
            Err(_) if process_id_is_running(process_id) == Some(false) => Ok(()),
            Err(error) => Err(error.into()),
        },
    )
}

#[cfg(windows)]
fn wait_for_windows_process_exit(
    process_ids: &[u32],
    timeout_ms: u64,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let processes = crate::windows_integration::enumerate_processes();
        if !processes
            .iter()
            .any(|process| process.process_id == std::process::id())
        {
            anyhow::bail!("等待 launcher 退出时无法读取完整的 Windows 进程列表");
        }
        let remaining = process_ids_still_running(
            process_ids,
            processes.into_iter().map(|process| process.process_id),
        );
        if remaining.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            log_launcher_stop_timeout(&remaining, timeout_ms, "windows");
            anyhow::bail!("等待当前发行版 launcher 退出超时，仍在运行：{remaining:?}");
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

#[cfg(target_os = "macos")]
fn wait_for_macos_process_exit(
    process_ids: &[u32],
    timeout_ms: u64,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let mut remaining = Vec::new();
        for process_id in process_ids {
            match process_id_is_running(*process_id) {
                Some(true) => remaining.push(*process_id),
                Some(false) => {}
                None => anyhow::bail!("等待 launcher 退出时无法确认进程 {process_id} 的状态"),
            }
        }
        if remaining.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            log_launcher_stop_timeout(&remaining, timeout_ms, "macos");
            anyhow::bail!("等待当前发行版 launcher 退出超时，仍在运行：{remaining:?}");
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn log_launcher_stop_timeout(remaining: &[u32], timeout_ms: u64, platform: &str) {
    let _ = crate::diagnostic_log::append_diagnostic_log(
        "watcher.stop_wait_timeout",
        serde_json::json!({
            "remaining_process_ids": remaining,
            "timeout_ms": timeout_ms,
            "platform": platform
        }),
    );
}

#[cfg(target_os = "macos")]
fn find_macos_codex_processes_for_debug_port(debug_port: u16) -> Vec<u32> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-axo", "pid=,args="])
        .output()
    else {
        return Vec::new();
    };
    macos_codex_process_ids_for_debug_port(
        String::from_utf8_lossy(&output.stdout).lines(),
        debug_port,
    )
}

#[cfg(target_os = "macos")]
fn macos_codex_process_ids_for_debug_port<'a>(
    process_lines: impl IntoIterator<Item = &'a str>,
    debug_port: u16,
) -> Vec<u32> {
    let debug_flag = format!("remote-debugging-port={debug_port}");
    let mut ids = process_lines
        .into_iter()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (pid, args) = trimmed.split_once(char::is_whitespace)?;
            let process_id = pid.parse::<u32>().ok()?;
            let is_desktop_main = (args.contains(".app/Contents/MacOS/ChatGPT")
                || args.contains(".app/Contents/MacOS/Codex"))
                && !args.contains("/Helpers/");
            (is_desktop_main && args.contains(&debug_flag)).then_some(process_id)
        })
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[cfg(windows)]
fn terminate_and_wait_for_exit(process_ids: Vec<u32>, timeout_ms: u64, interval_ms: u64) {
    if process_ids.is_empty() {
        return;
    }
    for process_id in &process_ids {
        let _ = crate::windows_integration::terminate_process(*process_id);
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let running_process_ids = crate::windows_integration::enumerate_processes()
            .into_iter()
            .map(|process| process.process_id);
        let remaining = process_ids_still_running(&process_ids, running_process_ids);
        if remaining.is_empty() || std::time::Instant::now() >= deadline {
            if !remaining.is_empty() {
                let _ = crate::diagnostic_log::append_diagnostic_log(
                    "watcher.stop_wait_timeout",
                    serde_json::json!({
                        "remaining_process_ids": remaining,
                        "timeout_ms": timeout_ms
                    }),
                );
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

#[cfg(windows)]
fn create_startup_shortcut(launcher_path: &Path, arguments: &str) -> anyhow::Result<()> {
    let Some(shortcut_path) = startup_shortcut_path() else {
        anyhow::bail!("无法定位 Windows 启动目录")
    };
    crate::windows_integration::create_shortcut(&crate::windows_integration::ShortcutSpec {
        path: shortcut_path,
        target: launcher_path.to_path_buf(),
        arguments: arguments.to_string(),
        working_directory: launcher_path.parent().map(Path::to_path_buf),
        description: "Codex++ watcher".to_string(),
        icon: None,
        show_minimized: true,
    })
}

#[cfg(windows)]
fn spawn_launcher(launcher_path: &Path, debug_port: u16) {
    let command = build_spawn_launcher_command(&launcher_path.to_string_lossy(), debug_port);
    if let Some((exe, args)) = command.split_first() {
        let mut command = Command::new(exe);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::windows_integration::CREATE_NO_WINDOW);
        let _ = command.spawn();
    }
}

#[cfg(windows)]
fn startup_shortcut_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| {
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join(WATCHER_STARTUP_SHORTCUT_NAME)
    })
}
