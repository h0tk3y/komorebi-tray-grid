fn main() {
    // Only do Windows-specific resource embedding when targeting Windows.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Hide the console window for the release binary.
        // The /SUBSYSTEM:WINDOWS flag only affects the MSVC linker.
        let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if target_env == "msvc" {
            println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:WINDOWS");
            // Use main() as the CRT entry point (rather than WinMain).
            println!("cargo:rustc-link-arg-bins=/ENTRY:mainCRTStartup");
        } else if target_env == "gnu" {
            println!("cargo:rustc-link-arg-bins=-Wl,--subsystem,windows");
        }

        // Embed the Win32 resource file (manifest, icons, version info).
        println!("cargo:rerun-if-changed=app.rc");
        println!("cargo:rerun-if-changed=app.manifest");
        println!("cargo:rerun-if-changed=assets/app.ico");
        embed_resource::compile("app.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();

        // The app icon lives in a separate generated file (`assets/app.ico`)
        // produced by `cargo run --example gen_icon`. We embed it through a
        // tiny on-the-fly resource script so a fresh checkout still builds
        // even before the icon has been generated.
        let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR is always set by Cargo");
        let icon_path = std::path::Path::new(&manifest_dir).join("assets/app.ico");
        if icon_path.exists() {
            let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR is set during build");
            let rc_path = std::path::Path::new(&out_dir).join("app-icon.rc");
            // Forward slashes in resource scripts are accepted by both
            // `rc.exe` and `windres`, so we don't need to escape anything.
            let icon_path_str = icon_path.to_string_lossy().replace('\\', "/");
            let rc_body = format!("1 ICON \"{icon_path_str}\"\n");
            std::fs::write(&rc_path, rc_body).expect("write app-icon.rc");
            embed_resource::compile(&rc_path, embed_resource::NONE)
                .manifest_required()
                .unwrap();
        }
    }
}
