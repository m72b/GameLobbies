//! Sign in through Steam (OpenID 2.0) to learn the account's SteamID64, without a password ever
//! reaching Game Lobby: the user signs in on steamcommunity.com in a sign-in window, and Steam
//! sends the window to our `return_to` address with a signed assertion. That navigation is
//! intercepted (it never loads); the assertion is checked here and confirmed with Steam
//! (`check_authentication`) before the SteamID64 is stored.
//!
//! Checks, all required: the `n` nonce in `return_to` belongs to the running sign-in; `mode` is
//! `id_res`; `op_endpoint` is Steam's; `return_to` is byte-equal to the one sent; `claimed_id`
//! equals `identity` and is a Steam individual-account id; `signed` covers `op_endpoint`,
//! `claimed_id`, `identity`, `return_to` and `response_nonce`; `response_nonce` is within five
//! minutes of now and was never seen before; and Steam answers `is_valid:true`.

use super::http::{network_error, read_capped_to, FetchError};
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

pub const OP_ENDPOINT: &str = "https://steamcommunity.com/openid/login";
const NS: &str = "http://specs.openid.net/auth/2.0";
const IDENTIFIER_SELECT: &str = "http://specs.openid.net/auth/2.0/identifier_select";
/// The path of `return_to`. Never served in [`ReturnMode::Intercept`].
pub const RETURN_PATH: &str = "/gamelobby/steam-return";
/// Fields `openid.signed` must list.
const MUST_SIGN: [&str; 5] = [
    "op_endpoint",
    "claimed_id",
    "identity",
    "return_to",
    "response_nonce",
];
/// How far `response_nonce`'s time may be from this PC's clock.
pub const NONCE_WINDOW_MS: i64 = 5 * 60_000;
const TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// `check_authentication` answers with two short lines.
const MAX_CHECK_BYTES: usize = 64 * 1024;
const MAX_PARAMS: usize = 64;
const MAX_VALUE_CHARS: usize = 2048;

static CLAIMED_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://steamcommunity\.com/openid/id/(7656119\d{10})$").expect("valid regex")
});

/// Where Steam sends the window back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnMode {
    /// `http://localhost/gamelobby/steam-return?n=…`, realm `http://localhost/`. Nothing
    /// listens: the navigation is caught by the sign-in window's navigation guard.
    Intercept,
    /// `http://127.0.0.1:<random port>/gamelobby/steam-return?n=…`, realm
    /// `http://127.0.0.1:<port>/`. The navigation is still caught by the guard; a listener on
    /// that one port and path takes the response as well, in case the window ever loads it.
    // Chosen by changing `RETURN_MODE`; the code for it is built and tested either way.
    #[allow(dead_code)]
    Loopback,
}

/// The mode this build uses. Whether Steam accepts the port-less `http://localhost/` realm is
/// not verified yet; if it doesn't, switch to [`ReturnMode::Loopback`] (see the design notes).
pub const RETURN_MODE: ReturnMode = ReturnMode::Intercept;

