//! Tray icon, "Minimize to tray", "Close to tray" and the "Launch on boot" start mode.
//!
//! The tray icon exists only while "Minimize to tray" or "Close to tray" is on, and is removed
//! when both are off (a permanent icon that does nothing would be clutter). Hiding the window
//! never stops anything else: the playtime tracker and the scan timer are Rust tasks. Every real
//! exit (tray "Quit", the app's Quit button, the updater) calls [`quit`] / [`allow_exit`] first,
//! so the close handler never blocks it.

use crate::types::Settings;
use crate::windows::MAIN;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Window, WindowEvent};

const TRAY_ID: &str = "main";
const MENU_OPEN: &str = "open";
const MENU_QUIT: &str = "quit";
/// Created in the data folder the first time the window is hidden; the hint is shown once.
const HINT_MARKER: &str = "tray-hint-shown";

static MINIMIZE_TO_TRAY: AtomicBool = AtomicBool::new(false);
static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(false);
/// A tray icon was created and is usable. The window is only hidden while this is true, so a
/// failed icon never leaves the user with an invisible app.
static TRAY_READY: AtomicBool = AtomicBool::new(false);
/// The app is exiting for real: the close handler lets everything through.
static QUITTING: AtomicBool = AtomicBool::new(false);
/// The window was hidden for the first time; the hint is sent when it is next shown.
static HINT_PENDING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Show,
    /// Started by Windows at sign-in, no tray: a minimized taskbar button.
    Minimized,
    /// Started by Windows at sign-in with the tray on: no window until the tray icon is used.
    Hidden,
}

pub fn wants_tray(settings: &Settings) -> bool {
    settings.minimize_to_tray || settings.close_to_tray
}

/// The first window state: only a start by Windows' Run entry (`--autostart`) is quiet.
pub fn start_mode(autostart: bool, tray_ready: bool) -> StartMode {
    match (autostart, tray_ready) {
        (false, _) => StartMode::Show,
        (true, true) => StartMode::Hidden,
        (true, false) => StartMode::Minimized,
    }
}

/// Whether the close button should hide the window instead of closing it.
fn hide_on_close(close_to_tray: bool, tray_ready: bool, quitting: bool) -> bool {
    close_to_tray && tray_ready && !quitting
}

fn hide_on_minimize(minimize_to_tray: bool, tray_ready: bool, quitting: bool) -> bool {
    minimize_to_tray && tray_ready && !quitting
}

/// Call before any deliberate exit (the updater's restart, Quit) so that Close to tray cannot
/// keep the app alive.
pub fn allow_exit() {
    QUITTING.store(true, Ordering::SeqCst);
}

pub fn quit(app: &AppHandle) {
    allow_exit();
    app.exit(0);
}

/// Startup (setup hook, main thread): stores the options, creates the tray icon when wanted and
/// returns how the window should first appear.
pub fn init(app: &AppHandle, settings: &Settings, autostart: bool) -> StartMode {
    store_flags(settings);
    if wants_tray(settings) {
        TRAY_READY.store(create_icon(app), Ordering::SeqCst);
    }
    start_mode(autostart, TRAY_READY.load(Ordering::SeqCst))
}

pub fn apply_settings(app: &AppHandle, settings: &Settings) {
    store_flags(settings);
    let want = wants_tray(settings);
    let handle = app.clone();
    // Tray icons are created and removed on the main thread.
    let queued = app.run_on_main_thread(move || {
        let exists = handle.tray_by_id(TRAY_ID).is_some();
        if want && !exists {
            TRAY_READY.store(create_icon(&handle), Ordering::SeqCst);
        } else if !want && exists {
            TRAY_READY.store(false, Ordering::SeqCst);
            let _ = handle.remove_tray_by_id(TRAY_ID);
            // A window hidden in the tray must not be stranded when its icon goes away.
            if let Err(e) = crate::windows::focus_main(&handle) {
                log::warn!("Could not show the main window: {e}");
            }
        }
    });
    if let Err(e) = queued {
        log::warn!("Could not update the tray icon: {e}");
    }
    sync_autostart(settings.launch_on_boot);
}

/// Makes the Run entry match the setting. Debug builds (including a throwaway
/// GAME_LOBBY_DATA_DIR library) never touch the user's real startup entry.
pub fn sync_autostart(enabled: bool) {
    if cfg!(debug_assertions) {
        return;
    }
    if let Err(e) = crate::autostart::reconcile(enabled) {
        log::warn!("{e}");
    }
}

