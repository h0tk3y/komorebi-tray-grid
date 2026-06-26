//! Top-level event-loop application logic.
//!
//! Owns the shared context menu, the live [`TrayManager`], the latest
//! [`WorldState`], and the autostart toggle state. Every interaction
//! ultimately routes through one of the `on_*` handlers, which are driven by
//! the `tao` event loop in `main.rs`.

use anyhow::{Context, Result};
use tao::event_loop::ControlFlow;
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
};

use crate::komorebi::state::WorldState;
use crate::render::Theme;
use crate::tray::TrayManager;

/// The full event-loop-bound application.
pub struct App {
    world: WorldState,
    tray: TrayManager,
    autostart_item: CheckMenuItem,
    quit_item: MenuItem,
}

impl App {
    /// Build the shared context menu, the [`TrayManager`], and seed the
    /// autostart checkbox from `initial_autostart`. No tray icons are
    /// created here — they're added on the first [`Self::on_state_changed`]
    /// call.
    pub fn new(initial_autostart: bool, theme: Theme) -> Result<Self> {
        let autostart_item = CheckMenuItem::new(
            "Enable autostart",
            true, // enabled
            initial_autostart,
            None, // accelerator
        );
        let quit_item = MenuItem::new("Quit", true, None);
        let separator = PredefinedMenuItem::separator();

        let menu = Menu::new();
        menu.append_items(&[
            &autostart_item as &dyn IsMenuItem,
            &separator as &dyn IsMenuItem,
            &quit_item as &dyn IsMenuItem,
        ])
        .context("build context menu")?;

        Ok(Self {
            world: WorldState::default(),
            tray: TrayManager::new(menu, theme),
            autostart_item,
            quit_item,
        })
    }

    /// Apply a fresh komorebi snapshot. Skips the (potentially expensive)
    /// reconcile when the state is unchanged.
    pub fn on_state_changed(&mut self, new_state: WorldState) {
        if new_state == self.world {
            return;
        }
        self.world = new_state;
        if let Err(e) = self.tray.reconcile(&self.world) {
            tracing::error!(error = %e, "tray reconcile failed");
        } else {
            tracing::debug!(
                monitors = self.world.monitors.len(),
                icons = self.tray.len(),
                "tray state updated",
            );
        }
    }

    /// Handle a menu activation. May set `control_flow` to `Exit` for the
    /// quit item.
    ///
    /// The `on_autostart_toggle` callback is invoked when the user clicks
    /// the autostart entry; it receives the newly-requested state and
    /// returns the actually-applied state (which may differ if the
    /// registry write failed). This indirection keeps `app.rs` free of any
    /// Win32 specifics — Step 5 plugs in the real autostart helper.
    pub fn on_menu_event(
        &mut self,
        event: MenuEvent,
        control_flow: &mut ControlFlow,
        on_autostart_toggle: &mut dyn FnMut(bool) -> bool,
    ) {
        if event.id == *self.quit_item.id() {
            tracing::info!("quit requested via tray menu");
            self.tray.clear();
            *control_flow = ControlFlow::Exit;
            return;
        }

        if event.id == *self.autostart_item.id() {
            // The CheckMenuItem already flipped its visual state when the
            // user clicked; mirror the request to the autostart backend and
            // resync the checkbox in case the write failed.
            let requested = self.autostart_item.is_checked();
            let applied = on_autostart_toggle(requested);
            if applied != requested {
                self.autostart_item.set_checked(applied);
            }
            tracing::info!(enabled = applied, "autostart toggle handled");
        }
    }

    /// Reset the checkbox to a known good value. Useful at startup or when
    /// the registry is mutated externally.
    pub fn refresh_autostart(&self, enabled: bool) {
        self.autostart_item.set_checked(enabled);
    }
}
