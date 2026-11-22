//! The cookie jar of a cookie-based store account (Battle.net): the cookies the
//! sign-in window held when the user reached the store's signed-in page, filtered to the store's
//! own domains, sent by [`super::http::StoreHttp`] with each request and updated from the
//! responses' `Set-Cookie` headers (again only for the store's domains). The jar is sealed with
//! DPAPI after every sync ([`super::session`]); cookie values never reach the renderer, a log or
//! an error message (`Debug` hides them).
//!
//! The rules are a small, strict subset of RFC 6265: host-only and domain cookies, path prefix,
//! `Secure`, `Max-Age` / `Expires`; a cookie for a domain outside the store's list, or one the
//! response's host may not set, is dropped.

use serde::{Deserialize, Serialize};
use url::Url;

/// Cookies kept per store at most (stores set a few dozen).
pub const MAX_COOKIES: usize = 200;
const MAX_NAME: usize = 256;
const MAX_VALUE: usize = 4096;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    /// Sent only to `domain` itself, not to its subdomains.
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// Unix seconds; `None` for a session cookie.
    pub expires: Option<i64>,
}

impl std::fmt::Debug for StoredCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cookie({} for {}, value hidden)", self.name, self.domain)
    }
}

/// Whether a cookie name is a plain token.
fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&b))
}

/// Whether a cookie value is plain printable ASCII without separators.
fn value_ok(value: &str) -> bool {
    value.len() <= MAX_VALUE
        && value
            .bytes()
            .all(|b| (0x21..0x7f).contains(&b) && !b";,\\\"".contains(&b))
}

/// Whether a cookie's domain is one of the store's domains or a subdomain of one.
pub fn domain_allowed(domain: &str, allowed: &[&str]) -> bool {
    super::signin::host_allowed(domain, allowed)
}

/// Whether `host` may receive (or set) a cookie for `domain`.
fn domain_matches(host: &str, domain: &str, host_only: bool) -> bool {
    host == domain || (!host_only && host.ends_with(&format!(".{domain}")))
}

fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || (request.starts_with(cookie)
            && (cookie.ends_with('/') || request[cookie.len()..].starts_with('/')))
}

impl StoredCookie {
    /// A cookie from the sign-in window (WebView2), cleaned; `None` when it is not one the store
    /// may use. A domain with a leading dot is a domain cookie, without one host-only.
    #[allow(clippy::too_many_arguments)]
    pub fn from_webview(
        name: &str,
        value: &str,
        domain: &str,
        path: &str,
        secure: bool,
        http_only: bool,
        expires: Option<i64>,
        allowed: &[&str],
    ) -> Option<Self> {
        let host_only = !domain.starts_with('.');
        let domain = domain.trim_start_matches('.').to_ascii_lowercase();
        let cookie = Self {
            name: name.to_string(),
            value: value.to_string(),
            domain,
            host_only,
            path: if path.starts_with('/') {
                path.to_string()
            } else {
                "/".into()
            },
            secure,
            http_only,
            expires,
        };
        cookie.valid(allowed).then_some(cookie)
    }

    fn valid(&self, allowed: &[&str]) -> bool {
        name_ok(&self.name)
            && value_ok(&self.value)
            && self.path.len() <= 1024
            && self.path.bytes().all(|b| (0x21..0x7f).contains(&b))
            && domain_allowed(&self.domain, allowed)
    }

    fn expired(&self, now_s: i64) -> bool {
        self.expires.is_some_and(|at| at <= now_s)
    }
}

/// A store's cookies, limited to its domains.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CookieJar {
    allowed: &'static [&'static str],
    cookies: Vec<StoredCookie>,
}

