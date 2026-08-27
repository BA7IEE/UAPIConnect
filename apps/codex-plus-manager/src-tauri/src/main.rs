#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|arg| arg == "--uninstall-cleanup") {
        if !codex_plus_core::distribution::FIXED_PROVIDER_EDITION {
            eprintln!("--uninstall-cleanup is only supported by U-API Connect");
            std::process::exit(2);
        }
        match codex_plus_core::uapi::uninstall_cleanup() {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("U-API uninstall cleanup failed: {error:#}");
                std::process::exit(1);
            }
        }
    }

    let configure_requested = std::env::args().any(|arg| arg == "--configure");
    if configure_requested && codex_plus_core::distribution::FIXED_PROVIDER_EDITION {
        if let Err(error) = codex_plus_core::manager_activation::request_configure() {
            let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
                "manager.configure_activation_failed",
                serde_json::json!({ "error": error.to_string() }),
            );
        }
    }

    for arg in std::env::args() {
        if codex_plus_core::distribution::FIXED_PROVIDER_EDITION {
            continue;
        }
        if arg.starts_with("dreamskin://") {
            if codex_plus_manager_lib::handle_dream_skin_url(&arg) {
                codex_plus_manager_lib::focus_existing_manager_window();
            }
        } else if arg.starts_with("codexplusplus://session") {
            if codex_plus_manager_lib::handle_session_share_url(&arg) {
                codex_plus_manager_lib::focus_existing_manager_window();
            }
        } else if arg.starts_with("codexplusplus://") {
            match codex_plus_core::provider_import::save_pending_provider_import_from_url(&arg) {
                Ok(request) => {
                    let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
                        "manager.provider_import_url.pending",
                        serde_json::json!({
                            "name": request.name,
                            "baseUrl": request.base_url
                        }),
                    );
                    codex_plus_manager_lib::focus_existing_manager_window();
                }
                Err(error) => {
                    let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
                        "manager.provider_import_url.failed",
                        serde_json::json!({
                            "error": error.to_string()
                        }),
                    );
                }
            }
        }
    }
    if codex_plus_core::distribution::UPDATES_ENABLED
        && std::env::args().any(|arg| arg == "--show-update")
    {
        unsafe {
            std::env::set_var("CODEX_PLUS_SHOW_UPDATE", "1");
        }
    }
    if configure_requested {
        unsafe {
            std::env::set_var("UAPI_CONNECT_CONFIGURE", "1");
        }
    }
    codex_plus_manager_lib::run();
}
