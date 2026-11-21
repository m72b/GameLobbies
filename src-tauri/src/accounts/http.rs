//! Shared pieces for talking to store web services: fixed-text errors, size-capped bodies and
//! title cleaning. Moved here unchanged from `steam_owned`, which still uses them the same way.
//!
//! Errors never carry a response body, a URL or `reqwest`'s own error text (it can contain the
//! request URL, and with it a key or a code).

/// Largest response body read by default. A large Steam library is about 150 bytes per game;
/// this allows far more than any real account.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Longest title kept from a store response.
pub const MAX_NAME_CHARS: usize = 200;

/// Why a request to a store failed. Every message is fixed text.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// HTTP 401/403: wrong or revoked credentials.
    Rejected,
    /// The service answered without the data (for Steam: private game details).
    Private,
    RateLimited,
    Network(&'static str),
    Status(u16),
    Invalid(&'static str),
}

impl FetchError {
    /// The message for `service` (a store's display name). Services with their own wording
    /// for [`FetchError::Rejected`] and [`FetchError::Private`] map those themselves (see
    /// `steam_owned`).
    pub fn message_for(&self, service: &str) -> String {
        match self {
            Self::Rejected => format!("{service} did not accept the saved credentials."),
            Self::Private => format!("{service} did not share the list of games."),
            Self::RateLimited => {
                format!("{service} is limiting requests. Game Lobby tries again later.")
            }
            Self::Network(what) => format!("Could not reach {service}: {what}"),
            Self::Status(code) => {
                format!("{service} returned an error (HTTP {code}). Game Lobby tries again later.")
            }
            Self::Invalid(what) => format!("{service} returned an unreadable response: {what}"),
        }
    }

    /// Whether only new credentials can fix it (no automatic retry).
    pub fn needs_new_credentials(&self) -> bool {
        matches!(self, Self::Rejected)
    }
}

/// A transport failure as fixed text. The error's own text is never used: it can carry the
/// request URL (with a key or a code in it).
pub fn network_error(e: &reqwest::Error) -> FetchError {
    FetchError::Network(if e.is_timeout() {
        "the request timed out."
    } else if e.is_connect() {
        "no connection. Check that this PC is online."
    } else {
        "the connection failed."
    })
}

/// The body, refused once it passes [`MAX_RESPONSE_BYTES`].
pub async fn read_capped(response: reqwest::Response) -> Result<Vec<u8>, FetchError> {
    read_capped_to(response, MAX_RESPONSE_BYTES).await
}

/// The body, refused once it passes `max` bytes (checked against `Content-Length` first and
/// again while reading, so a server that lies about the length is cut off too).
pub async fn read_capped_to(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, FetchError> {
    if response
        .content_length()
        .is_some_and(|len| len as usize > max)
    {
        return Err(FetchError::Invalid("the response is too large."));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| network_error(&e))? {
        if body.len() + chunk.len() > max {
            return Err(FetchError::Invalid("the response is too large."));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// An account name a store reports (GOG user name, BattleTag), for the sign-in
/// notice: plain one-line text of at most 64 characters, without markup characters.
pub fn display_name(raw: &str) -> Option<String> {
    let name = clean_name(raw)?;
    (name.chars().count() <= 64 && !name.contains(['<', '>', '"', '\\'])).then_some(name)
}

/// Plain one-line text: control and bidi characters dropped, whitespace collapsed, capped at
/// [`MAX_NAME_CHARS`].
pub fn clean_name(raw: &str) -> Option<String> {
    let text = crate::validation::plain_text(raw);
    let text: String = text.chars().take(MAX_NAME_CHARS).collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

// ---------------------------------------------------------------------------
// StoreHttp: requests of the cookie-based store accounts

/// Pages a store sync may request.
pub const MAX_PAGES: usize = 200;
/// Games a store sync may list.
pub const MAX_GAMES: usize = 50_000;
/// Redirects followed per request (each to a listed host over https).
pub const MAX_REDIRECTS: usize = 3;
/// Pause between two requests of one sync.
pub const PACING: std::time::Duration = std::time::Duration::from_millis(300);
const STORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const STORE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The HTTP client of one store sync: every request (and every redirect) must go to one of the
/// store's API hosts over https, without a port or user info; redirects are followed by hand
/// (at most [`MAX_REDIRECTS`], each checked), so the cookie jar sees every response and no
/// cookie ever goes to another host. A redirect off the list, or to a sign-in page, is not
/// followed and reads as [`FetchError::Rejected`] (the saved sign-in no longer works).
/// Responses are capped at [`MAX_RESPONSE_BYTES`]; requests are paced by [`PACING`]; a
/// `Retry-After` on 429 is kept for the caller's backoff.
pub struct StoreHttp {
    client: reqwest::Client,
    hosts: &'static [&'static str],
    https_only: bool,
    jar: std::sync::Mutex<super::cookies::CookieJar>,
    user_agent: String,
    pacing: std::time::Duration,
    last: tokio::sync::Mutex<Option<std::time::Instant>>,
    retry_after: std::sync::Mutex<Option<u64>>,
    /// An OAuth access token sent as `Authorization: Bearer` (GOG), only to `hosts`.
    bearer: Option<String>,
}

impl std::fmt::Debug for StoreHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StoreHttp({:?}, secrets hidden)", self.hosts)
    }
}

impl StoreHttp {
    /// Sends `token` as a bearer token with every request (to the listed hosts only).
    pub fn with_bearer(mut self, token: &str) -> Self {
        self.bearer = Some(token.to_string());
        self
    }

    /// Production: https only, to exactly `hosts`.
    pub fn new(
        hosts: &'static [&'static str],
        jar: super::cookies::CookieJar,
        user_agent: &str,
    ) -> Result<Self, FetchError> {
        Self::build(hosts, jar, user_agent, true, PACING)
    }

    #[cfg(test)]
    pub fn local(jar: super::cookies::CookieJar) -> Self {
        Self::build(
            &["127.0.0.1"],
            jar,
            "test-agent",
            false,
            std::time::Duration::ZERO,
        )
        .unwrap()
    }

    fn build(
        hosts: &'static [&'static str],
        jar: super::cookies::CookieJar,
        user_agent: &str,
        https_only: bool,
        pacing: std::time::Duration,
    ) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .timeout(STORE_TIMEOUT)
            .connect_timeout(STORE_CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(https_only)
            .build()
            .map_err(|_| FetchError::Network("the HTTP client could not start."))?;
        Ok(Self {
            client,
            hosts,
            https_only,
            jar: std::sync::Mutex::new(jar),
            user_agent: super::session::clean_user_agent(user_agent),
            pacing,
            last: tokio::sync::Mutex::new(None),
            retry_after: std::sync::Mutex::new(None),
            bearer: None,
        })
    }

    /// Whether a request may go to `url`.
    pub fn url_ok(&self, url: &url::Url) -> bool {
        let host = url.host_str().unwrap_or_default();
        let listed = self.hosts.contains(&host);
        if self.https_only {
            url.scheme() == "https"
                && url.port().is_none()
                && url.username().is_empty()
                && url.password().is_none()
                && matches!(url.host(), Some(url::Host::Domain(_)))
                && listed
        } else {
            matches!(url.scheme(), "http" | "https") && url.username().is_empty() && listed
        }
    }

    /// The cookies now (after `Set-Cookie` updates), to seal again after a sync.
    pub fn cookies(&self) -> super::cookies::CookieJar {
        super::lock(&self.jar).clone()
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Seconds a 429 asked to wait, if one did.
    pub fn retry_after(&self) -> Option<u64> {
        *super::lock(&self.retry_after)
    }

    async fn pace(&self) {
        let mut last = self.last.lock().await;
        if let Some(at) = *last {
            let since = at.elapsed();
            if since < self.pacing {
                tokio::time::sleep(self.pacing - since).await;
            }
        }
        *last = Some(std::time::Instant::now());
    }

    /// GETs `url` (JSON or HTML) and returns the body.
    pub async fn get(&self, url: url::Url) -> Result<Vec<u8>, FetchError> {
        let mut url = url;
        for _ in 0..=MAX_REDIRECTS {
            if !self.url_ok(&url) {
                return Err(FetchError::Invalid(
                    "a request would have left the store's servers.",
                ));
            }
            self.pace().await;
            let now_s = crate::types::now_ms() / 1000;
            let mut request = self
                .client
                .get(url.clone())
                .header(reqwest::header::USER_AGENT, &self.user_agent)
                .header(
                    reqwest::header::ACCEPT,
                    "application/json, text/html;q=0.9, */*;q=0.5",
                );
            if let Some(cookie) = super::lock(&self.jar).header_for(&url, now_s) {
                request = request.header(reqwest::header::COOKIE, cookie);
            }
            if let Some(token) = &self.bearer {
                request = request.bearer_auth(token);
            }
            let response = request.send().await.map_err(|e| network_error(&e))?;
            {
                let mut jar = super::lock(&self.jar);
                for header in response.headers().get_all(reqwest::header::SET_COOKIE) {
                    if let Ok(text) = header.to_str() {
                        jar.absorb(&url, text, now_s);
                    }
                }
            }
            let status = response.status().as_u16();
            match status {
                200 => return read_capped(response).await,
                301 | 302 | 303 | 307 | 308 => {
                    let next = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|loc| url.join(loc).ok())
                        .ok_or(FetchError::Invalid("a redirect had no usable address."))?;
                    let path = next.path().to_ascii_lowercase();
                    if !self.url_ok(&next) || path.contains("login") || path.contains("signin") {
                        // Usually the store's sign-in page: the saved sign-in no longer works.
                        return Err(FetchError::Rejected);
                    }
                    url = next;
                }
                401 | 403 => return Err(FetchError::Rejected),
                429 => {
                    let wait = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok());
                    *super::lock(&self.retry_after) = wait;
                    return Err(FetchError::RateLimited);
                }
                _ => return Err(FetchError::Status(status)),
            }
        }
        Err(FetchError::Invalid("the store redirected too often."))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    type Seen = Arc<Mutex<Vec<String>>>;

    /// A local store: `routes` maps a request path (with query) to a raw HTTP response head and
    /// body; request heads are kept.
    pub(crate) async fn store_server(routes: Vec<(&'static str, String)>) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let routes = Arc::new(routes);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (log, routes) = (log.clone(), routes.clone());
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => raw.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&raw).to_string();
                    let target = head.split(' ').nth(1).unwrap_or_default().to_string();
                    log.lock().unwrap().push(head.to_ascii_lowercase());
                    let out = routes
                        .iter()
                        .find(|(path, _)| *path == target)
                        .map(|(_, response)| response.clone())
                        .unwrap_or_else(|| {
                            "HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                .into()
                        });
                    let _ = socket.write_all(out.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// A local server that answers every request with `response`; request heads are kept
    /// (lowercased).
    pub(crate) async fn store_server_any(response: String) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (log, out) = (log.clone(), response.clone());
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => raw.extend_from_slice(&buf[..n]),
                        }
                    }
                    log.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&raw).to_ascii_lowercase());
                    let _ = socket.write_all(out.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    pub(crate) fn ok(body: &str, extra: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn redirect(to: &str) -> String {
        format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }

    const LOCAL: &[&str] = &["127.0.0.1"];

    #[tokio::test]
    async fn redirects_stay_on_the_list_and_cookies_follow_the_rules() {
        let (base, seen) = store_server(vec![
            ("/a", redirect("/b")),
            ("/b", ok("{\"x\":1}", "Set-Cookie: s=2; Path=/\r\nSet-Cookie: evil=1; Domain=evil.example\r\n")),
            ("/c", ok("{}", "")),
            ("/off", redirect("http://evil.example/steal")),
            ("/login", redirect("/account/login?next=/x")),
            ("/loop", redirect("/loop")),
            ("/limited", "HTTP/1.1 429 X\r\nRetry-After: 120\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()),
            ("/gone", "HTTP/1.1 401 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()),
            ("/big", format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_RESPONSE_BYTES + 1)),
        ])
        .await;
        let start = vec![crate::accounts::cookies::StoredCookie::from_webview(
            "s",
            "1",
            "127.0.0.1",
            "/",
            false,
            true,
            None,
            LOCAL,
        )
        .unwrap()];
        let http = StoreHttp::local(crate::accounts::cookies::CookieJar::new(LOCAL, start));
        let url = |p: &str| url::Url::parse(&format!("{base}{p}")).unwrap();
        // On-list redirect followed; the cookie went with both requests and was updated.
        assert_eq!(http.get(url("/a")).await.unwrap(), b"{\"x\":1}");
        http.get(url("/c")).await.unwrap();
        {
            let seen = seen.lock().unwrap();
            assert!(seen[0].contains("cookie: s=1") && seen[0].contains("user-agent: test-agent"));
            assert!(seen[1].starts_with("get /b "));
            assert!(seen[2].contains("cookie: s=2"), "{}", seen[2]);
            assert!(!seen.iter().any(|h| h.contains("evil")));
        }
        assert_eq!(http.cookies().cookies().len(), 1);
        // Off the list or to a sign-in page: not followed, read as a rejected sign-in.
        let before = seen.lock().unwrap().len();
        assert_eq!(http.get(url("/off")).await, Err(FetchError::Rejected));
        assert_eq!(http.get(url("/login")).await, Err(FetchError::Rejected));
        assert_eq!(seen.lock().unwrap().len(), before + 2);
        assert!(matches!(
            http.get(url("/loop")).await,
            Err(FetchError::Invalid(_))
        ));
        assert_eq!(seen.lock().unwrap().len(), before + 2 + 1 + MAX_REDIRECTS);
        assert_eq!(
            http.get(url("/limited")).await,
            Err(FetchError::RateLimited)
        );
        assert_eq!(http.retry_after(), Some(120));
        assert_eq!(http.get(url("/gone")).await, Err(FetchError::Rejected));
        assert!(matches!(
            http.get(url("/big")).await,
            Err(FetchError::Invalid(_))
        ));
        assert_eq!(
            http.get(url("/nothing")).await,
            Err(FetchError::Status(404))
        );
        // A host that is not listed is never contacted.
        let other = url::Url::parse("http://localhost:1/x").unwrap();
        assert!(matches!(http.get(other).await, Err(FetchError::Invalid(_))));
    }

    #[test]
    fn production_requests_need_https_and_a_listed_host() {
        let http = StoreHttp::new(&["www.gog.com"], Default::default(), "ua").unwrap();
        let ok = |u: &str| http.url_ok(&url::Url::parse(u).unwrap());
        assert!(ok("https://www.gog.com/u/x/games/stats?page=1"));
        for bad in [
            "http://www.gog.com/",
            "https://www.gog.com:444/",
            "https://user@www.gog.com/",
            "https://gog.com/",
            "https://menu.gog.com/",
            "https://www.gog.com.evil.example/",
            "https://127.0.0.1/",
        ] {
            assert!(!ok(bad), "{bad}");
        }
    }

    #[test]
    fn messages_are_fixed_text_naming_the_service() {
        assert_eq!(
            FetchError::RateLimited.message_for("Steam"),
            "Steam is limiting requests. Game Lobby tries again later."
        );
        assert_eq!(
            FetchError::Status(502).message_for("GOG"),
            "GOG returned an error (HTTP 502). Game Lobby tries again later."
        );
        assert_eq!(
            FetchError::Network("x").message_for("Steam"),
            "Could not reach Steam: x"
        );
        assert!(FetchError::Rejected.needs_new_credentials());
        assert!(!FetchError::Private.needs_new_credentials());
    }

    #[test]
    fn display_names_are_short_plain_text() {
        assert_eq!(display_name(" Gamer#1234 "), Some("Gamer#1234".into()));
        assert_eq!(
            display_name("me@example.com"),
            Some("me@example.com".into())
        );
        for bad in ["", "   ", "<b>x</b>", "a\"b", &"x".repeat(65)] {
            assert_eq!(display_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn names_are_cleaned_and_capped() {
        assert_eq!(clean_name("  A \n B\u{202e}\u{7} "), Some("A B".into()));
        assert_eq!(clean_name(" \t"), None);
        assert_eq!(clean_name(&"x".repeat(500)).unwrap().len(), MAX_NAME_CHARS);
    }
}
