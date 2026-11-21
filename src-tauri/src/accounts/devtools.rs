//! A diagnostic for the store sign-in window: console errors (uncaught script errors show up
//! there too) and failed or refused requests are written to Game Lobby's log, so a sign-in page
//! that doesn't work (Battle.net's form didn't render in the owner's test) shows why. The events
//! come from the WebView2 DevTools protocol (`Log.entryAdded`, `Network.requestWillBeSent` /
//! `responseReceived` / `loadingFailed`). The `Runtime` domain is deliberately not enabled: bot
//! and captcha checks look for it.
//!
//! What a page says is never logged, since it can contain what the user typed ("Invalid e-mail
//! alice@…"): only the error's class or a few known browser messages ([`safe_text`]), the
//! console source, and places as host and path (never a query, fragment, cookie or form value).
//! At most [`MAX_LINES`] lines per sign-in.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Lines logged per sign-in window at most.
pub const MAX_LINES: usize = 150;
const MAX_REQUESTS: usize = 2000;

/// The events the window subscribes to.
pub const EVENTS: [&str; 4] = [
    "Log.entryAdded",
    "Network.requestWillBeSent",
    "Network.responseReceived",
    "Network.loadingFailed",
];
/// The DevTools domains that must be enabled for them.
pub const DOMAINS: [&str; 2] = ["Log.enable", "Network.enable"];

/// `host/path` of a URL, without query or fragment; `?` when it isn't one.
pub fn place(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(url) if url.host_str().is_some() => {
            let mut out = format!("{}{}", url.host_str().unwrap_or_default(), url.path());
            out.truncate(120);
            out
        }
        Ok(url) => format!("{}:", url.scheme()),
        Err(_) => "?".into(),
    }
}

static ERROR_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(Uncaught )?(\(in promise\) )?([A-Z][A-Za-z]{0,40}(Error|Exception))\b")
        .expect("valid regex")
});
static NET_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"net::ERR_[A-Z_]{1,60}").expect("valid regex"));
static STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Failed to load resource: the server responded with a status of (\d{3})")
        .expect("valid regex")
});
static REFUSED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Refused to (load|connect|execute|frame|apply|evaluate|create|run)\b")
        .expect("valid regex")
});

/// What may be logged of a page message: the error class (`TypeError`, `Uncaught (in promise)
/// SyntaxError`), a network error code (`net::ERR_BLOCKED_BY_CLIENT`), "Failed to load resource
/// (HTTP 403)", or "Refused to load (Content Security Policy)". Anything else, including the
/// rest of those messages, is "(message hidden)".
pub fn safe_text(raw: &str) -> String {
    let first = raw.lines().next().unwrap_or_default().trim();
    if let Some(c) = ERROR_CLASS.captures(first) {
        return format!(
            "{}{}{}",
            c.get(1).map_or("", |m| m.as_str()),
            c.get(2).map_or("", |m| m.as_str()),
            &c[3]
        );
    }
    if let Some(c) = STATUS.captures(first) {
        return format!("Failed to load resource (HTTP {})", &c[1]);
    }
    if let Some(m) = NET_ERROR.find(first) {
        return m.as_str().to_string();
    }
    if let Some(c) = REFUSED.captures(first) {
        let csp = if first.contains("Content Security Policy") {
            " (Content Security Policy)"
        } else {
            ""
        };
        return format!("Refused to {}{csp}", &c[1]);
    }
    "(message hidden)".into()
}

/// A short plain token from the protocol itself (types, reasons): letters, digits, `_`, `-`.
fn token(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == ':')
        .take(60)
        .collect()
}

/// What one sign-in window has logged so far.
#[derive(Default, Debug)]
pub struct PageLog {
    requests: HashMap<String, String>,
    lines: usize,
}

