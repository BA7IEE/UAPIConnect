fn main() {
    println!("cargo:rerun-if-env-changed=UAPI_CONNECT_DISTRIBUTION");
    let app_manifest = if matches!(
        std::env::var("UAPI_CONNECT_DISTRIBUTION").as_deref(),
        Ok("1")
    ) {
        include_str!("../../../scripts/uapi/installer/windows/UAPIConnect.exe.manifest.xml")
    } else {
        include_str!("windows-app-manifest.xml")
    };
    let windows = tauri_build::WindowsAttributes::new().app_manifest(app_manifest);
    let attrs = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attrs).expect("failed to run Tauri build script");
}