/// Why a sign-in ended without an account. Messages are fixed text.
#[derive(Debug, Clone, PartialEq)]
pub enum SignInError {
    Cancelled,
    TimedOut,
    /// The response failed a check (the reason is fixed text).
    Invalid(&'static str),
    /// The same `response_nonce` came back twice.
    Replayed,
    /// Steam did not confirm the assertion (`is_valid` was not `true`).
    NotConfirmed,
    Fetch(FetchError),
    /// The window could not be set up safely (for example InPrivate is not available).
    Window(&'static str),
}

impl SignInError {
    pub fn message(&self) -> String {
        match self {
            Self::Cancelled => "Sign-in cancelled.".into(),
            Self::TimedOut => "Sign-in timed out after 10 minutes. Try again.".into(),
            Self::Invalid(why) => format!("Steam's sign-in answer was refused: {why}"),
            Self::Replayed => "Steam's sign-in answer was already used. Try again.".into(),
            Self::NotConfirmed => "Steam did not confirm the sign-in. Try again.".into(),
            Self::Fetch(e) => e.message_for("Steam"),
            Self::Window(why) => format!("The sign-in window could not open: {why}"),
        }
    }
}

/// A fresh random nonce for `return_to` (two v4 UUIDs: 244 random bits, 64 hex characters).
pub fn new_nonce() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// What one sign-in expects back.
#[derive(Clone, Debug, PartialEq)]
pub struct Expected {
    pub nonce: String,
    pub return_to: String,
    pub realm: String,
}

impl Expected {
    /// `base` is `http://localhost` or `http://127.0.0.1:<port>` (no trailing slash).
    pub fn new(base: &str, nonce: String) -> Self {
        Self {
            return_to: format!("{base}{RETURN_PATH}?n={nonce}"),
            realm: format!("{base}/"),
            nonce,
        }
    }

    /// The Steam page the sign-in window opens.
    pub fn start_url(&self) -> Url {
        let mut url = Url::parse(OP_ENDPOINT).expect("valid OpenID endpoint");
        url.query_pairs_mut()
            .append_pair("openid.ns", NS)
            .append_pair("openid.mode", "checkid_setup")
            .append_pair("openid.return_to", &self.return_to)
            .append_pair("openid.realm", &self.realm)
            .append_pair("openid.identity", IDENTIFIER_SELECT)
            .append_pair("openid.claimed_id", IDENTIFIER_SELECT);
        url
    }

    /// Whether a return carries exactly one `n` and it is this sign-in's nonce. A return without
    /// it is stopped but ignored, so a stray or forged navigation can't use up the sign-in.
    pub fn carries_nonce(&self, url: &Url) -> bool {
        let mut values = url.query_pairs().filter(|(k, _)| k == "n").map(|(_, v)| v);
        matches!((values.next(), values.next()), (Some(n), None) if n == self.nonce.as_str())
    }

    /// Whether a navigation goes to `return_to`'s address (any query): such a navigation is
    /// always stopped, and its query checked.
    pub fn is_return(&self, url: &Url) -> bool {
        let Ok(expected) = Url::parse(&self.return_to) else {
            return false;
        };
        url.scheme() == expected.scheme()
            && url.host_str() == expected.host_str()
            && url.port() == expected.port()
            && url.path() == expected.path()
            && url.username().is_empty()
            && url.password().is_none()
    }
}

/// A response that passed the local checks.
#[derive(Clone, Debug, PartialEq)]
pub struct Assertion {
    pub steam_id: String,
    pub response_nonce: String,
    /// The `openid.*` fields, for `check_authentication`.
    pub params: Vec<(String, String)>,
}

/// Unix ms of a `response_nonce` (`YYYY-MM-DDTHH:MM:SSZ` followed by up to 255 characters).
pub fn nonce_time_ms(nonce: &str) -> Option<i64> {
    let b = nonce.as_bytes();
    if b.len() < 20 || b.len() > 20 + 255 || !nonce.is_ascii() {
        return None;
    }
    for (i, c) in b[..20].iter().enumerate() {
        let ok = match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        };
        if !ok {
            return None;
        }
    }
    let num = |from: usize, to: usize| nonce[from..to].parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + s * 1000)
}

/// `response_nonce` values already accepted, kept for twice the freshness window.
#[derive(Default, Debug)]
pub struct NonceCache {
    seen: HashMap<String, i64>,
}

impl NonceCache {
    const MAX: usize = 1024;

    /// Records `nonce` at `now`; false when it was seen before (a replay).
    pub fn insert(&mut self, nonce: &str, now: i64) -> bool {
        self.seen
            .retain(|_, at| now.saturating_sub(*at) <= 2 * NONCE_WINDOW_MS && *at <= now);
        if self.seen.contains_key(nonce) {
            return false;
        }
        if self.seen.len() >= Self::MAX {
            if let Some(oldest) = self
                .seen
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(k, _)| k.clone())
            {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(nonce.to_string(), now);
        true
    }
}

/// The query of a return navigation as a map; a repeated key or an oversized response is
/// refused.
fn query_map(url: &Url) -> Result<BTreeMap<String, String>, SignInError> {
    let mut map = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if map.len() >= MAX_PARAMS || value.chars().count() > MAX_VALUE_CHARS {
            return Err(SignInError::Invalid("the answer is too large."));
        }
        if map.insert(key.into_owned(), value.into_owned()).is_some() {
            return Err(SignInError::Invalid("a field appears twice."));
        }
    }
    Ok(map)
}

