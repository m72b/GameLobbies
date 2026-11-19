//! GOG account (cookie session from the sign-in window): who is signed in, which games the
//! account owns, their titles and GOG's playtime. Unofficial web endpoints used by GOG's own
//! site; they may change without notice.
//!
//! - `https://menu.gog.com/v1/account/basic`: `isLoggedIn`, `username` (the probe).
//! - `https://embed.gog.com/user/data/games`: `owned`, every owned product id.
//! - `https://www.gog.com/u/<username>/games/stats?sort=recent_playtime&order=desc&page=<n>`:
//!   titles, minutes played and the last session, page by page.
//!
//! Ids are GOG product ids (digits only), so `gog:<id>` matches the GOG Galaxy scan; links are
//! `goggalaxy://openGameView/<id>`.

use super::apply::{FetchOutcome, OwnedGame};
use super::http::{clean_name, FetchError, StoreHttp, MAX_GAMES, MAX_PAGES};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use url::Url;

pub const API_HOSTS: &[&str] = &["menu.gog.com", "embed.gog.com", "www.gog.com"];
pub const COOKIE_DOMAINS: &[&str] = &["gog.com"];
/// Where the sign-in window starts (GOG shows its sign-in when not signed in).
pub const START_URL: &str = "https://www.gog.com/account";
/// A minute count above this (about 19 years) is not believable.
const MAX_MINUTES: u64 = 10_000_000;

static USERNAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,64}$").expect("valid regex"));
static PRODUCT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{1,20}$").expect("valid regex"));

/// Whether the sign-in window reached the signed-in account page (`www.gog.com/account`, also
/// with a language prefix such as `/en/account`).
pub fn signed_in_page(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("www.gog.com")
        && url
            .path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).collect::<Vec<_>>())
            .is_some_and(|s| match s.as_slice() {
                ["account", ..] => true,
                [lang, "account", ..] => {
                    lang.len() == 2 && lang.bytes().all(|b| b.is_ascii_lowercase())
                }
                _ => false,
            })
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

/// Unix ms of GOG's `lastSession` (`2021-03-04T19:33:22+0000` or `…Z`).
fn last_session_ms(text: &str) -> Option<i64> {
    let (stamp, zone) = text.split_at_checked(19)?;
    if !matches!(zone, "+0000" | "+00:00" | "Z") {
        return None;
    }
    super::steam_openid::nonce_time_ms(&format!("{stamp}Z"))
}

