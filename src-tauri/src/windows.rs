//! Window creation with navigation guards: the webviews may only show the app's own
//! pages; allow-listed external links open in the default browser instead.

use tauri::webview::Color;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use url::Url;

pub const MAIN: &str = "main";

/// Whether a webview may use the app's commands and local protocols: only Game Lobby's own
/// window. Sign-in windows (`signin-<store>`) show remote store pages and get neither.
pub fn is_main_label(label: &str) -> bool {
    label == MAIN
}

/// Refuses a command that was not called from the main window. The command ACL (capabilities)
/// already keeps other webviews out; this is the second check, used by every accounts and setup
/// command and, through the invoke handler in `lib.rs`, by every other command.
pub fn require_main<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) -> Result<(), String> {
    if is_main_label(window.label()) {
        Ok(())
    } else {
        Err(NOT_MAIN.into())
    }
}

/// The error for a command called from another webview.
pub const NOT_MAIN: &str = "This window may not use Game Lobby's commands.";

/// The `403` a local protocol (`lobby-cover`, `lobby-shot`) returns to any webview but `main`.
pub fn forbidden_response() -> tauri::http::Response<std::borrow::Cow<'static, [u8]>> {
    tauri::http::Response::builder()
        .status(tauri::http::StatusCode::FORBIDDEN)
        .body(std::borrow::Cow::Borrowed(&b"Forbidden"[..]))
        .unwrap()
}
const PAGES: [&str; 2] = ["/", "/index.html"];
const EXTERNAL_HOSTS: [&str; 11] = [
    "playnite.link",
    // Steam Web API key page, privacy settings and account page (owned-games setup).
    "steamcommunity.com",
    "store.steampowered.com",
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

/// The user guide, the only page of github.com that the app's links may open (any section of
/// it). Exact path, no query: the rest of github.com stays closed.
pub const USER_GUIDE: &str = "https://github.com/m72b/GameLobby/blob/main/docs/USER-GUIDE.md";

fn is_allowed_external(url: &Url) -> bool {
    let plain = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none();
    let host = url.host_str().map(str::to_lowercase);
    let listed = host.as_deref().is_some_and(|h| EXTERNAL_HOSTS.contains(&h));
    let guide = host.as_deref() == Some("github.com")
        && url.query().is_none()
        && Some(url.path()) == Url::parse(USER_GUIDE).ok().as_ref().map(Url::path);
    plain && (listed || guide)
}

fn open_if_allowed_external(url: &Url) {
    if is_allowed_external(url) {
        let target = url.to_string();
        tauri::async_runtime::spawn(async move {
            let _ = crate::platform::shell_open(target).await;
        });
    }
}

/// Settings pages that `settings_open` may jump to (the `settings:open` event's payload).
pub const SETTINGS_SECTIONS: [&str; 5] =
    ["updates", "cover-art", "metadata", "api-keys", "accounts"];

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, MAIN, WebviewUrl::App("index.html".into()))
        .title("Game Lobby")
        .inner_size(1440.0, 900.0)
        .min_inner_size(800.0, 600.0)
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

/// Shows Settings, which lives inside the main window: brings that window to the front and
/// tells it with a `settings:open` event (payload: one of [`SETTINGS_SECTIONS`], or null for the
/// page last shown). Does nothing when the main window is gone; the request comes from it.
pub fn open_settings_at(app: &AppHandle, section: Option<&str>) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window(MAIN) else {
        return Ok(());
    };
    let _ = window.show();
    let _ = window.unminimize();
    window.set_focus()?;
    app.emit_to(MAIN, "settings:open", section)
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
        None => create(app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_main_page_is_an_app_page() {
        let page = |u: &str| is_app_page(&Url::parse(u).unwrap());
        assert!(page("http://tauri.localhost/index.html"));
        assert!(page("http://tauri.localhost/"));
        // Settings is part of the main window now; its old page no longer exists.
        assert!(!page("http://tauri.localhost/settings.html"));
        assert!(!page("https://example.com/index.html"));
    }

    #[test]
    fn only_the_main_window_is_trusted() {
        assert!(is_main_label("main"));
        for label in ["signin-steam", "signin-gog", "Main", "main ", ""] {
            assert!(!is_main_label(label), "{label}");
        }
        assert_eq!(forbidden_response().status(), 403);
    }

    #[test]
    fn settings_sections_include_the_api_keys_tab() {
        assert!(SETTINGS_SECTIONS.contains(&"api-keys"));
        assert!(SETTINGS_SECTIONS.contains(&"accounts"));
        assert!(!SETTINGS_SECTIONS.contains(&"libraries"));
    }

    #[test]
    fn only_the_user_guide_opens_on_github() {
        let ok = |u: &str| is_allowed_external(&Url::parse(u).unwrap());
        assert!(ok(USER_GUIDE));
        assert!(ok(&format!("{USER_GUIDE}#launch-options-on-a-launcher")));
        assert!(ok("https://www.steamgriddb.com/profile/preferences/api"));
        for bad in [
            "https://github.com/m72b/GameLobby",
            "https://github.com/evil/repo/blob/main/docs/USER-GUIDE.md",
            "https://github.com/m72b/GameLobby/blob/main/docs/USER-GUIDE.md?x=1",
            "https://github.com/m72b/GameLobby/blob/main/docs/USER-GUIDE.md/../../x",
            "http://github.com/m72b/GameLobby/blob/main/docs/USER-GUIDE.md",
            "https://user@github.com/m72b/GameLobby/blob/main/docs/USER-GUIDE.md",
            "https://github.com:444/m72b/GameLobby/blob/main/docs/USER-GUIDE.md",
            "https://github.com.evil.example/m72b/GameLobby/blob/main/docs/USER-GUIDE.md",
            "https://raw.githubusercontent.com/m72b/GameLobby/main/docs/USER-GUIDE.md",
        ] {
            assert!(!ok(bad), "{bad}");
        }
    }
}