impl PageLog {
    /// The log line for one DevTools event (`json` is its parameter object), if it is worth one.
    pub fn line(&mut self, event: &str, json: &str) -> Option<String> {
        if self.lines >= MAX_LINES {
            return None;
        }
        let v: Value = serde_json::from_str(json).ok()?;
        let s = |p: &str| v.pointer(p).and_then(Value::as_str);
        let out = match event {
            "Log.entryAdded" if s("/entry/level") == Some("error") => {
                let at = s("/entry/url").map(place).unwrap_or_default();
                Some(format!(
                    "console error [{}]: {} ({at})",
                    token(s("/entry/source").unwrap_or("?")),
                    safe_text(s("/entry/text").unwrap_or(""))
                ))
            }
            "Network.requestWillBeSent" => {
                if let (Some(id), Some(url)) = (s("/requestId"), s("/request/url")) {
                    if self.requests.len() < MAX_REQUESTS {
                        self.requests.insert(id.to_string(), place(url));
                    }
                }
                None
            }
            "Network.responseReceived" => {
                let status = v.pointer("/response/status").and_then(Value::as_f64)? as i64;
                (status >= 400).then(|| {
                    let at = s("/response/url").map(place).unwrap_or_default();
                    format!(
                        "HTTP {status} for {} ({at})",
                        token(s("/type").unwrap_or("request"))
                    )
                })
            }
            "Network.loadingFailed" => {
                let at = s("/requestId")
                    .and_then(|id| self.requests.get(id).cloned())
                    .unwrap_or_else(|| "?".into());
                let blocked = s("/blockedReason")
                    .map(|r| format!(", blocked: {}", token(r)))
                    .unwrap_or_default();
                let canceled = if v.pointer("/canceled").and_then(Value::as_bool) == Some(true) {
                    ", canceled"
                } else {
                    ""
                };
                Some(format!(
                    "request failed: {} for {}{blocked}{canceled} ({at})",
                    safe_text(s("/errorText").unwrap_or("?")),
                    token(s("/type").unwrap_or("request"))
                ))
            }
            _ => None,
        };
        if out.is_some() {
            self.lines += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_never_carry_a_query() {
        assert_eq!(
            place("https://account.battle.net/login/en/?ref=x&code=secret#f"),
            "account.battle.net/login/en/"
        );
        assert_eq!(place("data:text/html,x"), "data:");
        assert_eq!(place("nonsense"), "?");
    }

    #[test]
    fn page_messages_are_reduced_to_safe_classes() {
        for (raw, want) in [
            ("Uncaught TypeError: Cannot assign to read only property 'toString' of object", "Uncaught TypeError"),
            ("Uncaught (in promise) SyntaxError: Unexpected token < in JSON", "Uncaught (in promise) SyntaxError"),
            ("Failed to load resource: the server responded with a status of 403 ()", "Failed to load resource (HTTP 403)"),
            ("GET https://us.battle.net/login?x=1 net::ERR_BLOCKED_BY_CLIENT", "net::ERR_BLOCKED_BY_CLIENT"),
            ("Refused to load the script 'https://x/y.js' because it violates the following Content Security Policy directive", "Refused to load (Content Security Policy)"),
            ("Invalid email: alice@example.com", "(message hidden)"),
            ("Error: alice@example.com is not registered", "(message hidden)"),
            ("ValidationError for alice@example.com", "ValidationError"),
            ("", "(message hidden)"),
        ] {
            let got = safe_text(raw);
            assert_eq!(got, want, "{raw}");
            assert!(!got.contains("alice"));
        }
    }

    #[test]
    fn events_become_short_lines_without_secrets() {
        let mut log = PageLog::default();
        // Uncaught errors arrive as console errors; the Runtime domain is not used.
        assert!(!EVENTS
            .iter()
            .chain(DOMAINS.iter())
            .any(|e| e.starts_with("Runtime")));
        assert_eq!(
            log.line(
                "Runtime.exceptionThrown",
                r#"{"exceptionDetails":{"text":"x"}}"#
            ),
            None
        );
        let line = log
            .line(
                "Log.entryAdded",
                r#"{"entry":{"source":"javascript","level":"error","text":"Uncaught TypeError: Cannot read properties of undefined (reading 'alice@example.com')","url":"https://account.battle.net/static/app.js?v=1&token=abc"}}"#,
            )
            .unwrap();
        assert_eq!(
            line,
            "console error [javascript]: Uncaught TypeError (account.battle.net/static/app.js)"
        );
        assert!(!line.contains("alice") && !line.contains("token=abc"));
        assert_eq!(
            log.line(
                "Log.entryAdded",
                r#"{"entry":{"source":"other","level":"error","text":"Invalid email: alice@example.com","url":"https://account.battle.net/login"}}"#
            )
            .unwrap(),
            "console error [other]: (message hidden) (account.battle.net/login)"
        );
        assert_eq!(
            log.line(
                "Log.entryAdded",
                r#"{"entry":{"level":"info","text":"hi"}}"#
            ),
            None
        );
        assert_eq!(
            log.line(
                "Network.requestWillBeSent",
                r#"{"requestId":"7","request":{"url":"https://us.battle.net/login/srp?csrf=abc"}}"#
            ),
            None
        );
        assert_eq!(
            log.line(
                "Network.loadingFailed",
                r#"{"requestId":"7","errorText":"net::ERR_BLOCKED_BY_CLIENT","blockedReason":"inspector","type":"XHR"}"#
            )
            .unwrap(),
            "request failed: net::ERR_BLOCKED_BY_CLIENT for XHR, blocked: inspector (us.battle.net/login/srp)"
        );
        assert_eq!(
            log.line(
                "Network.responseReceived",
                r#"{"type":"Document","response":{"status":403,"url":"https://account.battle.net/login?x=1"}}"#
            )
            .unwrap(),
            "HTTP 403 for Document (account.battle.net/login)"
        );
        assert_eq!(
            log.line(
                "Network.responseReceived",
                r#"{"response":{"status":200,"url":"https://a.b/"}}"#
            ),
            None
        );
        assert_eq!(log.line("Other.event", "{}"), None);
        assert_eq!(log.line("Log.entryAdded", "not json"), None);
    }

    #[test]
    fn a_flood_is_capped() {
        let mut log = PageLog::default();
        let n = (0..1000)
            .filter_map(|_| {
                log.line(
                    "Log.entryAdded",
                    r#"{"entry":{"level":"error","text":"x"}}"#,
                )
            })
            .count();
        assert_eq!(n, MAX_LINES);
    }
}
