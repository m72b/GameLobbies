//! A diagnostic for the store sign-in window: page script errors, console errors and failed
//! or refused requests are written to Game Lobby's log, so a sign-in page that doesn't work
//! (Battle.net's form didn't render in the owner's test) shows why. The events come from the
//! WebView2 DevTools protocol (`Runtime.exceptionThrown`, `Log.entryAdded`,
//! `Network.requestWillBeSent` / `responseReceived` / `loadingFailed`).
//!
//! Only places are logged as host and path, never a query, fragment, cookie or form value;
//! messages are cut to one short line with any URL query removed, and the log redacts again.
//! At most [`MAX_LINES`] lines per sign-in.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Lines logged per sign-in window at most.
pub const MAX_LINES: usize = 150;
const MAX_REQUESTS: usize = 2000;
const MAX_TEXT: usize = 200;

static QUERY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\?[^\s"'<>)]*"#).expect("valid regex"));

/// The events the window subscribes to.
pub const EVENTS: [&str; 5] = [
    "Runtime.exceptionThrown",
    "Log.entryAdded",
    "Network.requestWillBeSent",
    "Network.responseReceived",
    "Network.loadingFailed",
];
/// The DevTools domains that must be enabled for them.
pub const DOMAINS: [&str; 3] = ["Runtime.enable", "Log.enable", "Network.enable"];

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

/// One short line of page text: first line, control characters dropped, URL queries removed.
fn text(raw: &str) -> String {
    let first = raw.lines().next().unwrap_or_default();
    let clean: String = first.chars().filter(|c| !c.is_control()).collect();
    let clean = QUERY.replace_all(&clean, "?…").into_owned();
    clean.chars().take(MAX_TEXT).collect()
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
            "Runtime.exceptionThrown" => {
                let message = s("/exceptionDetails/exception/description")
                    .or_else(|| s("/exceptionDetails/text"))
                    .unwrap_or("unknown error");
                let at = s("/exceptionDetails/url").map(place).unwrap_or_default();
                Some(format!("script error: {} ({at})", text(message)))
            }
            "Log.entryAdded" if s("/entry/level") == Some("error") => {
                let at = s("/entry/url").map(place).unwrap_or_default();
                Some(format!(
                    "console error [{}]: {} ({at})",
                    text(s("/entry/source").unwrap_or("?")),
                    text(s("/entry/text").unwrap_or(""))
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
                        text(s("/type").unwrap_or("request"))
                    )
                })
            }
            "Network.loadingFailed" => {
                let at = s("/requestId")
                    .and_then(|id| self.requests.get(id).cloned())
                    .unwrap_or_else(|| "?".into());
                let blocked = s("/blockedReason")
                    .map(|r| format!(", blocked: {}", text(r)))
                    .unwrap_or_default();
                let canceled = if v.pointer("/canceled").and_then(Value::as_bool) == Some(true) {
                    ", canceled"
                } else {
                    ""
                };
                Some(format!(
                    "request failed: {} for {}{blocked}{canceled} ({at})",
                    text(s("/errorText").unwrap_or("?")),
                    text(s("/type").unwrap_or("request"))
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
    fn events_become_short_lines_without_secrets() {
        let mut log = PageLog::default();
        let line = log
            .line(
                "Runtime.exceptionThrown",
                r#"{"exceptionDetails":{"text":"Uncaught","url":"https://account.battle.net/static/app.js?v=1&token=abc","exception":{"description":"TypeError: Cannot assign to read only property 'toString' of object '#<Object>'\n    at https://x/y.js?k=1"}}}"#,
            )
            .unwrap();
        assert_eq!(
            line,
            "script error: TypeError: Cannot assign to read only property 'toString' of object '#<Object>' (account.battle.net/static/app.js)"
        );
        assert!(!line.contains("token=abc"));
        assert_eq!(
            log.line(
                "Log.entryAdded",
                r#"{"entry":{"source":"security","level":"error","text":"Refused to load https://hcaptcha.com/x?sitekey=1","url":"https://account.battle.net/login"}}"#
            )
            .unwrap(),
            "console error [security]: Refused to load https://hcaptcha.com/x?… (account.battle.net/login)"
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
                    "Runtime.exceptionThrown",
                    r#"{"exceptionDetails":{"text":"x"}}"#,
                )
            })
            .count();
        assert_eq!(n, MAX_LINES);
    }
}
