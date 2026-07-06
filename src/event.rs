//! User-event variants delivered to the `tao` event loop.
//!
//! Every event the application reacts to — komorebi state changes, tray icon
//! clicks, and menu activations — is funneled through this enum so the event
//! loop has a single match site.

use crate::komorebi::state::WorldState;
use crate::windows_theme::ColorScheme;

#[derive(Debug)]
pub enum UserEvent {
    /// Fresh world state from the komorebi worker (already debounced).
    StateChanged(WorldState),
    /// Mouse / hover event on any tray icon.
    TrayIcon(tray_icon::TrayIconEvent),
    /// Activation of any tray-icon context-menu item.
    Menu(tray_icon::menu::MenuEvent),
    /// Windows app color scheme changed (light/dark).
    ColorSchemeChanged(ColorScheme),
    /// Global hotkey pressed. `menu_was_visible` reflects whether one of our
    /// tray menus was already displayed at the moment the hotkey fired; it
    /// decides whether we advance to the next monitor or (re)open the menu for
    /// the currently focused monitor.
    Hotkey { menu_was_visible: bool },
}
