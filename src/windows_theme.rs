//! Windows app color-scheme detection and change notifications.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use tao::event_loop::EventLoopProxy;
use windows::core::w;
use windows::Win32::System::Registry::{
    HKEY, RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_NOTIFY,
    REG_NOTIFY_CHANGE_LAST_SET,
};
use winreg::enums::HKEY_CURRENT_USER as HKEY_CURRENT_USER_WINREG;
use winreg::RegKey;

use crate::event::UserEvent;

const PERSONALIZE_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ColorScheme {
    Dark,
    Light,
}

pub fn current_scheme() -> ColorScheme {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER_WINREG);
    let Ok(key) = hkcu.open_subkey(PERSONALIZE_PATH) else {
        return ColorScheme::Dark;
    };
    let value: u32 = key.get_value("AppsUseLightTheme").unwrap_or(0);
    if value == 0 {
        ColorScheme::Dark
    } else {
        ColorScheme::Light
    }
}

pub fn spawn_watcher(proxy: EventLoopProxy<UserEvent>) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("windows-theme-watcher".into())
        .spawn(move || {
            let mut last_scheme = current_scheme();

            loop {
                if let Err(e) = wait_for_personalize_change() {
                    tracing::warn!(error = %e, "windows theme watcher failed; retrying");
                    thread::sleep(Duration::from_millis(750));
                    continue;
                }

                let scheme = current_scheme();
                if scheme != last_scheme {
                    last_scheme = scheme;
                    if proxy.send_event(UserEvent::ColorSchemeChanged(scheme)).is_err() {
                        break;
                    }
                }
            }
        })
        .map_err(Into::into)
}

fn wait_for_personalize_change() -> Result<()> {
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            0,
            KEY_NOTIFY,
            &mut key,
        )
    }
    .ok()
    .context("open HKCU\\...\\Themes\\Personalize for change notifications")?;

    let notify_result = unsafe {
        RegNotifyChangeKeyValue(
            key,
            false,
            REG_NOTIFY_CHANGE_LAST_SET,
            None,
            false,
        )
    }
    .ok()
    .context("wait for theme registry change")
    .map(|_| ());

    let _ = unsafe { RegCloseKey(key) };
    notify_result
}
