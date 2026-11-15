//! Window creation with navigation guards: the webviews may only show the app's own
//! pages; allow-listed external links open in the default browser instead.

use tauri::webview::Color;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use url::Url;

pub const MAIN: &str = "main";
pub const SETTINGS: &str = "settings";
const PAGES: [&str; 3] = ["/", "/index.html", "/settings.html"];
const EXTERNAL_HOSTS: [&str; 9] = [
    "playnite.link",
    "steamgriddb.com",
    "www.steamgriddb.com",
    "dev.twitch.tv",
    "api-docs.igdb.com",
    "igdb.com",
    "www.igdb.com",
    "howlongtobeat.com",
    "backloggd.com",
];

fn is_app_page(url: &Url) -> bool {
    let local = match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => {
            url.host_str() == Some("tauri.localhost")
                || (tauri::is_dev()
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
                    && url.port() == Some(5173))
        }
        _ => false,
    };
    local && PAGES.contains(&url.path())
}

fn open_if_allowed_external(url: &Url) {
    let allowed = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|h| EXTERNAL_HOSTS.contains(&h.to_lowercase().as_str()));
    if allowed {
        let target = url.to_string();
        tauri::async_runtime::spawn(async move {
            let _ = crate::platform::shell_open(target).await;
        });
    }
}

/// Settings pages that `settings_open` may jump to (`settings.html?section=<id>`).
pub const SETTINGS_SECTIONS: [&str; 3] = ["updates", "cover-art", "metadata"];

pub fn create(app: &AppHandle, settings: bool) -> tauri::Result<()> {
    create_page(app, settings, None)
}

fn create_page(app: &AppHandle, settings: bool, section: Option<&str>) -> tauri::Result<()> {
    let (label, page, title, size, min) = if settings {
        (
            SETTINGS,
            "settings.html",
            "Game Lobby Settings",
            (760.0, 820.0),
            (640.0, 600.0),
        )
    } else {
        (
            MAIN,
            "index.html",
            "Game Lobby",
            (1440.0, 900.0),
            (800.0, 600.0),
        )
    };
    let page = match section {
        Some(section) => format!("{page}?section={section}"),
        None => page.to_string(),
    };
    WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title(title)
        .inner_size(size.0, size.1)
        .min_inner_size(min.0, min.1)
        .decorations(false)
        .background_color(Color(16, 17, 19, 255))
        .on_navigation(|url| {
            if url.as_str() == "about:blank" || is_app_page(url) {
                return true;
            }
            open_if_allowed_external(url);
            false
        })
        .build()?;
    Ok(())
}

/// Opens (or focuses) the settings window, optionally on one of [`SETTINGS_SECTIONS`]. An open
/// window is told to switch with a `settings:section` event.
pub fn open_settings_at(app: &AppHandle, section: Option<&str>) -> tauri::Result<()> {
    match app.get_webview_window(SETTINGS) {
        Some(window) => {
            let _ = window.unminimize();
            if let Some(section) = section {
                let _ = app.emit_to(SETTINGS, "settings:section", section);
            }
            window.set_focus()
        }
        None => create_page(app, true, section),
    }
}

/// Brings the main window to the front: shown, unminimized and focused. Used when a second
/// copy of Game Lobby is started. Creates the window if it has been closed.
pub fn focus_main(app: &AppHandle) -> tauri::Result<()> {
    match app.get_webview_window(MAIN) {
        Some(window) => {
            window.show()?;
            if window.is_minimized().unwrap_or(false) {
                window.unminimize()?;
            }
            window.set_focus()
        }
        None => create(app, false),
    }
}