/// One stats page: its games (title, minutes, last session) and the number of pages.
pub fn parse_stats_page(body: &[u8]) -> Result<(Vec<OwnedGame>, usize), FetchError> {
    let value = json(body)?;
    let pages = value
        .get("pages")
        .and_then(Value::as_u64)
        .map_or(1, |p| p as usize)
        .clamp(1, MAX_PAGES);
    let items = value
        .pointer("/_embedded/items")
        .and_then(Value::as_array)
        .ok_or(FetchError::Invalid("the list of games is missing."))?;
    let mut games = Vec::new();
    for item in items.iter().take(MAX_GAMES) {
        let Some(game) = item.get("game") else {
            continue;
        };
        let id = match game.get("id") {
            Some(Value::Number(n)) => n.as_u64().map(|n| n.to_string()),
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let Some(id) = id.filter(|id| PRODUCT_ID.is_match(id)) else {
            continue;
        };
        let Some(title) = game
            .get("title")
            .and_then(Value::as_str)
            .and_then(clean_name)
        else {
            continue;
        };
        // Stats are keyed by the user's id; take the largest figure there is.
        let stats: Vec<&Value> = item
            .get("stats")
            .and_then(Value::as_object)
            .map(|m| m.values().collect())
            .unwrap_or_default();
        let minutes = stats
            .iter()
            .filter_map(|s| s.get("playtime").and_then(Value::as_u64))
            .max()
            .unwrap_or(0)
            .min(MAX_MINUTES);
        let last = stats
            .iter()
            .filter_map(|s| s.get("lastSession").and_then(Value::as_str))
            .filter_map(last_session_ms)
            .max();
        games.push(OwnedGame {
            launch: launch_link(&id),
            store_id: id,
            title,
            playtime_seconds: (minutes > 0).then(|| minutes as i64 * 60),
            last_played_ms: last,
        });
    }
    Ok((games, pages))
}

/// Checks that the session works; the user name.
pub async fn probe(http: &StoreHttp) -> Result<String, FetchError> {
    let url = Url::parse("https://menu.gog.com/v1/account/basic").expect("valid URL");
    parse_basic(&http.get(url).await?)
}

/// The whole library. `complete` only when the owned list and every stats page were read.
pub async fn fetch(http: &StoreHttp) -> Result<FetchOutcome, FetchError> {
    let username = probe(http).await?;
    let owned = parse_owned(
        &http
            .get(Url::parse("https://embed.gog.com/user/data/games").expect("valid URL"))
            .await?,
    )?;
    let mut games: HashMap<String, OwnedGame> = HashMap::new();
    let mut complete = true;
    let mut page = 1;
    let mut pages = 1;
    while page <= pages && page <= MAX_PAGES {
        let mut url = Url::parse("https://www.gog.com/").expect("valid URL");
        url.set_path(&format!("u/{username}/games/stats"));
        url.query_pairs_mut()
            .append_pair("sort", "recent_playtime")
            .append_pair("order", "desc")
            .append_pair("page", &page.to_string());
        match http.get(url).await.and_then(|body| parse_stats_page(&body)) {
            Ok((list, total)) => {
                pages = total;
                for game in list {
                    if games.len() >= MAX_GAMES {
                        break;
                    }
                    games.entry(game.store_id.clone()).or_insert(game);
                }
            }
            Err(e) if page == 1 => return Err(e),
            Err(_) => {
                complete = false;
                break;
            }
        }
        page += 1;
    }
    let mut owned_ids = owned;
    // A titled game the stats list shows is owned even if the owned list missed it.
    owned_ids.extend(games.keys().cloned());
    let mut games: Vec<OwnedGame> = games.into_values().collect();
    games.sort_by(|a, b| a.store_id.cmp(&b.store_id));
    Ok(FetchOutcome {
        games,
        owned_ids,
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_needs_a_signed_in_user_with_a_plain_name() {
        assert_eq!(
            parse_basic(br#"{"isLoggedIn":true,"username":"gamer_1"}"#),
            Ok("gamer_1".into())
        );
        assert_eq!(
            parse_basic(br#"{"isLoggedIn":false}"#),
            Err(FetchError::Rejected)
        );
        for bad in [
            &br#"{"isLoggedIn":true,"username":"../x"}"#[..],
            br#"{"isLoggedIn":true,"username":"a b"}"#,
            br#"{"isLoggedIn":true}"#,
            b"<html>",
        ] {
            assert!(matches!(parse_basic(bad), Err(FetchError::Invalid(_))));
        }
    }

    #[test]
    fn owned_ids_are_digits_only() {
        let ids =
            parse_owned(br#"{"owned":[1207658924,"1495134320","../x",-1,1.5,null]}"#).unwrap();
        let mut ids: Vec<_> = ids.into_iter().collect();
        ids.sort();
        assert_eq!(ids, ["1207658924", "1495134320"]);
        assert!(parse_owned(br#"{"other":[]}"#).is_err());
        let huge = format!(r#"{{"owned":[{}]}}"#, vec!["1"; MAX_GAMES + 1].join(","));
        assert!(parse_owned(huge.as_bytes()).is_err());
    }

    #[test]
    fn stats_pages_give_titles_playtime_and_last_session() {
        let body = br#"{"page":1,"pages":3,"_embedded":{"items":[
            {"game":{"id":"1207658924","title":"  The Witcher  "},"stats":{"4711":{"playtime":90,"lastSession":"2021-03-04T19:33:22+0000"}}},
            {"game":{"id":1495134320,"title":"Gwent"},"stats":[]},
            {"game":{"id":"x1","title":"Bad id"}},
            {"game":{"id":"5","title":"   "}},
            {"game":{"id":"6","title":"Huge"},"stats":{"1":{"playtime":99999999999}}},
            {"nogame":true}
        ]}}"#;
        let (games, pages) = parse_stats_page(body).unwrap();
        assert_eq!(pages, 3);
        assert_eq!(games.len(), 3);
        assert_eq!(games[0].title, "The Witcher");
        assert_eq!(games[0].playtime_seconds, Some(5400));
        assert_eq!(games[0].last_played_ms, Some(1_614_886_402_000));
        assert_eq!(
            games[0].launch.as_deref(),
            Some("goggalaxy://openGameView/1207658924")
        );
        assert_eq!(games[1].playtime_seconds, None);
        assert_eq!(games[2].playtime_seconds, Some(MAX_MINUTES as i64 * 60));
        // Pages are capped; hostile shapes are errors, not panics.
        let (_, pages) =
            parse_stats_page(br#"{"pages":99999999,"_embedded":{"items":[]}}"#).unwrap();
        assert_eq!(pages, MAX_PAGES);
        for bad in [&b"[]"[..], br#"{"_embedded":{"items":7}}"#, b"\xff\xfe"] {
            assert!(parse_stats_page(bad).is_err());
        }
    }

    #[test]
    fn links_and_signed_in_pages() {
        let link = launch_link("1207658924").unwrap();
        assert_eq!(crate::launch::validate_launch_uri(&link).unwrap(), link);
        assert!(crate::validation::renderer_link_ok(&link));
        assert_eq!(launch_link("12a"), None);
        let page = |u: &str| signed_in_page(&Url::parse(u).unwrap());
        assert!(page("https://www.gog.com/account"));
        assert!(page("https://www.gog.com/en/account/settings"));
        for bad in [
            "https://www.gog.com/",
            "https://login.gog.com/account",
            "http://www.gog.com/account",
            "https://www.gog.com/english/account",
        ] {
            assert!(!page(bad), "{bad}");
        }
        assert_eq!(last_session_ms("2021-03-04T19:33:22-0500"), None);
    }
}
