//! GOG account through GOG Galaxy's public OAuth client (unofficial; see [`super::launcher_clients`]).
//!
//! Sign-in happens in the user's own browser on `auth.gog.com` (any method GOG offers: e-mail,
//! Google, Discord, ...). GOG then shows `https://embed.gog.com/on_login_success?origin=client&code=…`;
//! the user pastes that address (or the code) into GameLobbies, which checks it strictly
//! ([`parse_pasted`]) and exchanges it once at `auth.gog.com/token` for an access and a refresh
//! token (sealed with DPAPI). The renderer never sees a token; the code is never logged.
//!
//! With the bearer token:
//! - `https://embed.gog.com/userData.json`: `isLoggedIn`, `username` (the probe);
//! - `https://embed.gog.com/user/data/games`: `owned`, every owned product id;
//! - `https://api.gog.com/products?ids=…`: titles, 50 ids at a time;
//! - `https://gameplay.gog.com/games/<id>/users/<user>/sessions`: `time_sum`, minutes played
//!   (what Heroic reads), one request per game, for at most [`MAX_PLAYTIME_GAMES`] games.
//!
//! Ids are GOG product ids (digits), so `gog:<id>` matches the GOG Galaxy scan; links are
//! `goggalaxy://openGameView/<id>`.

use super::apply::{FetchOutcome, OwnedGame};
use super::http::{clean_name, FetchError, StoreHttp, MAX_GAMES, MAX_PAGES};
use super::launcher_clients::{GOG_CLIENT_ID, GOG_CLIENT_SECRET, GOG_REDIRECT_URI};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use url::Url;

/// Hosts the bearer token is sent to.
pub const API_HOSTS: &[&str] = &["embed.gog.com", "api.gog.com", "gameplay.gog.com"];
/// The token endpoint's host (no bearer token is sent there).
pub const TOKEN_HOSTS: &[&str] = &["auth.gog.com"];
const TITLE_BATCH: usize = 50;
/// Games whose playtime is asked for per sync (one request each, 300 ms apart).
pub const MAX_PLAYTIME_GAMES: usize = 500;
/// A minute count above this (about 19 years) is not believable.
const MAX_MINUTES: u64 = 10_000_000;
const MAX_PASTE: usize = 4096;

static USERNAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,64}$").expect("valid regex"));
static PRODUCT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{1,20}$").expect("valid regex"));
static CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{16,1024}$").expect("valid regex"));
static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._~+/=-]{16,4096}$").expect("valid regex"));

/// Where requests go. Production: GOG's hosts over https; tests: a local server.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub auth: Url,
    pub embed: Url,
    pub api: Url,
    pub gameplay: Url,
}

impl Endpoints {
    pub fn production() -> Self {
        let url = |u: &str| Url::parse(u).expect("valid URL");
        Self {
            auth: url("https://auth.gog.com/"),
            embed: url("https://embed.gog.com/"),
            api: url("https://api.gog.com/"),
            gameplay: url("https://gameplay.gog.com/"),
        }
    }

    #[cfg(test)]
    pub fn local(base: &str) -> Self {
        let url = |p: &str| Url::parse(&format!("{base}/{p}/")).unwrap();
        Self {
            auth: url("auth"),
            embed: url("embed"),
            api: url("api"),
            gameplay: url("gameplay"),
        }
    }
}

fn join(base: &Url, path: &str) -> Url {
    base.join(path).expect("valid path")
}

/// The page the browser opens: GOG's sign-in for the Galaxy client, with its fixed redirect.
pub fn auth_url() -> Url {
    let mut url = Url::parse("https://auth.gog.com/auth").expect("valid URL");
    url.query_pairs_mut()
        .append_pair("client_id", GOG_CLIENT_ID)
        .append_pair("redirect_uri", GOG_REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("layout", "client2");
    url
}

/// Whether `url` is exactly [`auth_url`] (the only GOG page GameLobbies opens in the browser).
pub fn is_auth_url(url: &Url) -> bool {
    *url == auth_url()
}

/// Why a pasted address was refused. Fixed text that never repeats the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteError {
    Empty,
    TooLong,
    WrongAddress,
    NoCode,
}

impl PasteError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Paste the address GOG showed after you signed in.",
            Self::TooLong => "That is too long to be GOG's sign-in address.",
            Self::WrongAddress => "That is not GOG's sign-in address. It starts with https://embed.gog.com/on_login_success.",
            Self::NoCode => "That address has no sign-in code. Sign in again and paste the whole address.",
        }
    }
}