/// The local checks (everything but `check_authentication` and the replay set).
pub fn check_response(
    url: &Url,
    expected: &Expected,
    now_ms: i64,
) -> Result<Assertion, SignInError> {
    if !expected.is_return(url) {
        return Err(SignInError::Invalid("it came back to the wrong address."));
    }
    let q = query_map(url)?;
    let get = |key: &str| q.get(key).map(String::as_str);
    if get("n") != Some(expected.nonce.as_str()) {
        return Err(SignInError::Invalid("it belongs to another sign-in."));
    }
    if get("openid.mode") == Some("cancel") {
        return Err(SignInError::Cancelled);
    }
    if get("openid.ns") != Some(NS) {
        return Err(SignInError::Invalid("it is not an OpenID 2.0 answer."));
    }
    if get("openid.mode") != Some("id_res") {
        return Err(SignInError::Invalid("it is not a sign-in confirmation."));
    }
    if get("openid.op_endpoint") != Some(OP_ENDPOINT) {
        return Err(SignInError::Invalid("it did not come from Steam."));
    }
    if get("openid.return_to") != Some(expected.return_to.as_str()) {
        return Err(SignInError::Invalid("its return address does not match."));
    }
    let claimed = get("openid.claimed_id").unwrap_or_default();
    if get("openid.identity") != Some(claimed) {
        return Err(SignInError::Invalid("its account fields do not match."));
    }
    let steam_id = CLAIMED_ID
        .captures(claimed)
        .map(|c| c[1].to_string())
        .ok_or(SignInError::Invalid("it does not name a Steam account."))?;
    let signed: Vec<&str> = get("openid.signed")
        .unwrap_or_default()
        .split(',')
        .collect();
    if MUST_SIGN.iter().any(|f| !signed.contains(f))
        || signed
            .iter()
            .any(|f| f.is_empty() || !q.contains_key(&format!("openid.{f}")))
    {
        return Err(SignInError::Invalid(
            "its signature does not cover the account.",
        ));
    }
    if get("openid.sig").is_none_or(str::is_empty)
        || get("openid.assoc_handle").is_none_or(str::is_empty)
    {
        return Err(SignInError::Invalid("it is not signed."));
    }
    let response_nonce = get("openid.response_nonce").unwrap_or_default();
    let at =
        nonce_time_ms(response_nonce).ok_or(SignInError::Invalid("its time is unreadable."))?;
    if (now_ms - at).abs() > NONCE_WINDOW_MS {
        return Err(SignInError::Invalid(
            "it is too old, or this PC's clock is wrong.",
        ));
    }
    Ok(Assertion {
        steam_id,
        response_nonce: response_nonce.to_string(),
        params: q
            .iter()
            .filter(|(k, _)| k.starts_with("openid."))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    })
}

/// Where `check_authentication` goes. Production is Steam over https only; tests use a local
/// server.
#[derive(Clone, Debug)]
pub struct Endpoint {
    url: Url,
    https_only: bool,
}

impl Endpoint {
    pub fn production() -> Self {
        Self {
            url: Url::parse(OP_ENDPOINT).expect("valid OpenID endpoint"),
            https_only: true,
        }
    }

    #[cfg(test)]
    pub fn local(base: &str) -> Self {
        Self {
            url: Url::parse(&format!("{base}/openid/login")).unwrap(),
            https_only: false,
        }
    }
}

/// Asks Steam whether it really issued `assertion` (OpenID 2.0, 11.4.2).
pub async fn check_authentication(
    endpoint: &Endpoint,
    assertion: &Assertion,
) -> Result<(), SignInError> {
    if endpoint.https_only
        && (endpoint.url.scheme() != "https"
            || endpoint.url.host_str() != Some("steamcommunity.com"))
    {
        return Err(SignInError::Invalid("it can only be confirmed by Steam."));
    }
    let form: Vec<(&str, &str)> = assertion
        .params
        .iter()
        .map(|(k, v)| {
            if k == "openid.mode" {
                (k.as_str(), "check_authentication")
            } else {
                (k.as_str(), v.as_str())
            }
        })
        .collect();
    let http = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(endpoint.https_only)
        .user_agent(concat!("GameLobby/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| SignInError::Fetch(FetchError::Network("the HTTP client could not start.")))?;
    let response = http
        .post(endpoint.url.clone())
        .form(&form)
        .send()
        .await
        .map_err(|e| SignInError::Fetch(network_error(&e)))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(SignInError::Fetch(FetchError::Status(status)));
    }
    let body = read_capped_to(response, MAX_CHECK_BYTES)
        .await
        .map_err(SignInError::Fetch)?;
    let text = String::from_utf8(body).map_err(|_| SignInError::NotConfirmed)?;
    let mut valid = None;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key == "is_valid" {
                if valid.is_some() {
                    return Err(SignInError::NotConfirmed);
                }
                valid = Some(value.trim_end_matches('\r') == "true");
            }
        }
    }
    if valid == Some(true) {
        Ok(())
    } else {
        Err(SignInError::NotConfirmed)
    }
}