impl CookieJar {
    /// A jar holding the `cookies` that belong to `allowed` (the rest are dropped).
    pub fn new(allowed: &'static [&'static str], cookies: Vec<StoredCookie>) -> Self {
        let mut jar = Self {
            allowed,
            cookies: Vec::new(),
        };
        for cookie in cookies {
            if cookie.valid(allowed) {
                jar.put(cookie, i64::MIN);
            }
        }
        jar
    }

    pub fn cookies(&self) -> &[StoredCookie] {
        &self.cookies
    }

    /// Adds or replaces a cookie. At [`MAX_COOKIES`], expired cookies go first, then the oldest
    /// session cookie; only when neither exists is the new cookie dropped.
    fn put(&mut self, cookie: StoredCookie, now_s: i64) {
        self.cookies.retain(|c| {
            !(c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        });
        if self.cookies.len() >= MAX_COOKIES {
            self.cookies.retain(|c| !c.expired(now_s));
        }
        if self.cookies.len() >= MAX_COOKIES {
            if let Some(oldest) = self.cookies.iter().position(|c| c.expires.is_none()) {
                self.cookies.remove(oldest);
            }
        }
        if self.cookies.len() < MAX_COOKIES {
            self.cookies.push(cookie);
        }
    }

    /// The `Cookie` header for a request to `url` at `now_s`, or `None` when nothing applies.
    pub fn header_for(&self, url: &Url, now_s: i64) -> Option<String> {
        let host = url.host_str()?.to_ascii_lowercase();
        let https = url.scheme() == "https";
        let pairs: Vec<String> = self
            .cookies
            .iter()
            .filter(|c| {
                !c.expired(now_s)
                    && (https || !c.secure)
                    && domain_matches(&host, &c.domain, c.host_only)
                    && path_matches(url.path(), &c.path)
            })
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    /// Takes one `Set-Cookie` header of a response from `url` at `now_s`. Ignored when it is
    /// malformed, for a domain outside the store's list or one `url`'s host may not set.
    pub fn absorb(&mut self, url: &Url, header: &str, now_s: i64) {
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return;
        };
        let mut parts = header.split(';');
        let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else {
            return;
        };
        let (name, value) = (name.trim(), value.trim().trim_matches('"'));
        let mut domain = None;
        let mut path = None;
        let mut expires = None;
        let mut max_age = None;
        let (mut secure, mut http_only) = (false, false);
        for attribute in parts {
            let (key, val) = match attribute.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (attribute.trim(), ""),
            };
            match key.to_ascii_lowercase().as_str() {
                "domain" if !val.is_empty() => {
                    domain = Some(val.trim_start_matches('.').to_ascii_lowercase())
                }
                "path" if val.starts_with('/') => path = Some(val.to_string()),
                "max-age" => max_age = val.parse::<i64>().ok(),
                "expires" => expires = http_date(val),
                "secure" => secure = true,
                "httponly" => http_only = true,
                _ => {}
            }
        }
        let host_only = domain.is_none();
        let domain = domain.unwrap_or_else(|| host.clone());
        if !domain_matches(&host, &domain, false) {
            return;
        }
        let expires = match max_age {
            Some(seconds) => Some(now_s.saturating_add(seconds)),
            None => expires,
        };
        let default_path = match url.path().rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(at) => url.path()[..at].to_string(),
        };
        let cookie = StoredCookie {
            name: name.to_string(),
            value: value.to_string(),
            domain,
            host_only,
            path: path.unwrap_or(default_path),
            secure,
            http_only,
            expires,
        };
        if !cookie.valid(self.allowed) {
            return;
        }
        if cookie.expired(now_s) {
            // A deletion.
            self.cookies.retain(|c| {
                !(c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
            });
            return;
        }
        self.put(cookie, now_s);
    }
}

