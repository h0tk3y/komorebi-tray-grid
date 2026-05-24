//! Per-user "Launch on Windows logon" toggle.
//!
//! The toggle is persisted as a string value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. The value name
//! identifies the application (`komorebi-tray-grid`); the value data is the
//! quoted absolute path to the running executable.
//!
//! Per-user means no admin rights required; no UAC prompt; no service install.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
use winreg::RegKey;

/// Subkey holding all the per-user logon launchers Windows knows about.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Value name used under `RUN_KEY`. Pick something stable and distinctive.
const VALUE_NAME: &str = "komorebi-tray-grid";

/// Open the `Run` subkey for read-only access.
fn run_key_read() -> Result<RegKey> {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .with_context(|| format!(r"open HKCU\{RUN_KEY} for read"))
}

/// Open (creating if necessary) the `Run` subkey for write access.
fn run_key_write() -> Result<RegKey> {
    // The Run subkey always exists on a normal Windows install, but
    // `create_subkey` is idempotent and gives us write access in one call.
    let (key, _disp) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey_with_flags(RUN_KEY, KEY_SET_VALUE | KEY_READ)
        .with_context(|| format!(r"open HKCU\{RUN_KEY} for write"))?;
    Ok(key)
}

/// `true` when an autostart entry currently exists *and* points at the
/// expected executable. We don't strictly check the path; presence of any
/// value under our name is enough for the checkmark.
pub fn is_enabled() -> bool {
    match run_key_read() {
        Ok(key) => key.get_value::<String, _>(VALUE_NAME).is_ok(),
        Err(_) => false,
    }
}

/// Write the autostart entry pointing at the currently-running executable.
pub fn enable() -> Result<()> {
    let exe = current_exe().context("query current executable path")?;
    let value = quoted_path(&exe);
    run_key_write()?
        .set_value(VALUE_NAME, &value)
        .with_context(|| format!(r"write HKCU\{RUN_KEY}\{VALUE_NAME}"))?;
    Ok(())
}

/// Remove the autostart entry, if any. Missing entries are not an error.
pub fn disable() -> Result<()> {
    let key = run_key_write()?;
    match key.delete_value(VALUE_NAME) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!(r"delete HKCU\{RUN_KEY}\{VALUE_NAME}")),
    }
}

/// Apply `enabled` and return the actually-persisted state. On failure,
/// logs the error and returns the previously-stored state (which may be
/// the opposite of what was requested).
pub fn set_enabled(enabled: bool) -> bool {
    let result = if enabled { enable() } else { disable() };
    match result {
        Ok(()) => enabled,
        Err(e) => {
            tracing::error!(error = ?e, requested = enabled, "autostart toggle failed");
            is_enabled()
        }
    }
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(Into::into)
}

/// Wrap a path in double quotes, doubling any embedded quotes so the
/// Windows shell parses the command line as a single argv[0].
fn quoted_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let escaped = s.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn quoted_path_wraps_in_quotes() {
        let p = Path::new(r"C:\Program Files\komorebi-tray-grid\komorebi-tray-grid.exe");
        assert_eq!(
            quoted_path(p),
            r#""C:\Program Files\komorebi-tray-grid\komorebi-tray-grid.exe""#
        );
    }

    #[test]
    fn quoted_path_escapes_embedded_quotes() {
        // Truly absurd in practice, but we should still produce a parseable
        // command line.
        let p = Path::new(r#"C:\weird"name.exe"#);
        assert_eq!(quoted_path(p), r#""C:\weird""name.exe""#);
    }
}
