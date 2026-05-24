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
        embed_resource::compile("app.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
