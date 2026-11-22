//! The Wikimedia API client: exact host allow list, https only, redirects only between allowed
//! hosts (each hop checked, at most two), timeouts, a response size cap (also after gzip
//! decoding), content-type checks, serial requests spaced [`SPACING`] apart, `maxlag=5` on
//! every Action API call, and `Retry-After` / backoff on 429, 503 and maxlag answers. Images
//! are never fetched here; they go through the cover cache (`crate::covercache`).

use std::fmt;
use std::io::Read;
use std::time::{Duration, Instant};

/// The only hosts API requests go to.
pub const API_HOSTS: [&str; 4] = [
    "www.wikidata.org",
    "query.wikidata.org",
    "en.wikipedia.org",
    "commons.wikimedia.org",
];
/// Wikimedia asks automated clients to identify themselves (no personal data in it).
pub const USER_AGENT: &str = concat!(
    "GameLobby/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/m72b/GameLobby)"
);
/// Minimum gap between two requests (Wikimedia asks for serial, unhurried clients).
pub const SPACING: Duration = Duration::from_millis(300);
const TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 2;
/// Waits before retrying a request that was limited, when the server names no time.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(20)];
/// Longest `Retry-After` honoured; a longer one ends the request as rate limited.
const MAX_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub enum WikiError {
    /// The URL or a redirect is not https on an allowed host.
    NotAllowed,
    /// Limited (429/503 or maxlag) even after waiting.
    RateLimited,
    Http(u16),
    Network(String),
    Invalid(&'static str),
    /// An API error code (`{"error":{"code":...}}`).
    Api(String),
}

impl fmt::Display for WikiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAllowed => f.write_str("Wikimedia request to a host that is not allowed."),
            Self::RateLimited => {
                f.write_str("Wikimedia is busy or limiting requests right now; try again later.")
            }
            Self::Http(code) => write!(f, "Wikimedia returned HTTP {code}."),
            Self::Network(msg) => write!(f, "Could not reach Wikimedia: {msg}"),
            Self::Invalid(msg) => write!(f, "Wikimedia returned an unexpected response: {msg}"),
            Self::Api(code) => write!(f, "Wikimedia refused the request ({code})."),
        }
    }
}

impl std::error::Error for WikiError {}

impl WikiError {
    /// The network or the service itself is failing (as opposed to one odd answer).
    pub fn is_outage(&self) -> bool {
        matches!(self, Self::Network(_) | Self::Http(500..=599))
    }
}

/// Which API a request goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `www.wikidata.org/w/api.php`
    Wikidata,
    /// `query.wikidata.org/sparql`
    Sparql,
    /// `en.wikipedia.org/w/api.php`
    Wikipedia,
    /// `commons.wikimedia.org/w/api.php`
    Commons,
}

/// Where requests go. Production is fixed to [`API_HOSTS`] over https; tests use a local
/// server.
#[derive(Clone, Debug)]
pub struct Endpoints {
    wikidata: url::Url,
    sparql: url::Url,
    wikipedia: url::Url,
    commons: url::Url,
    https_only: bool,
    hosts: Vec<String>,
    spacing: Duration,
    retry_delays: Vec<Duration>,
    max_wait: Duration,
}

impl Endpoints {
    pub fn production() -> Self {
        let url = |s: &str| url::Url::parse(s).expect("valid endpoint");
        Self {
            wikidata: url("https://www.wikidata.org/w/api.php"),
            sparql: url("https://query.wikidata.org/sparql"),
            wikipedia: url("https://en.wikipedia.org/w/api.php"),
            commons: url("https://commons.wikimedia.org/w/api.php"),
            https_only: true,
            hosts: API_HOSTS.iter().map(|h| h.to_string()).collect(),
            spacing: SPACING,
            retry_delays: RETRY_DELAYS.to_vec(),
            max_wait: MAX_WAIT,
        }
    }