/// The sign-in code from what the user pasted: the whole address
/// (`https://embed.gog.com/on_login_success?origin=client&code=…`, exact host and path, https,
/// no port or user info, exactly one `code`, nothing but `origin` and `code`) or the bare code.
pub fn parse_pasted(input: &str) -> Result<String, PasteError> {
    if input.len() > MAX_PASTE {
        return Err(PasteError::TooLong);
    }
    let text = input.trim();
    if text.is_empty() {
        return Err(PasteError::Empty);
    }
    if CODE.is_match(text) {
        return Ok(text.to_string());
    }
    let url = Url::parse(text).map_err(|_| PasteError::WrongAddress)?;
    if url.scheme() != "https"
        || url.host_str() != Some("embed.gog.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/on_login_success"
        || url.fragment().is_some()
    {
        return Err(PasteError::WrongAddress);
    }
    let mut codes = Vec::new();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => codes.push(value.into_owned()),
            "origin" if value == "client" => {}
            _ => return Err(PasteError::WrongAddress),
        }
    }
    match codes.as_slice() {
        [code] if CODE.is_match(code) => Ok(code.clone()),
        [] => Err(PasteError::NoCode),
        _ => Err(PasteError::WrongAddress),
    }
}

/// The tokens of a GOG sign-in.
#[derive(Clone, PartialEq)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// Unix ms.
    pub expires_at: i64,
    pub user_id: String,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tokens(user {}, values hidden)", self.user_id)
    }
}

/// A token response, checked: plain token shapes, a numeric user id, a sane lifetime.
pub fn parse_token_response(body: &[u8], now_ms: i64) -> Result<Tokens, FetchError> {
    #[derive(Deserialize)]
    struct Response {
        access_token: Option<String>,
        refresh_token: Option<String>,
        expires_in: Option<i64>,
        user_id: Option<Value>,
    }
    let r: Response = serde_json::from_slice(body)
        .map_err(|_| FetchError::Invalid("the sign-in answer is not the expected JSON."))?;
    let access = r.access_token.filter(|t| TOKEN.is_match(t));
    let refresh = r.refresh_token.filter(|t| TOKEN.is_match(t));
    let user_id = match r.user_id {
        Some(Value::String(s)) => Some(s),
        Some(Value::Number(n)) => n.as_u64().map(|n| n.to_string()),
        _ => None,
    }
    .filter(|id| PRODUCT_ID.is_match(id));
    let (Some(access), Some(refresh), Some(user_id)) = (access, refresh, user_id) else {
        return Err(FetchError::Invalid(
            "the sign-in answer is missing its tokens.",
        ));
    };
    let lifetime = r.expires_in.unwrap_or(3600).clamp(60, 30 * 86_400);
    Ok(Tokens {
        access,
        refresh,
        expires_at: now_ms + lifetime * 1000,
        user_id,
    })
}

async fn token_request(
    http: &StoreHttp,
    endpoints: &Endpoints,
    grant: &[(&str, &str)],
) -> Result<Tokens, FetchError> {
    let mut url = join(&endpoints.auth, "token");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", GOG_CLIENT_ID);
        q.append_pair("client_secret", GOG_CLIENT_SECRET);
        for (k, v) in grant {
            q.append_pair(k, v);
        }
    }
    match http.get(url).await {
        Ok(body) => parse_token_response(&body, crate::types::now_ms()),
        // GOG answers a used, expired or foreign code (or refresh token) with 400 or 401.
        Err(FetchError::Status(400 | 401)) => Err(FetchError::Rejected),
        Err(e) => Err(e),
    }
}

/// Exchanges a sign-in code (once) for tokens.
pub async fn exchange(
    http: &StoreHttp,
    endpoints: &Endpoints,
    code: &str,
) -> Result<Tokens, FetchError> {
    token_request(
        http,
        endpoints,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", GOG_REDIRECT_URI),
        ],
    )
    .await
}