/// Every check, in order: local checks, then the replay set (before any network request, so
/// two copies of one answer can't both pass), then Steam's confirmation. The SteamID64 on
/// success.
pub async fn verify(
    endpoint: &Endpoint,
    url: &Url,
    expected: &Expected,
    now_ms: i64,
    seen: &std::sync::Mutex<NonceCache>,
) -> Result<String, SignInError> {
    let assertion = check_response(url, expected, now_ms)?;
    if !seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(&assertion.response_nonce, now_ms)
    {
        return Err(SignInError::Replayed);
    }
    check_authentication(endpoint, &assertion).await?;
    Ok(assertion.steam_id)
}

// ---------------------------------------------------------------------------
// Loopback return (ReturnMode::Loopback only)

const MAX_REQUEST_HEAD: usize = 8 * 1024;
const RETURN_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Game Lobby</title><p>Signed in. You can close this window.</p>";

/// Answers requests on the loopback listener: only `GET <RETURN_PATH>?…` is accepted (its URL
/// is handed to `on_return`); everything else gets 404. Runs until the task is aborted.
pub async fn serve_return<F>(listener: tokio::net::TcpListener, on_return: F)
where
    F: Fn(Url) + Send + Sync + 'static,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(local) = listener.local_addr() else {
        return;
    };
    let on_return = std::sync::Arc::new(on_return);
    loop {
        let Ok((mut socket, peer)) = listener.accept().await else {
            return;
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let on_return = on_return.clone();
        tokio::spawn(async move {
            let mut head = Vec::new();
            let mut buf = [0u8; 2048];
            let read = tokio::time::timeout(Duration::from_secs(10), async {
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return false,
                        Ok(n) if head.len() + n > MAX_REQUEST_HEAD => return false,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                true
            })
            .await;
            let url = (read == Ok(true))
                .then(|| return_url(&head, local.port()))
                .flatten();
            let reply = match &url {
                Some(_) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{RETURN_PAGE}",
                    RETURN_PAGE.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
                    .to_string(),
            };
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.shutdown().await;
            if let Some(url) = url {
                on_return(url);
            }
        });
    }
}

