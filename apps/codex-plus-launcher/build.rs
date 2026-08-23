fn main() {
    println!("cargo:rerun-if-env-changed=UAPI_CONNECT_DISTRIBUTION");
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../codex-plus-manager/src-tauri/icons/icon.ico");
        let app_manifest = if matches!(
            std::env::var("UAPI_CONNECT_DISTRIBUTION").as_deref(),
            Ok("1")
        ) {
            include_str!("../../scripts/uapi/installer/windows/UAPIConnect.exe.manifest.xml")
        } else {
            include_str!("../codex-plus-manager/src-tauri/windows-app-manifest.xml")
        };
        resource.set_manifest(app_manifest);
        resource.compile().expect("compile launcher icon resource");
    }
}