    /// Every service on one local test server (`<base>/wikidata`, `/sparql`, ...).
    #[cfg(test)]
    pub fn local(base: &str) -> Self {
        let url = |p: &str| url::Url::parse(&format!("{base}/{p}")).unwrap();
        Self {
            wikidata: url("wikidata"),
            sparql: url("sparql"),
            wikipedia: url("wikipedia"),
            commons: url("commons"),
            https_only: false,
            hosts: vec!["127.0.0.1".into()],
            spacing: Duration::from_millis(1),
            retry_delays: vec![Duration::from_millis(5), Duration::from_millis(5)],
            max_wait: Duration::from_millis(20),
        }
    }

    fn base(&self, service: Service) -> &url::Url {
        match service {
            Service::Wikidata => &self.wikidata,
            Service::Sparql => &self.sparql,
            Service::Wikipedia => &self.wikipedia,
            Service::Commons => &self.commons,
        }
    }

    /// Whether a URL (the first request or a redirect) may be requested.
    pub fn allows(&self, url: &url::Url) -> bool {
        allowed(url, &self.hosts, self.https_only)
    }
}

fn allowed(url: &url::Url, hosts: &[String], https_only: bool) -> bool {
    let scheme_ok = url.scheme() == "https" || (!https_only && url.scheme() == "http");
    scheme_ok
        && url.username().is_empty()
        && url.password().is_none()
        && (!https_only || url.port().is_none())
        && url
            .host_str()
            .is_some_and(|h| hosts.iter().any(|a| a.eq_ignore_ascii_case(h)))
}

/// Spaces requests at least `spacing` apart, across every caller (one per app).
#[derive(Default)]
pub struct Limiter {
    next: tokio::sync::Mutex<Option<Instant>>,
}

impl Limiter {
    pub async fn wait(&self, spacing: Duration) {
        let mut next = self.next.lock().await;
        if let Some(at) = *next {
            let now = Instant::now();
            if at > now {
                tokio::time::sleep(at - now).await;
            }
        }
        *next = Some(Instant::now() + spacing);
    }

    async fn push_back(&self, by: Duration) {
        let mut next = self.next.lock().await;
        let at = Instant::now() + by;
        if next.is_none_or(|n| n < at) {
            *next = Some(at);
        }
    }
}

pub struct Api<'a> {
    http: reqwest::Client,
    endpoints: Endpoints,
    limiter: &'a Limiter,
}

/// Seconds from a `Retry-After` header (the HTTP-date form is treated as absent).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Decodes a gzip body with the same size cap as plain bodies.
pub fn gunzip_capped(bytes: &[u8], max: usize) -> Result<Vec<u8>, WikiError> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| WikiError::Invalid("the compressed response is damaged."))?;
    if out.len() > max {
        return Err(WikiError::Invalid("the response is too large."));
    }
    Ok(out)
}

async fn read_capped(mut response: reqwest::Response) -> Result<String, WikiError> {
    if response
        .content_length()
        .is_some_and(|len| len as usize > MAX_RESPONSE_BYTES)
    {
        return Err(WikiError::Invalid("the response is too large."));
    }
    let gzip = response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("gzip"));
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| WikiError::Network(e.without_url().to_string()))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(WikiError::Invalid("the response is too large."));
        }
        body.extend_from_slice(&chunk);
    }
    if gzip {
        body = gunzip_capped(&body, MAX_RESPONSE_BYTES)?;
    }
    String::from_utf8(body).map_err(|_| WikiError::Invalid("the response is not text."))
}

fn content_type_ok(response: &reqwest::Response, service: Service) -> bool {
    let value = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let essence = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    match service {
        Service::Sparql => {
            essence == "application/sparql-results+json" || essence == "application/json"
        }
        _ => essence == "application/json",
    }
}

/// Whether an Action API body is a `maxlag` error (the servers are lagging; wait and retry).
fn is_maxlag(body: &str) -> bool {
    body.len() < 4096
        && serde_json::from_str::<serde_json::Value>(body).is_ok_and(|v| {
            v.get("error")
                .and_then(|e| e.get("code"))
                .and_then(serde_json::Value::as_str)
                == Some("maxlag")
        })
}