/// The URL of a `GET <RETURN_PATH>?… HTTP/1.x` request head, or `None`.
fn return_url(head: &[u8], port: u16) -> Option<Url> {
    let text = std::str::from_utf8(head).ok()?;
    let line = text.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if method != "GET" || !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return None;
    }
    if !target.starts_with(RETURN_PATH) || !target[RETURN_PATH.len()..].starts_with('?') {
        return None;
    }
    let url = Url::parse(&format!("http://127.0.0.1:{port}{target}")).ok()?;
    (url.path() == RETURN_PATH).then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const STEAM_ID: &str = "76561197960287930";
    const NOW: i64 = 1_795_000_000_000;

    fn stamp(ms: i64) -> String {
        // Inverse of nonce_time_ms for the test clock (UTC).
        let secs = ms / 1000;
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }

    fn expected() -> Expected {
        Expected::new("http://localhost", "a".repeat(64))
    }

    /// A positive assertion as Steam sends it, with `edit` applied to the fields.
    fn response(edit: impl FnOnce(&mut Vec<(String, String)>)) -> Url {
        let e = expected();
        let claimed = format!("https://steamcommunity.com/openid/id/{STEAM_ID}");
        let mut fields: Vec<(String, String)> = [
            ("openid.ns", NS),
            ("openid.mode", "id_res"),
            ("openid.op_endpoint", OP_ENDPOINT),
            ("openid.claimed_id", &claimed),
            ("openid.identity", &claimed),
            ("openid.return_to", &e.return_to),
            ("openid.response_nonce", &format!("{}abc123", stamp(NOW))),
            ("openid.assoc_handle", "1234567890"),
            (
                "openid.signed",
                "signed,op_endpoint,claimed_id,identity,return_to,response_nonce,assoc_handle",
            ),
            ("openid.sig", "c2lnbmF0dXJlLWJ5dGVz="),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        edit(&mut fields);
        let mut url = Url::parse(&e.return_to).unwrap();
        {
            let mut q = url.query_pairs_mut();
            for (k, v) in &fields {
                q.append_pair(k, v);
            }
        }
        url
    }

    fn set(fields: &mut [(String, String)], key: &str, value: &str) {
        fields.iter_mut().find(|(k, _)| k == key).unwrap().1 = value.into();
    }

    #[test]
    fn the_start_url_asks_steam_for_identifier_select_with_our_return_address() {
        let e = expected();
        assert_eq!(
            e.return_to,
            format!("http://localhost{RETURN_PATH}?n={}", "a".repeat(64))
        );
        assert_eq!(e.realm, "http://localhost/");
        let url = e.start_url();
        assert_eq!(url.host_str(), Some("steamcommunity.com"));
        assert_eq!(url.path(), "/openid/login");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["openid.mode"], "checkid_setup");
        assert_eq!(q["openid.return_to"], e.return_to);
        assert_eq!(q["openid.realm"], "http://localhost/");
        assert_eq!(q["openid.identity"], IDENTIFIER_SELECT);
        assert_eq!(q["openid.claimed_id"], IDENTIFIER_SELECT);
        let loop_back = Expected::new("http://127.0.0.1:50123", "b".repeat(64));
        assert_eq!(loop_back.realm, "http://127.0.0.1:50123/");
        assert!(loop_back.is_return(&Url::parse(&loop_back.return_to).unwrap()));
        assert!(!loop_back
            .is_return(&Url::parse(&format!("http://127.0.0.1:50124{RETURN_PATH}?n=x")).unwrap()));
    }

    #[test]
    fn only_a_return_with_this_sign_ins_single_nonce_counts() {
        let e = expected();
        assert!(e.carries_nonce(&response(|_| {})));
        let base = format!("http://localhost{RETURN_PATH}");
        for bad in [
            base.clone(),
            format!("{base}?openid.mode=id_res"),
            format!("{base}?n={}", "b".repeat(64)),
            format!("{base}?n={0}&n={0}", "a".repeat(64)),
            format!("{base}?n=&openid.mode=id_res"),
        ] {
            assert!(!e.carries_nonce(&Url::parse(&bad).unwrap()), "{bad}");
        }
    }

    #[test]
    fn nonces_are_long_random_and_distinct() {
        let (a, b) = (new_nonce(), new_nonce());
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn a_valid_answer_passes_the_local_checks() {
        let got = check_response(&response(|_| {}), &expected(), NOW).unwrap();
        assert_eq!(got.steam_id, STEAM_ID);
        assert!(got.params.iter().all(|(k, _)| k.starts_with("openid.")));
        assert!(!got.params.iter().any(|(k, _)| k == "n"));
    }

    #[test]
    fn tampered_or_foreign_answers_are_refused() {
        let e = expected();
        let bad = |url: Url| check_response(&url, &e, NOW).unwrap_err();
        // Wrong op_endpoint.
        assert!(matches!(
            bad(response(|f| set(
                f,
                "openid.op_endpoint",
                "https://evil.example/openid/login"
            ))),
            SignInError::Invalid(_)
        ));
        // claimed_id differs from identity.
        assert!(matches!(
            bad(response(|f| set(
                f,
                "openid.identity",
                "https://steamcommunity.com/openid/id/76561197960287931"
            ))),
            SignInError::Invalid(_)
        ));
        // Not a Steam id.
        let other = "https://steamcommunity.com.evil.example/openid/id/76561197960287930";
        assert!(matches!(
            bad(response(|f| {
                set(f, "openid.identity", other);
                set(f, "openid.claimed_id", other)
            })),
            SignInError::Invalid(_)
        ));
        // `signed` does not cover claimed_id (a tampered signed list).
        assert!(matches!(
            bad(response(|f| set(
                f,
                "openid.signed",
                "signed,op_endpoint,identity,return_to,response_nonce"
            ))),
            SignInError::Invalid(_)
        ));
        // `signed` names a field that is missing.
        assert!(matches!(
            bad(response(|f| set(
                f,
                "openid.signed",
                "signed,op_endpoint,claimed_id,identity,return_to,response_nonce,invite"
            ))),
            SignInError::Invalid(_)
        ));
        // No signature.
        assert!(matches!(
            bad(response(|f| set(f, "openid.sig", ""))),
            SignInError::Invalid(_)
        ));
        // Wrong return_to value.
        assert!(matches!(
            bad(response(|f| set(
                f,
                "openid.return_to",
                "http://localhost/gamelobby/steam-return?n=other"
            ))),
            SignInError::Invalid(_)
        ));
        // A different sign-in's nonce in the address.
        let mut url = response(|_| {});
        let query = url
            .query()
            .unwrap()
            .replacen(&"a".repeat(64), &"c".repeat(64), 1);
        url.set_query(Some(&query));
        assert!(matches!(bad(url), SignInError::Invalid(_)));
        // The wrong address.
        let mut url = response(|_| {});
        url.set_path("/elsewhere");
        assert!(matches!(bad(url), SignInError::Invalid(_)));
        // A field twice.
        let mut url = response(|_| {});
        url.query_pairs_mut().append_pair(
            "openid.claimed_id",
            "https://steamcommunity.com/openid/id/76561197960287931",
        );
        assert!(matches!(bad(url), SignInError::Invalid(_)));
        // Not id_res; cancel is its own outcome.
        assert!(matches!(
            bad(response(|f| set(f, "openid.mode", "setup_needed"))),
            SignInError::Invalid(_)
        ));
        assert_eq!(
            bad(response(|f| set(f, "openid.mode", "cancel"))),
            SignInError::Cancelled
        );
        // Error messages never carry the response.
        let message = bad(response(|f| {
            set(f, "openid.op_endpoint", "https://x.example/")
        }))
        .message();
        assert!(
            !message.contains(STEAM_ID) && !message.contains("x.example"),
            "{message}"
        );
    }

    #[test]
    fn stale_or_future_nonces_are_refused() {
        let e = expected();
        for offset in [-(NONCE_WINDOW_MS + 1000), NONCE_WINDOW_MS + 1000] {
            let url = response(|f| {
                set(
                    f,
                    "openid.response_nonce",
                    &format!("{}x", stamp(NOW + offset)),
                )
            });
            assert!(matches!(
                check_response(&url, &e, NOW),
                Err(SignInError::Invalid(_))
            ));
        }
        for offset in [-(NONCE_WINDOW_MS - 1000), NONCE_WINDOW_MS - 1000] {
            let url = response(|f| {
                set(
                    f,
                    "openid.response_nonce",
                    &format!("{}x", stamp(NOW + offset)),
                )
            });
            assert!(check_response(&url, &e, NOW).is_ok());
        }
        let url = response(|f| set(f, "openid.response_nonce", "yesterday"));
        assert!(matches!(
            check_response(&url, &e, NOW),
            Err(SignInError::Invalid(_))
        ));
    }

    #[test]
    fn nonce_times_parse_as_utc() {
        assert_eq!(nonce_time_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            nonce_time_ms("2000-03-01T00:00:01Zabc"),
            Some(951_868_801_000)
        );
        assert_eq!(nonce_time_ms(&stamp(NOW)), Some(NOW));
        for bad in [
            "2026-13-01T00:00:00Z",
            "2026-01-01 00:00:00Z",
            "2026-01-01T00:00:00",
            "２０２６-01-01T00:00:00Z",
        ] {
            assert_eq!(nonce_time_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_replay_set_refuses_a_second_use_and_forgets_old_entries() {
        let mut cache = NonceCache::default();
        assert!(cache.insert("n1", NOW));
        assert!(!cache.insert("n1", NOW + 1000));
        assert!(cache.insert("n2", NOW));
        // Long after the window, an entry is dropped (a nonce that old fails the time check).
        assert!(cache.insert("n1", NOW + 3 * NONCE_WINDOW_MS));
        for i in 0..2000 {
            cache.insert(&format!("x{i}"), NOW + 3 * NONCE_WINDOW_MS);
        }
        assert!(cache.seen.len() <= NonceCache::MAX);
    }

    type Seen = Arc<Mutex<Vec<String>>>;

    /// A local stand-in for Steam's `check_authentication`: every request gets `body`; request
    /// bodies are kept.
    async fn serve(status: u16, body: &'static str) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let log = log.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        let text = String::from_utf8_lossy(&raw).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let len = text
                                .lines()
                                .find_map(|l| {
                                    l.to_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if raw.len() >= end + 4 + len {
                                log.lock().unwrap().push(text);
                                break;
                            }
                        }
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => raw.extend_from_slice(&buf[..n]),
                        }
                    }
                    let out = format!(
                        "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(out.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    const VALID: &str = "ns:http://specs.openid.net/auth/2.0\nis_valid:true\n";

    #[tokio::test]
    async fn a_valid_answer_is_confirmed_with_steam_and_yields_the_steam_id() {
        let (base, seen) = serve(200, VALID).await;
        let cache = Mutex::new(NonceCache::default());
        let id = verify(
            &Endpoint::local(&base),
            &response(|_| {}),
            &expected(),
            NOW,
            &cache,
        )
        .await
        .unwrap();
        assert_eq!(id, STEAM_ID);
        let request = seen.lock().unwrap()[0].clone();
        assert!(
            request.starts_with("POST /openid/login HTTP/1.1"),
            "{request}"
        );
        assert!(
            request.contains("openid.mode=check_authentication"),
            "{request}"
        );
        assert!(
            request.contains("openid.sig=c2lnbmF0dXJlLWJ5dGVz%3D"),
            "{request}"
        );
        assert!(!request.contains("id_res"), "{request}");
        assert!(
            !request.contains("&n=") && !request.contains("\r\nn="),
            "{request}"
        );
    }

    #[tokio::test]
    async fn steam_saying_no_or_failing_is_not_a_sign_in() {
        for (status, body, want) in [
            (
                200,
                "ns:http://specs.openid.net/auth/2.0\nis_valid:false\n",
                SignInError::NotConfirmed,
            ),
            (
                200,
                "is_valid:true\nis_valid:false\n",
                SignInError::NotConfirmed,
            ),
            (200, "<html>is_valid:true</html>", SignInError::NotConfirmed),
            (200, "", SignInError::NotConfirmed),
            (500, VALID, SignInError::Fetch(FetchError::Status(500))),
        ] {
            let (base, _) = serve(status, body).await;
            let cache = Mutex::new(NonceCache::default());
            let got = verify(
                &Endpoint::local(&base),
                &response(|_| {}),
                &expected(),
                NOW,
                &cache,
            )
            .await;
            assert_eq!(got, Err(want));
        }
    }

    #[tokio::test]
    async fn a_tampered_answer_never_reaches_steam_and_a_reused_one_is_refused() {
        let (base, seen) = serve(200, VALID).await;
        let endpoint = Endpoint::local(&base);
        let cache = Mutex::new(NonceCache::default());
        let tampered =
            response(|f| set(f, "openid.op_endpoint", "https://evil.example/openid/login"));
        assert!(verify(&endpoint, &tampered, &expected(), NOW, &cache)
            .await
            .is_err());
        assert!(seen.lock().unwrap().is_empty());
        let url = response(|_| {});
        assert!(verify(&endpoint, &url, &expected(), NOW, &cache)
            .await
            .is_ok());
        assert_eq!(
            verify(&endpoint, &url, &expected(), NOW + 1000, &cache).await,
            Err(SignInError::Replayed)
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn production_confirmation_only_goes_to_steam_over_https() {
        let endpoint = Endpoint::production();
        assert_eq!(endpoint.url.as_str(), OP_ENDPOINT);
        let bad = Endpoint {
            url: Url::parse("http://steamcommunity.com/openid/login").unwrap(),
            https_only: true,
        };
        let assertion = check_response(&response(|_| {}), &expected(), NOW).unwrap();
        assert!(matches!(
            check_authentication(&bad, &assertion).await,
            Err(SignInError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn the_loopback_listener_takes_only_the_return_path() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let got: Arc<Mutex<Vec<Url>>> = Arc::default();
        let sink = got.clone();
        let task = tokio::spawn(serve_return(listener, move |url| {
            sink.lock().unwrap().push(url)
        }));
        let ask = |target: String| async move {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).await.unwrap();
            out
        };
        assert!(ask(format!("{RETURN_PATH}?n=1&openid.mode=id_res"))
            .await
            .starts_with("HTTP/1.1 200"));
        assert!(ask("/other?n=1".into()).await.starts_with("HTTP/1.1 404"));
        assert!(ask(RETURN_PATH.into()).await.starts_with("HTTP/1.1 404"));
        assert!(ask(format!("{RETURN_PATH}x?n=1"))
            .await
            .starts_with("HTTP/1.1 404"));
        let urls = got.lock().unwrap().clone();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].port(), Some(port));
        assert_eq!(urls[0].path(), RETURN_PATH);
        task.abort();
        assert!(return_url(b"POST /gamelobby/steam-return?n=1 HTTP/1.1\r\n\r\n", 1).is_none());
    }
}