/// New tokens from the refresh token.
pub async fn refresh(
    http: &StoreHttp,
    endpoints: &Endpoints,
    refresh_token: &str,
) -> Result<Tokens, FetchError> {
    token_request(
        http,
        endpoints,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
    .await
}

pub fn launch_link(product_id: &str) -> Option<String> {
    PRODUCT_ID
        .is_match(product_id)
        .then(|| format!("goggalaxy://openGameView/{product_id}"))
}

fn json(body: &[u8]) -> Result<Value, FetchError> {
    serde_json::from_slice(body).map_err(|_| FetchError::Invalid("it is not the expected JSON."))
}

/// The signed-in user name, or [`FetchError::Rejected`] when nobody is signed in.
pub fn parse_basic(body: &[u8]) -> Result<String, FetchError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Basic {
        #[serde(default)]
        is_logged_in: bool,
        #[serde(default)]
        username: Option<String>,
    }
    let basic: Basic = serde_json::from_slice(body)
        .map_err(|_| FetchError::Invalid("it is not the expected JSON."))?;
    if !basic.is_logged_in {
        return Err(FetchError::Rejected);
    }
    basic
        .username
        .filter(|u| USERNAME.is_match(u))
        .ok_or(FetchError::Invalid("the user name is missing or unusual."))
}

/// Every owned product id (`owned`: numbers or digit strings; anything else is skipped).
pub fn parse_owned(body: &[u8]) -> Result<HashSet<String>, FetchError> {
    let value = json(body)?;
    let list = value
        .get("owned")
        .and_then(Value::as_array)
        .ok_or(FetchError::Invalid("the list of owned games is missing."))?;
    if list.len() > MAX_GAMES {
        return Err(FetchError::Invalid("it lists too many games."));
    }
    Ok(list
        .iter()
        .filter_map(|v| match v {
            Value::Number(n) => n.as_u64().map(|n| n.to_string()),
            Value::String(s) => Some(s.clone()),
            _ => None,
        })
        .filter(|id| PRODUCT_ID.is_match(id))
        .collect())
}

/// `products?ids=…`: (id, title) of each game (DLC, packs and other types are skipped).
pub fn parse_products(body: &[u8]) -> Result<Vec<(String, String)>, FetchError> {
    let value = json(body)?;
    let list = value.as_array().ok_or(FetchError::Invalid(
        "the product list is not the expected JSON.",
    ))?;
    Ok(list
        .iter()
        .take(TITLE_BATCH)
        .filter(|p| {
            p.get("game_type")
                .and_then(Value::as_str)
                .is_none_or(|t| t == "game")
        })
        .filter_map(|p| {
            let id = match p.get("id") {
                Some(Value::Number(n)) => n.as_u64().map(|n| n.to_string()),
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            }
            .filter(|id| PRODUCT_ID.is_match(id))?;
            let title = p
                .get("title")
                .and_then(Value::as_str)
                .and_then(clean_name)?;
            Some((id, title))
        })
        .collect())
}

/// `time_sum` (minutes) of a sessions answer, capped; `None` when it has none.
pub fn parse_time_sum(body: &[u8]) -> Option<i64> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let minutes = value.get("time_sum")?.as_u64()?.min(MAX_MINUTES);
    Some(minutes as i64)
}

/// Checks that the token works; the user name.
pub async fn probe(http: &StoreHttp, endpoints: &Endpoints) -> Result<String, FetchError> {
    parse_basic(&http.get(join(&endpoints.embed, "userData.json")).await?)
}