impl<'a> Api<'a> {
    pub fn new(endpoints: Endpoints, limiter: &'a Limiter) -> Result<Self, WikiError> {
        for service in [
            Service::Wikidata,
            Service::Sparql,
            Service::Wikipedia,
            Service::Commons,
        ] {
            if !endpoints.allows(endpoints.base(service)) {
                return Err(WikiError::NotAllowed);
            }
        }
        let hosts = endpoints.hosts.clone();
        let https_only = endpoints.https_only;
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if allowed(attempt.url(), &hosts, https_only) {
                    attempt.follow()
                } else {
                    attempt.error("redirect to a host that is not allowed")
                }
            }))
            .https_only(endpoints.https_only)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| WikiError::Network(e.without_url().to_string()))?;
        Ok(Self {
            http,
            endpoints,
            limiter,
        })
    }

    /// The full URL of a request (Action API calls get `format=json`, `formatversion=2` and
    /// `maxlag=5`).
    pub fn url(&self, service: Service, params: &[(&str, &str)]) -> url::Url {
        let mut url = self.endpoints.base(service).clone();
        {
            let mut query = url.query_pairs_mut();
            for (k, v) in params {
                query.append_pair(k, v);
            }
            if service == Service::Sparql {
                query.append_pair("format", "json");
            } else {
                query.append_pair("format", "json");
                query.append_pair("formatversion", "2");
                query.append_pair("maxlag", "5");
            }
        }
        url
    }

    /// GET with retries on 429/503/maxlag. Returns the body text.
    pub async fn get(
        &self,
        service: Service,
        params: &[(&str, &str)],
    ) -> Result<String, WikiError> {
        let url = self.url(service, params);
        if !self.endpoints.allows(&url) {
            return Err(WikiError::NotAllowed);
        }
        let accept = if service == Service::Sparql {
            "application/sparql-results+json"
        } else {
            "application/json"
        };
        let mut delays = self.endpoints.retry_delays.iter();
        loop {
            self.limiter.wait(self.endpoints.spacing).await;
            let response = self
                .http
                .get(url.clone())
                .header(reqwest::header::ACCEPT, accept)
                .header(reqwest::header::ACCEPT_ENCODING, "gzip")
                .send()
                .await
                .map_err(|e| {
                    if e.is_redirect() {
                        WikiError::NotAllowed
                    } else {
                        WikiError::Network(e.without_url().to_string())
                    }
                })?;
            if !self.endpoints.allows(response.url()) {
                return Err(WikiError::NotAllowed);
            }
            let status = response.status().as_u16();
            let wait = retry_after(response.headers());
            let limited = match status {
                200 => {
                    if !content_type_ok(&response, service) {
                        return Err(WikiError::Invalid("unexpected content type."));
                    }
                    let body = read_capped(response).await?;
                    if service == Service::Sparql || !is_maxlag(&body) {
                        return Ok(body);
                    }
                    true
                }
                429 | 503 => true,
                code => return Err(WikiError::Http(code)),
            };
            debug_assert!(limited);
            let Some(default) = delays.next() else {
                return Err(WikiError::RateLimited);
            };
            let wait = wait.unwrap_or(*default);
            if wait > self.endpoints.max_wait {
                return Err(WikiError::RateLimited);
            }
            self.limiter.push_back(wait).await;
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One recorded request: the path and query (`/wikidata?action=...`) and its headers.
    #[derive(Debug, Clone)]
    pub struct Req {
        pub target: String,
        pub headers: Vec<(String, String)>,
    }

    impl Req {
        pub fn param(&self, name: &str) -> Option<String> {
            let url = url::Url::parse(&format!("http://x{}", self.target)).ok()?;
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        }
        pub fn path(&self) -> &str {
            self.target.split('?').next().unwrap_or_default()
        }
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    /// A reply: status, extra headers, body.
    pub type Reply = (u16, Vec<(String, String)>, Vec<u8>);
    pub type Handler = Arc<dyn Fn(&Req) -> Reply + Send + Sync>;
    pub type Log = Arc<Mutex<Vec<Req>>>;

    pub fn json_reply(body: &str) -> Reply {
        (
            200,
            vec![(
                "Content-Type".into(),
                "application/json; charset=utf-8".into(),
            )],
            body.as_bytes().to_vec(),
        )
    }

    pub async fn serve(handler: Handler) -> (String, Log) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Log = Arc::default();
        let seen = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => raw.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&raw).to_string();
                    let mut lines = head.split("\r\n");
                    let first = lines.next().unwrap_or_default().to_string();
                    let target = first
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_string();
                    let headers = lines
                        .filter_map(|l| l.split_once(':'))
                        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
                        .collect();
                    let req = Req { target, headers };
                    seen.lock().unwrap().push(req.clone());
                    let (status, headers, body) = handler(&req);
                    let mut out = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n");
                    for (n, v) in headers {
                        out.push_str(&format!("{n}: {v}\r\n"));
                    }
                    out.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
                    let mut bytes = out.into_bytes();
                    bytes.extend_from_slice(&body);
                    let _ = socket.write_all(&bytes).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), log)
    }

    #[test]
    fn production_goes_only_to_the_four_https_hosts() {
        let e = Endpoints::production();
        let limiter = Limiter::default();
        assert!(Api::new(e.clone(), &limiter).is_ok());
        for service in [
            Service::Wikidata,
            Service::Sparql,
            Service::Wikipedia,
            Service::Commons,
        ] {
            let base = e.base(service);
            assert_eq!(base.scheme(), "https");
            assert!(API_HOSTS.contains(&base.host_str().unwrap()));
        }
        let ok = |u: &str| e.allows(&url::Url::parse(u).unwrap());
        assert!(ok("https://www.wikidata.org/w/api.php"));
        assert!(ok("https://commons.wikimedia.org/w/api.php?x=1"));
        for bad in [
            "http://www.wikidata.org/w/api.php",
            "https://wikidata.org/w/api.php",
            "https://www.wikidata.org:8443/w/api.php",
            "https://user@www.wikidata.org/w/api.php",
            "https://www.wikidata.org.evil.example/w/api.php",
            "https://de.wikipedia.org/w/api.php",
            "https://upload.wikimedia.org/wikipedia/commons/a/a1/X.png",
            "https://evil.example/?h=www.wikidata.org",
        ] {
            assert!(!ok(bad), "{bad}");
        }
        assert!(USER_AGENT.starts_with("GameLobby/"));
        assert!(USER_AGENT.ends_with("(https://github.com/m72b/GameLobby)"));
    }

    #[test]
    fn action_api_urls_carry_maxlag_and_format() {
        let limiter = Limiter::default();
        let api = Api::new(Endpoints::production(), &limiter).unwrap();
        let url = api.url(
            Service::Wikidata,
            &[("action", "wbsearchentities"), ("search", "Doom & Co")],
        );
        assert_eq!(
            url.as_str(),
            "https://www.wikidata.org/w/api.php?action=wbsearchentities&search=Doom+%26+Co&format=json&formatversion=2&maxlag=5"
        );
        let sparql = api.url(Service::Sparql, &[("query", "SELECT ?i")]);
        assert_eq!(
            sparql.as_str(),
            "https://query.wikidata.org/sparql?query=SELECT+%3Fi&format=json"
        );
    }

    #[tokio::test]
    async fn requests_send_the_user_agent_and_decode_gzip() {
        let (base, log) = serve(Arc::new(|_: &Req| {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(br#"{"ok":1}"#).unwrap();
            (
                200,
                vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("Content-Encoding".into(), "gzip".into()),
                ],
                gz.finish().unwrap(),
            )
        }))
        .await;
        let limiter = Limiter::default();
        let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
        let body = api
            .get(Service::Wikidata, &[("action", "x")])
            .await
            .unwrap();
        assert_eq!(body, r#"{"ok":1}"#);
        let req = log.lock().unwrap()[0].clone();
        assert_eq!(req.header("User-Agent"), Some(USER_AGENT));
        assert_eq!(req.header("Accept-Encoding"), Some("gzip"));
        assert_eq!(req.param("maxlag").as_deref(), Some("5"));
        // A gzip bomb is cut off at the cap.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&vec![b'a'; 2000]).unwrap();
        assert!(gunzip_capped(&gz.finish().unwrap(), 1000).is_err());
    }

    #[tokio::test]
    async fn maxlag_and_429_wait_then_give_up() {
        let calls = Arc::new(Mutex::new(0));
        let counter = calls.clone();
        let (base, log) = serve(Arc::new(move |req: &Req| {
            let mut n = counter.lock().unwrap();
            *n += 1;
            match req.path() {
                // First a maxlag error, then 429, then success.
                "/wikidata" if *n == 1 => {
                    let mut r = json_reply(r#"{"error":{"code":"maxlag","info":"lagged"}}"#);
                    r.1.push(("Retry-After".into(), "0".into()));
                    r
                }
                "/wikidata" if *n == 2 => (429, vec![], vec![]),
                "/wikidata" => json_reply(r#"{"entities":{}}"#),
                _ => (503, vec![("Retry-After".into(), "0".into())], vec![]),
            }
        }))
        .await;
        let limiter = Limiter::default();
        let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
        assert_eq!(
            api.get(Service::Wikidata, &[]).await.unwrap(),
            r#"{"entities":{}}"#
        );
        assert_eq!(log.lock().unwrap().len(), 3);
        // Always 503: two retries, then RateLimited.
        assert_eq!(
            api.get(Service::Commons, &[]).await.unwrap_err(),
            WikiError::RateLimited
        );
        assert_eq!(log.lock().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn long_retry_after_ends_the_request() {
        let (base, log) = serve(Arc::new(|_: &Req| {
            (429, vec![("Retry-After".into(), "3600".into())], vec![])
        }))
        .await;
        let limiter = Limiter::default();
        let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
        assert_eq!(
            api.get(Service::Sparql, &[]).await.unwrap_err(),
            WikiError::RateLimited
        );
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn content_type_size_and_status_are_checked() {
        let (base, _) = serve(Arc::new(|req: &Req| match req.path() {
            "/wikidata" => (
                200,
                vec![("Content-Type".into(), "text/html".into())],
                b"<html>".to_vec(),
            ),
            "/wikipedia" => (
                200,
                vec![("Content-Type".into(), "application/json".into())],
                vec![b' '; MAX_RESPONSE_BYTES + 10],
            ),
            "/sparql" => (
                200,
                vec![(
                    "Content-Type".into(),
                    "application/sparql-results+json;charset=utf-8".into(),
                )],
                b"{}".to_vec(),
            ),
            _ => (404, vec![], vec![]),
        }))
        .await;
        let limiter = Limiter::default();
        let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
        assert!(matches!(
            api.get(Service::Wikidata, &[]).await,
            Err(WikiError::Invalid(_))
        ));
        assert!(matches!(
            api.get(Service::Wikipedia, &[]).await,
            Err(WikiError::Invalid(_))
        ));
        assert_eq!(api.get(Service::Sparql, &[]).await.unwrap(), "{}");
        assert_eq!(
            api.get(Service::Commons, &[]).await.unwrap_err(),
            WikiError::Http(404)
        );
    }

    #[tokio::test]
    async fn redirects_to_other_hosts_are_refused() {
        let (base, log) = serve(Arc::new(|req: &Req| match req.path() {
            "/wikidata" => (
                302,
                vec![("Location".into(), "http://localhost:1/evil".into())],
                vec![],
            ),
            "/commons" => (302, vec![("Location".into(), "/sparql".into())], vec![]),
            _ => json_reply("{}"),
        }))
        .await;
        let limiter = Limiter::default();
        let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
        assert_eq!(
            api.get(Service::Wikidata, &[]).await.unwrap_err(),
            WikiError::NotAllowed
        );
        // A redirect within the allowed host is followed.
        assert_eq!(api.get(Service::Commons, &[]).await.unwrap(), "{}");
        assert!(log
            .lock()
            .unwrap()
            .iter()
            .all(|r| !r.target.contains("evil")));
    }
}