fn store_flags(settings: &Settings) {
    MINIMIZE_TO_TRAY.store(settings.minimize_to_tray, Ordering::SeqCst);
    CLOSE_TO_TRAY.store(settings.close_to_tray, Ordering::SeqCst);
}

fn create_icon(app: &AppHandle) -> bool {
    match build_icon(app) {
        Ok(()) => true,
        Err(e) => {
            log::error!("Could not create the tray icon: {e}");
            false
        }
    }
}

fn build_icon(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, MENU_OPEN, "Open Game Lobby", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Game Lobby")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_OPEN => show_main(app),
            MENU_QUIT => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Err(e) = crate::windows::focus_main(app) {
        log::warn!("Could not show the main window: {e}");
    }
}

fn quit_app(app: &AppHandle) {
    log::info!("Quit from the tray menu.");
    quit(app);
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() != MAIN {
        return;
    }
    let ready = TRAY_READY.load(Ordering::SeqCst);
    let quitting = QUITTING.load(Ordering::SeqCst);
    match event {
        WindowEvent::CloseRequested { api, .. }
            if hide_on_close(CLOSE_TO_TRAY.load(Ordering::SeqCst), ready, quitting) =>
        {
            api.prevent_close();
            hide(window);
        }
        // Windows reports a minimize as a resize to nothing; there is no separate event.
        WindowEvent::Resized(_)
            if hide_on_minimize(MINIMIZE_TO_TRAY.load(Ordering::SeqCst), ready, quitting)
                && window.is_minimized().unwrap_or(false) =>
        {
            hide(window);
        }
        _ => {}
    }
}

fn hide(window: &Window) {
    if let Err(e) = window.hide() {
        log::warn!("Could not hide the main window: {e}");
        return;
    }
    note_first_hide(window.app_handle());
}

/// The first time the window disappears into the tray, remember to tell the user (once, ever)
/// the next time it is shown.
fn note_first_hide(app: &AppHandle) {
    let Some(state) = app.try_state::<crate::state::AppState>() else {
        return;
    };
    let marker = state.data_dir.join(HINT_MARKER);
    if marker.exists() {
        return;
    }
    if std::fs::write(&marker, b"").is_ok() {
        HINT_PENDING.store(true, Ordering::SeqCst);
    }
}

/// Sends the one-time "still running in the tray" hint to the renderer, if one is waiting. Called
/// whenever the main window is shown.
pub fn announce_if_pending(app: &AppHandle) {
    if HINT_PENDING.swap(false, Ordering::SeqCst) {
        let _ = app.emit_to(MAIN, "tray:hint", ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(minimize: bool, close: bool) -> Settings {
        Settings {
            minimize_to_tray: minimize,
            close_to_tray: close,
            ..Settings::default()
        }
    }

    #[test]
    fn the_tray_is_wanted_when_either_option_is_on() {
        assert!(!wants_tray(&settings(false, false)));
        assert!(wants_tray(&settings(true, false)));
        assert!(wants_tray(&settings(false, true)));
        assert!(wants_tray(&settings(true, true)));
        assert!(!wants_tray(&Settings::default()));
    }

    #[test]
    fn a_normal_start_shows_the_window_and_an_autostart_stays_quiet() {
        assert_eq!(start_mode(false, false), StartMode::Show);
        assert_eq!(start_mode(false, true), StartMode::Show);
        assert_eq!(start_mode(true, true), StartMode::Hidden);
        // No working tray icon: never hide the only way back to the app.
        assert_eq!(start_mode(true, false), StartMode::Minimized);
    }

    #[test]
    fn closing_and_minimizing_hide_only_with_the_option_a_tray_icon_and_no_exit_in_progress() {
        assert!(hide_on_close(true, true, false));
        assert!(!hide_on_close(false, true, false));
        assert!(!hide_on_close(true, false, false));
        // Quit, the updater and the installer must always get through.
        assert!(!hide_on_close(true, true, true));
        assert!(hide_on_minimize(true, true, false));
        assert!(!hide_on_minimize(false, true, false));
        assert!(!hide_on_minimize(true, false, false));
        assert!(!hide_on_minimize(true, true, true));
    }

    #[test]
    fn the_hint_is_sent_once() {
        HINT_PENDING.store(true, Ordering::SeqCst);
        assert!(HINT_PENDING.swap(false, Ordering::SeqCst));
        assert!(!HINT_PENDING.swap(false, Ordering::SeqCst));
    }
}