/// The whole library. `complete` when the owned list and every titles batch were read. Playtime
/// is asked for game by game, the games checked longest ago (or never, `ages`) first, until
/// [`MAX_PLAYTIME_GAMES`] or `deadline`: a large library is covered over several syncs, and the
/// owned games and titles are returned in time either way.
pub async fn fetch(
    http: &StoreHttp,
    endpoints: &Endpoints,
    user_id: &str,
    ages: &HashMap<String, i64>,
    deadline: std::time::Instant,
) -> Result<FetchOutcome, FetchError> {
    probe(http, endpoints).await?;
    let owned = parse_owned(&http.get(join(&endpoints.embed, "user/data/games")).await?)?;
    let mut ids: Vec<String> = owned.iter().cloned().collect();
    ids.sort();
    let mut titles: HashMap<String, String> = HashMap::new();
    let mut complete = true;
    for (n, batch) in ids.chunks(TITLE_BATCH).enumerate() {
        if n >= MAX_PAGES {
            complete = false;
            break;
        }
        let mut url = join(&endpoints.api, "products");
        url.query_pairs_mut().append_pair("ids", &batch.join(","));
        match http.get(url).await.and_then(|body| parse_products(&body)) {
            Ok(found) => titles.extend(found),
            Err(FetchError::Rejected) => return Err(FetchError::Rejected),
            Err(_) => {
                complete = false;
                break;
            }
        }
    }
    let mut playtime: HashMap<String, i64> = HashMap::new();
    let mut checked: HashSet<String> = HashSet::new();
    if PRODUCT_ID.is_match(user_id) {
        let mut queue: Vec<&String> = titles.keys().collect();
        queue.sort_by_key(|id| (ages.get(*id).copied().unwrap_or(i64::MIN), (*id).clone()));
        for id in queue.into_iter().take(MAX_PLAYTIME_GAMES) {
            if std::time::Instant::now() >= deadline {
                break;
            }
            let url = join(
                &endpoints.gameplay,
                &format!("games/{id}/users/{user_id}/sessions"),
            );
            match http.get(url).await {
                Ok(body) => {
                    if let Some(minutes) = parse_time_sum(&body).filter(|m| *m > 0) {
                        playtime.insert(id.clone(), minutes * 60);
                    }
                    checked.insert(id.clone());
                }
                // Never played: no sessions.
                Err(FetchError::Status(404)) => {
                    checked.insert(id.clone());
                }
                Err(FetchError::Rejected) => return Err(FetchError::Rejected),
                Err(_) => break,
            }
        }
    }
    let mut games: Vec<OwnedGame> = titles
        .into_iter()
        .map(|(id, title)| OwnedGame {
            launch: launch_link(&id),
            playtime_seconds: playtime.get(&id).copied(),
            last_played_ms: None,
            store_id: id,
            title,
        })
        .collect();
    games.sort_by(|a, b| a.store_id.cmp(&b.store_id));
    Ok(FetchOutcome {
        games,
        owned_ids: owned,
        complete,
        playtime_checked: checked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::http::tests::{ok, store_server};

    const CODE_TEXT: &str = "AbCdEf0123456789_-xyzAbCdEf0123456789";

    #[test]
    fn the_browser_opens_exactly_gogs_galaxy_sign_in() {
        let url = auth_url();
        assert_eq!(
            url.as_str(),
            "https://auth.gog.com/auth?client_id=46899977096215655&redirect_uri=https%3A%2F%2Fembed.gog.com%2Fon_login_success%3Forigin%3Dclient&response_type=code&layout=client2"
        );
        assert!(is_auth_url(&url));
        assert!(!is_auth_url(
            &Url::parse("https://auth.gog.com/auth").unwrap()
        ));
    }

    #[test]
    fn pasted_addresses_are_checked_strictly() {
        let ok_url =
            format!("https://embed.gog.com/on_login_success?origin=client&code={CODE_TEXT}");
        assert_eq!(parse_pasted(&ok_url), Ok(CODE_TEXT.into()));
        assert_eq!(parse_pasted(&format!("  {ok_url}\n")), Ok(CODE_TEXT.into()));
        assert_eq!(parse_pasted(CODE_TEXT), Ok(CODE_TEXT.into()));
        for (bad, why) in [
            ("".to_string(), PasteError::Empty),
            ("x".repeat(5000), PasteError::TooLong),
            (
                format!("http://embed.gog.com/on_login_success?code={CODE_TEXT}"),
                PasteError::WrongAddress,
            ),
            (
                format!("https://embed.gog.com.evil.example/on_login_success?code={CODE_TEXT}"),
                PasteError::WrongAddress,
            ),
            (
                format!("https://evil.example/on_login_success?code={CODE_TEXT}"),
                PasteError::WrongAddress,
            ),
            (
                format!("https://embed.gog.com:444/on_login_success?code={CODE_TEXT}"),
                PasteError::WrongAddress,
            ),
            (
                format!("https://u@embed.gog.com/on_login_success?code={CODE_TEXT}"),
                PasteError::WrongAddress,
            ),
            (
                format!("https://embed.gog.com/other?code={CODE_TEXT}"),
                PasteError::WrongAddress,
            ),
            (
                format!("https://embed.gog.com/on_login_success?code={CODE_TEXT}&code={CODE_TEXT}"),
                PasteError::WrongAddress,
            ),
            (
                format!("https://embed.gog.com/on_login_success?code={CODE_TEXT}&x=1"),
                PasteError::WrongAddress,
            ),
            (
                format!("https://embed.gog.com/on_login_success?code={CODE_TEXT}#f"),
                PasteError::WrongAddress,
            ),
            (
                "https://embed.gog.com/on_login_success?code=a%20b".to_string(),
                PasteError::WrongAddress,
            ),
            (
                "https://embed.gog.com/on_login_success?origin=client".to_string(),
                PasteError::NoCode,
            ),
            ("short".to_string(), PasteError::WrongAddress),
        ] {
            assert_eq!(
                parse_pasted(&bad),
                Err(why),
                "{}",
                &bad[..bad.len().min(80)]
            );
            assert!(!why.message().contains(CODE_TEXT));
        }
    }

    fn token_body(access: &str) -> String {
        format!(
            r#"{{"expires_in":3600,"token_type":"bearer","access_token":"{access}","user_id":"48628349957132247","refresh_token":"REFRESHrefresh0123456789","session_id":"s"}}"#
        )
    }

    #[test]
    fn token_answers_are_checked() {
        let t =
            parse_token_response(token_body("ACCESSaccess0123456789").as_bytes(), 1000).unwrap();
        assert_eq!(
            (t.user_id.as_str(), t.expires_at),
            ("48628349957132247", 1000 + 3_600_000)
        );
        assert!(!format!("{t:?}").contains("ACCESS"));
        for bad in [
            &br#"{"access_token":"short","refresh_token":"REFRESHrefresh0123456789","user_id":"1"}"#[..],
            br#"{"access_token":"ACCESSaccess0123456789","refresh_token":"REFRESHrefresh0123456789","user_id":"x1"}"#,
            br#"{"access_token":"ACCESS access 0123456789","refresh_token":"REFRESHrefresh0123456789","user_id":"1"}"#,
            br#"{"error":"invalid_grant"}"#,
            b"<html>",
        ] {
            assert!(parse_token_response(bad, 0).is_err());
        }
        // A silly lifetime is clamped.
        let t = parse_token_response(br#"{"expires_in":-5,"access_token":"ACCESSaccess0123456789","refresh_token":"REFRESHrefresh0123456789","user_id":7}"#, 0).unwrap();
        assert_eq!(t.expires_at, 60_000);
    }

    #[tokio::test]
    async fn codes_and_refresh_tokens_are_exchanged_at_the_token_endpoint() {
        let http = StoreHttp::local(Default::default());
        let (base2, seen2) = crate::accounts::http::tests::store_server_any(ok(
            &token_body("ACCESSaccess0123456789"),
            "",
        ))
        .await;
        let endpoints = Endpoints::local(&base2);
        let tokens = exchange(&http, &endpoints, CODE_TEXT).await.unwrap();
        assert_eq!(tokens.access, "ACCESSaccess0123456789");
        let request = seen2.lock().unwrap()[0].clone();
        assert!(request.starts_with("get /auth/token?"), "{request}");
        for part in [
            "client_id=46899977096215655",
            "grant_type=authorization_code",
            &format!("code={}", CODE_TEXT.to_ascii_lowercase()),
            "redirect_uri=https%3a%2f%2fembed.gog.com%2fon_login_success%3forigin%3dclient",
        ] {
            assert!(request.contains(part), "{part} in {request}");
        }
        refresh(&http, &endpoints, "REFRESHrefresh0123456789")
            .await
            .unwrap();
        assert!(seen2.lock().unwrap()[1].contains("grant_type=refresh_token"));
        // The token endpoint never gets a bearer token.
        assert!(seen2
            .lock()
            .unwrap()
            .iter()
            .all(|h| !h.contains("authorization:")));
        // A used code: 400 reads as rejected.
        let (base3, _) = crate::accounts::http::tests::store_server_any(
            "HTTP/1.1 400 X\r\nContent-Length: 27\r\nConnection: close\r\n\r\n{\"error\":\"invalid_grant\"}  ".into(),
        )
        .await;
        assert_eq!(
            exchange(&http, &Endpoints::local(&base3), CODE_TEXT).await,
            Err(FetchError::Rejected)
        );
    }

    #[tokio::test]
    async fn the_library_titles_and_playtime_come_from_the_token_apis() {
        let products = r#"[{"id":1207658924,"title":"The Witcher","game_type":"game"},{"id":"1495134320","title":"Gwent"},{"id":5,"title":"A DLC","game_type":"dlc"}]"#;
        let (base, seen) = store_server(vec![
            (
                "/embed/userData.json",
                ok(r#"{"isLoggedIn":true,"username":"gamer"}"#, ""),
            ),
            (
                "/embed/user/data/games",
                ok(r#"{"owned":[1207658924,1495134320,5]}"#, ""),
            ),
            (
                "/api/products?ids=1207658924%2C1495134320%2C5",
                ok(products, ""),
            ),
            (
                "/gameplay/games/1207658924/users/42/sessions",
                ok(r#"{"time_sum":90,"sessions":[]}"#, ""),
            ),
        ])
        .await;
        let http = StoreHttp::local(Default::default()).with_bearer("ACCESSaccess0123456789");
        let far = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let out = fetch(&http, &Endpoints::local(&base), "42", &HashMap::new(), far)
            .await
            .unwrap();
        assert!(out.complete);
        // Gwent's sessions are a 404 (never played): checked too.
        assert_eq!(out.playtime_checked.len(), 2);
        assert_eq!(out.owned_ids.len(), 3);
        let titles: Vec<&str> = out.games.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["The Witcher", "Gwent"]);
        assert_eq!(out.games[0].playtime_seconds, Some(5400));
        assert_eq!(out.games[1].playtime_seconds, None);
        assert_eq!(
            out.games[0].launch.as_deref(),
            Some("goggalaxy://openGameView/1207658924")
        );
        let seen = seen.lock().unwrap();
        assert!(seen
            .iter()
            .all(|h| h.contains("authorization: bearer accessaccess0123456789")));
    }

    #[tokio::test]
    async fn playtime_stops_at_the_deadline_and_goes_oldest_first() {
        let (base, seen) = store_server(vec![
            (
                "/embed/userData.json",
                ok(r#"{"isLoggedIn":true,"username":"gamer"}"#, ""),
            ),
            ("/embed/user/data/games", ok(r#"{"owned":[1,2,3]}"#, "")),
            (
                "/api/products?ids=1%2C2%2C3",
                ok(
                    r#"[{"id":1,"title":"A"},{"id":2,"title":"B"},{"id":3,"title":"C"}]"#,
                    "",
                ),
            ),
        ])
        .await;
        let http = StoreHttp::local(Default::default()).with_bearer("ACCESSaccess0123456789");
        // Past the deadline: the games and titles still come back, no playtime is asked for.
        let past = std::time::Instant::now();
        let out = fetch(&http, &Endpoints::local(&base), "42", &HashMap::new(), past)
            .await
            .unwrap();
        assert!(out.complete);
        assert_eq!(out.games.len(), 3);
        assert!(out.playtime_checked.is_empty());
        assert!(!seen
            .lock()
            .unwrap()
            .iter()
            .any(|h| h.contains("/gameplay/")));
        // With time: never-checked games first, then the oldest.
        seen.lock().unwrap().clear();
        let ages: HashMap<String, i64> = [("1".to_string(), 500), ("2".to_string(), 100)].into();
        let far = std::time::Instant::now() + std::time::Duration::from_secs(60);
        fetch(&http, &Endpoints::local(&base), "42", &ages, far)
            .await
            .unwrap();
        let order: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|h| {
                h.split("/gameplay/games/")
                    .nth(1)
                    .map(|r| r.split('/').next().unwrap().to_string())
            })
            .collect();
        assert_eq!(order, ["3", "2", "1"]);
    }

    #[test]
    fn hostile_answers_are_errors_not_panics() {
        assert_eq!(
            parse_basic(br#"{"isLoggedIn":false}"#),
            Err(FetchError::Rejected)
        );
        assert!(parse_basic(br#"{"isLoggedIn":true,"username":"../x"}"#).is_err());
        assert!(parse_owned(br#"{"other":[]}"#).is_err());
        let huge = format!(r#"{{"owned":[{}]}}"#, vec!["1"; MAX_GAMES + 1].join(","));
        assert!(parse_owned(huge.as_bytes()).is_err());
        let ids = parse_owned(br#"{"owned":[1,"2","../x",-1,1.5,null]}"#).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(parse_products(b"{}").is_err());
        assert_eq!(
            parse_products(br#"[{"id":"x","title":"A"},{"id":1,"title":"  "},{"id":2}]"#).unwrap(),
            []
        );
        assert_eq!(
            parse_time_sum(br#"{"time_sum":99999999999}"#),
            Some(MAX_MINUTES as i64)
        );
        assert_eq!(parse_time_sum(br#"{"time_sum":-1}"#), None);
        assert_eq!(parse_time_sum(b"<html>"), None);
        let link = launch_link("1207658924").unwrap();
        assert_eq!(crate::launch::validate_launch_uri(&link).unwrap(), link);
        assert!(crate::validation::renderer_link_ok(&link));
        assert_eq!(launch_link("12a"), None);
    }
}