/// Merges cookie lists read from several URLs: one cookie per name, domain and path (the first
/// one read wins).
pub fn merge(lists: Vec<Vec<StoredCookie>>) -> Vec<StoredCookie> {
    let mut out: Vec<StoredCookie> = Vec::new();
    for cookie in lists.into_iter().flatten() {
        if !out
            .iter()
            .any(|c| c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        {
            out.push(cookie);
        }
    }
    out
}

/// Unix seconds of an HTTP date (`Wed, 21 Oct 2026 07:28:00 GMT`, also with `-` between the
/// date parts); `None` when it is not one.
fn http_date(text: &str) -> Option<i64> {
    let text = text.split_once(',').map_or(text, |(_, rest)| rest).trim();
    let parts: Vec<&str> = text.split([' ', '-']).filter(|p| !p.is_empty()).collect();
    let [day, month, year, time, zone] = parts.as_slice() else {
        return None;
    };
    if !zone.eq_ignore_ascii_case("GMT") && !zone.eq_ignore_ascii_case("UTC") {
        return None;
    }
    let month = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| month.eq_ignore_ascii_case(m))? as i64
        + 1;
    let mut year: i64 = year.parse().ok()?;
    if year < 100 {
        year += if year < 70 { 2000 } else { 1900 };
    }
    let day: i64 = day.parse().ok()?;
    let clock: Vec<i64> = time
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let [h, m, s] = clock.as_slice() else {
        return None;
    };
    let stamp = format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z");
    super::steam_openid::nonce_time_ms(&stamp).map(|ms| ms / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOG: &[&str] = &["gog.com"];
    const NOW: i64 = 1_800_000_000;

    fn url(u: &str) -> Url {
        Url::parse(u).unwrap()
    }

    #[test]
    fn only_the_stores_domains_are_kept() {
        let ok = |domain: &str| {
            StoredCookie::from_webview("gog-al", "abc", domain, "/", true, true, None, GOG)
                .is_some()
        };
        assert!(ok(".gog.com"));
        assert!(ok("www.gog.com"));
        assert!(ok("login.gog.com"));
        for bad in [
            ".google.com",
            "gog.com.evil.example",
            "evilgog.com",
            ".com",
            "xn--gog-xyz.com",
            "",
        ] {
            assert!(!ok(bad), "{bad}");
        }
        // Names and values with separators or control characters are not cookies.
        for (name, value) in [
            ("a b", "x"),
            ("a", "x;y"),
            ("a", "x\ny"),
            ("", "x"),
            ("a", "é"),
        ] {
            assert!(
                StoredCookie::from_webview(name, value, ".gog.com", "/", true, true, None, GOG)
                    .is_none(),
                "{name:?}={value:?}"
            );
        }
        let jar = CookieJar::new(
            GOG,
            vec![
                StoredCookie::from_webview("a", "1", ".gog.com", "/", true, false, None, GOG)
                    .unwrap(),
                StoredCookie {
                    domain: "google.com".into(),
                    ..StoredCookie::from_webview("b", "2", ".gog.com", "/", true, false, None, GOG)
                        .unwrap()
                },
            ],
        );
        assert_eq!(jar.cookies().len(), 1);
    }

    #[test]
    fn cookies_go_only_where_they_belong() {
        let c = |name: &str, domain: &str, path: &str, secure: bool| {
            StoredCookie::from_webview(name, "v", domain, path, secure, true, None, GOG).unwrap()
        };
        let mut expired = c("old", ".gog.com", "/", true);
        expired.expires = Some(NOW - 1);
        let jar = CookieJar::new(
            GOG,
            vec![
                c("dom", ".gog.com", "/", true),
                c("host", "www.gog.com", "/", true),
                c("path", ".gog.com", "/account", true),
                c("plain", ".gog.com", "/", false),
                expired,
            ],
        );
        assert_eq!(
            jar.header_for(&url("https://www.gog.com/account/x"), NOW)
                .unwrap(),
            "dom=v; host=v; path=v; plain=v"
        );
        assert_eq!(
            jar.header_for(&url("https://menu.gog.com/v1/account/basic"), NOW)
                .unwrap(),
            "dom=v; plain=v"
        );
        assert_eq!(
            jar.header_for(&url("https://www.gog.com/accountx"), NOW)
                .unwrap(),
            "dom=v; host=v; plain=v"
        );
        assert_eq!(
            jar.header_for(&url("http://www.gog.com/"), NOW).unwrap(),
            "plain=v"
        );
        assert_eq!(jar.header_for(&url("https://evil.example/"), NOW), None);
        // Debug output never shows a value.
        assert!(!format!("{:?}", jar).contains("=v"));
        assert!(format!("{:?}", jar.cookies()[0]).contains("value hidden"));
    }

    #[test]
    fn set_cookie_updates_only_the_stores_domains() {
        let mut jar = CookieJar::new(GOG, Vec::new());
        let from = url("https://www.gog.com/u/x/games/stats");
        jar.absorb(
            &from,
            "sid=abc; Domain=.gog.com; Path=/; Secure; HttpOnly",
            NOW,
        );
        jar.absorb(&from, "local=1", NOW);
        jar.absorb(&from, "evil=1; Domain=evil.example", NOW);
        jar.absorb(&from, "other=1; Domain=menu.gog.com", NOW);
        jar.absorb(&from, "com=1; Domain=com", NOW);
        jar.absorb(&from, "bad name=1", NOW);
        jar.absorb(&from, "noequals", NOW);
        let names: Vec<&str> = jar.cookies().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["sid", "local"]);
        let local = &jar.cookies()[1];
        assert!(local.host_only);
        assert_eq!(
            (local.domain.as_str(), local.path.as_str()),
            ("www.gog.com", "/u/x/games")
        );
        // Replaced, then deleted.
        jar.absorb(&from, "sid=new; Domain=gog.com; Path=/; Max-Age=60", NOW);
        assert_eq!(
            jar.cookies()
                .iter()
                .find(|c| c.name == "sid")
                .unwrap()
                .value,
            "new"
        );
        assert_eq!(
            jar.cookies()
                .iter()
                .find(|c| c.name == "sid")
                .unwrap()
                .expires,
            Some(NOW + 60)
        );
        jar.absorb(&from, "sid=; Domain=gog.com; Path=/; Max-Age=0", NOW);
        assert!(jar.cookies().iter().all(|c| c.name != "sid"));
        jar.absorb(&from, "e=1; Expires=Wed, 21 Oct 2015 07:28:00 GMT", NOW);
        assert!(jar.cookies().iter().all(|c| c.name != "e"));
        // A flood of session cookies is capped by evicting the oldest ones: the newest stays.
        for i in 0..500 {
            jar.absorb(&from, &format!("c{i}=1"), NOW);
        }
        assert_eq!(jar.cookies().len(), MAX_COOKIES);
        assert!(jar.cookies().iter().any(|c| c.name == "c499"));
        assert!(jar.cookies().iter().all(|c| c.name != "c0"));
    }

    #[test]
    fn a_full_jar_evicts_expired_then_session_cookies() {
        let from = url("https://www.gog.com/");
        let mut jar = CookieJar::new(GOG, Vec::new());
        for i in 0..MAX_COOKIES {
            jar.absorb(
                &from,
                &format!("p{i}=1; Max-Age={}", if i == 7 { 10 } else { 100_000 }),
                NOW,
            );
        }
        // Cookie p7 expires at NOW + 10: later it is the one evicted.
        jar.absorb(&from, "fresh=1; Max-Age=100", NOW + 20);
        assert!(jar.cookies().iter().any(|c| c.name == "fresh"));
        assert!(jar.cookies().iter().all(|c| c.name != "p7"));
        // Full of long-lived cookies: one session cookie goes before the new one is dropped.
        jar.absorb(&from, "s1=1", NOW + 20);
        assert!(
            jar.cookies().iter().all(|c| c.name != "s1"),
            "full, no expired or session cookie"
        );
        assert_eq!(jar.cookies().len(), MAX_COOKIES);
    }

    #[test]
    fn cookies_read_from_several_urls_are_merged_once() {
        let c = |name: &str, value: &str, path: &str| {
            StoredCookie::from_webview(name, value, ".gog.com", path, true, true, None, GOG)
                .unwrap()
        };
        let merged = merge(vec![
            vec![c("a", "1", "/"), c("b", "1", "/")],
            vec![c("a", "2", "/"), c("a", "3", "/account"), c("c", "1", "/")],
        ]);
        let shown: Vec<(String, String, String)> = merged
            .iter()
            .map(|c| (c.name.clone(), c.value.clone(), c.path.clone()))
            .collect();
        assert_eq!(
            shown,
            [
                ("a".into(), "1".into(), "/".into()),
                ("b".into(), "1".into(), "/".into()),
                ("a".into(), "3".into(), "/account".into()),
                ("c".into(), "1".into(), "/".into()),
            ]
        );
    }

    #[test]
    fn http_dates_parse() {
        assert_eq!(http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            http_date("Wed, 21-Oct-2015 07:28:00 GMT"),
            Some(1_445_412_480)
        );
        assert_eq!(http_date("Wed, 21 Oct 2015 07:28:00 PST"), None);
        assert_eq!(http_date("soon"), None);
    }
}
