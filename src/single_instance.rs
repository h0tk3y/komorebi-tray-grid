//! Single-instance guard backed by a named Windows mutex.
//!
//! The guard is an RAII handle: when it's dropped, the kernel automatically
//! releases the mutex and the *next* process to call [`acquire`] will succeed.
//! The mutex is unnamed within the user session (`Local\…`), which is enough
//! to prevent a second tray instance per logged-in user without requiring
//! admin rights to create a `Global\…` object.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use anyhow::{Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

/// Per-user mutex name. Using `Local\\` keeps it scoped to this user session,
/// matching the per-user nature of the tray icons we manage.
const MUTEX_NAME: &str = r"Local\komorebi-tray-grid-singleton";

/// Result of the single-instance check.
pub enum Acquisition {
    /// This process holds the lock and owns the returned [`InstanceGuard`].
    Acquired(InstanceGuard),
    /// Another instance is already running; the caller should exit silently.
    AlreadyRunning,
}

/// Try to acquire the single-instance lock.
///
/// Returns [`Acquisition::AlreadyRunning`] if a sibling instance already
/// holds the mutex; otherwise the returned [`InstanceGuard`] keeps the lock
/// for the lifetime of the process (drop it manually only on a clean exit).
pub fn acquire() -> Result<Acquisition> {
    let name_wide: Vec<u16> = OsStr::new(MUTEX_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `CreateMutexW` is documented as safe to call with a null
    // security attributes pointer and a null-terminated wide name; the
    // returned handle must be closed on drop, which we do.
    let handle = unsafe {
        CreateMutexW(
            None,                       // default security descriptor
            true,                       // initial owner
            PCWSTR(name_wide.as_ptr()), // name
        )
        .context("CreateMutexW failed")?
    };

    // `CreateMutexW` returns a valid handle even when the mutex already
    // existed; we have to consult `GetLastError` to distinguish the cases.
    // SAFETY: trivially safe — no inputs, no outputs that escape this scope.
    let last_err = unsafe { GetLastError() };
    if last_err == ERROR_ALREADY_EXISTS {
        // We still got a handle; close it so we don't leak it.
        // SAFETY: the handle was returned by `CreateMutexW` above and has
        // not been used elsewhere.
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(Acquisition::AlreadyRunning);
    }

    Ok(Acquisition::Acquired(InstanceGuard { handle }))
}

/// RAII handle for the single-instance mutex. Dropping it releases the lock.
pub struct InstanceGuard {
    handle: HANDLE,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: `self.handle` was returned by `CreateMutexW` in [`acquire`]
        // and has not been closed elsewhere.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

// `HANDLE` is not `Send` by default in the `windows` crate, but Win32
// mutex handles are safe to send between threads (closing happens on
// drop, which is single-threaded).
unsafe impl Send for InstanceGuard {}
unsafe impl Sync for InstanceGuard {}
