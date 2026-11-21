//! Humble account (cookie session from the sign-in window): the games of the user's orders,
//! from the pages Humble's own library uses (unofficial; they may change):
//!
//! - `https://www.humblebundle.com/home/library`: the order keys (`gamekeys`), read from the
//!   one `<script id="user-home-json-data" type="application/json">` block only (the probe);
//! - `https://www.humblebundle.com/api/v1/orders?all_tpkds=true&gamekeys=…`, 40 keys at a time.
//!
//! Kept: products with a Windows download, and Steam keys. Humble has no launcher, so these
//! games are listed as owned with no way to start them from Game Lobby.

use super::apply::{FetchOutcome, OwnedGame};
use super::http::{clean_name, FetchError, StoreHttp, MAX_GAMES, MAX_PAGES};
use crate::launch::OWNED_ONLY_PREFIX;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use url::Url;

pub const API_HOSTS: &[&str] = &["www.humblebundle.com"];
pub const COOKIE_DOMAINS: &[&str] = &["humblebundle.com"];
pub const START_URL: &str = "https://www.humblebundle.com/login?goto=%2Fhome%2Flibrary";
/// Order keys per orders request.
const BATCH: usize = 40;
/// The JSON block is read only up to this size.
const MAX_BLOCK: usize = 4 * 1024 * 1024;
const MAX_KEYS: usize = 10_000;
const MARKER: &str = "id=\"user-home-json-data\"";

static GAMEKEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9]{1,64}$").expect("valid regex"));
static MACHINE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,128}$").expect("valid regex"));

/// Whether the sign-in window reached the signed-in library.
pub fn signed_in_page(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("www.humblebundle.com")
        && url.path().trim_end_matches('/') == "/home/library"
}

/// What the library page's data block gives.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Library {
    pub keys: Vec<String>,
    /// The account's e-mail or name, when the block has one.
    pub account: Option<String>,
}

/// The order keys (and the account name) from the library page: the JSON of the single
/// `user-home-json-data` block. No block (not signed in): [`FetchError::Rejected`].
pub fn parse_library_page(html: &[u8]) -> Result<Library, FetchError> {
    let text = String::from_utf8_lossy(html);
    let mut blocks = text.match_indices(MARKER);
    let Some((at, _)) = blocks.next() else {
        return Err(FetchError::Rejected);
    };
    if blocks.next().is_some() {
        return Err(FetchError::Invalid("the library page has two data blocks."));
    }
    let rest = &text[at..];
    let open = rest
        .find('>')
        .ok_or(FetchError::Invalid("the library data block is cut off."))?;
    let body = &rest[open + 1..];
    let close = body
        .find("</script>")
        .ok_or(FetchError::Invalid("the library data block is cut off."))?;
    if close > MAX_BLOCK {
        return Err(FetchError::Invalid("the library data block is too large."));
    }
    let value: Value = serde_json::from_str(body[..close].trim())
        .map_err(|_| FetchError::Invalid("the library data is not the expected JSON."))?;
    let keys = value
        .get("gamekeys")
        .and_then(Value::as_array)
        .ok_or(FetchError::Invalid("the library data has no order keys."))?;
    if keys.len() > MAX_KEYS {
        return Err(FetchError::Invalid("it lists too many orders."));
    }
    let mut out: Vec<String> = keys
        .iter()
        .filter_map(Value::as_str)
        .filter(|k| GAMEKEY.is_match(k))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    let account = [
        "/userOptions/email",
        "/user/email",
        "/email",
        "/userOptions/username",
    ]
    .iter()
    .find_map(|path| value.pointer(path).and_then(Value::as_str))
    .and_then(super::http::display_name);
    Ok(Library { keys: out, account })
}

fn owned_game(machine_name: &str, human_name: &str) -> Option<OwnedGame> {
    if !MACHINE_NAME.is_match(machine_name) {
        return None;
    }
    let title = clean_name(human_name)?;
    Some(OwnedGame {
        store_id: machine_name.to_string(),
        title,
        launch: Some(format!("{OWNED_ONLY_PREFIX}humble:{machine_name}")),
        playtime_seconds: None,
        last_played_ms: None,
    })
}

/// The games of one orders response (an object keyed by order key).
pub fn parse_orders(body: &[u8], out: &mut HashMap<String, OwnedGame>) -> Result<(), FetchError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| FetchError::Invalid("it is not the expected JSON."))?;
    let orders = value
        .as_object()
        .ok_or(FetchError::Invalid("the orders are not the expected JSON."))?;
    for order in orders.values() {
        let subproducts = order.get("subproducts").and_then(Value::as_array);
        for sub in subproducts.into_iter().flatten() {
            let windows = sub
                .get("downloads")
                .and_then(Value::as_array)
                .is_some_and(|d| {
                    d.iter()
                        .any(|x| x.get("platform").and_then(Value::as_str) == Some("windows"))
                });
            let (Some(machine), Some(human)) = (
                sub.get("machine_name").and_then(Value::as_str),
                sub.get("human_name").and_then(Value::as_str),
            ) else {
                continue;
            };
            if windows && out.len() < MAX_GAMES {
                if let Some(game) = owned_game(machine, human) {
                    out.entry(game.store_id.clone()).or_insert(game);
                }
            }
        }
        let keys = order
            .pointer("/tpkd_dict/all_tpks")
            .and_then(Value::as_array);
        for key in keys.into_iter().flatten() {
            if key.get("key_type").and_then(Value::as_str) != Some("steam") {
                continue;
            }
            let (Some(machine), Some(human)) = (
                key.get("machine_name").and_then(Value::as_str),
                key.get("human_name").and_then(Value::as_str),
            ) else {
                continue;
            };
            if out.len() < MAX_GAMES {
                if let Some(game) = owned_game(machine, human) {
                    out.entry(game.store_id.clone()).or_insert(game);
                }
            }
        }
    }
    Ok(())
}

async fn library(http: &StoreHttp) -> Result<Library, FetchError> {
    let url = Url::parse("https://www.humblebundle.com/home/library").expect("valid URL");
    parse_library_page(&http.get(url).await?)
}

/// Checks that the session works; the account name when the page gives one.
pub async fn probe(http: &StoreHttp) -> Result<Option<String>, FetchError> {
    library(http).await.map(|l| l.account)
}

pub async fn fetch(http: &StoreHttp) -> Result<FetchOutcome, FetchError> {
    let keys = library(http).await?.keys;
    let orders = Url::parse("https://www.humblebundle.com/api/v1/orders").expect("valid URL");
    fetch_orders(http, &orders, &keys).await
}

/// The orders of `keys`, [`BATCH`] at a time from `orders`. The first failed batch ends the
/// fetch as incomplete (nothing is removed then), so a struggling store can't hold the scan for
/// up to [`MAX_PAGES`] slow requests; a rejected session ends it as an error.
async fn fetch_orders(
    http: &StoreHttp,
    orders: &Url,
    keys: &[String],
) -> Result<FetchOutcome, FetchError> {
    let mut games = HashMap::new();
    let mut complete = true;
    for (n, batch) in keys.chunks(BATCH).enumerate() {
        if n >= MAX_PAGES {
            complete = false;
            break;
        }
        let mut url = orders.clone();
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("all_tpkds", "true");
            for key in batch {
                q.append_pair("gamekeys", key);
            }
        }
        match http
            .get(url)
            .await
            .and_then(|body| parse_orders(&body, &mut games))
        {
            Ok(()) => {}
            Err(FetchError::Rejected) => return Err(FetchError::Rejected),
            Err(_) => {
                complete = false;
                break;
            }
        }
    }
    let mut games: Vec<OwnedGame> = games.into_values().collect();
    games.sort_by(|a, b| a.store_id.cmp(&b.store_id));
    let owned_ids: HashSet<String> = games.iter().map(|g| g.store_id.clone()).collect();
    Ok(FetchOutcome {
        games,
        owned_ids,
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(json: &str) -> Vec<u8> {
        format!(
            "<html><head><script>var x = 1;</script></head><body><script id=\"user-home-json-data\" type=\"application/json\">\n  {json}\n</script><script>other()</script></body></html>"
        )
        .into_bytes()
    }

    #[test]
    fn order_keys_come_only_from_the_data_block() {
        let library = parse_library_page(&page(r#"{"gamekeys":["abcDEF123","abcDEF123","bad key","../x",7],"other":{"gamekeys":["nested"]},"userOptions":{"email":"me@example.com"}}"#)).unwrap();
        assert_eq!(library.keys, ["abcDEF123"]);
        assert_eq!(library.account.as_deref(), Some("me@example.com"));
        let plain =
            parse_library_page(&page(r#"{"gamekeys":[],"userOptions":{"email":"<b>"}}"#)).unwrap();
        assert_eq!(plain.account, None);
        // Signed out: no block.
        assert_eq!(
            parse_library_page(b"<html>Log in</html>"),
            Err(FetchError::Rejected)
        );
        // Two blocks, a cut-off block, broken JSON or no keys: errors.
        let twice = [page("{}"), page("{}")].concat();
        assert!(matches!(
            parse_library_page(&twice),
            Err(FetchError::Invalid(_))
        ));
        assert!(matches!(
            parse_library_page(b"<script id=\"user-home-json-data\">{\"gamekeys\":[]}"),
            Err(FetchError::Invalid(_))
        ));
        assert!(matches!(
            parse_library_page(&page("{not json")),
            Err(FetchError::Invalid(_))
        ));
        assert!(matches!(
            parse_library_page(&page("{}")),
            Err(FetchError::Invalid(_))
        ));
        // A huge block is refused without parsing it.
        let huge = page(&format!(
            r#"{{"gamekeys":[],"pad":"{}"}}"#,
            "x".repeat(MAX_BLOCK + 10)
        ));
        assert!(matches!(
            parse_library_page(&huge),
            Err(FetchError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn the_first_failed_batch_ends_the_fetch() {
        use crate::accounts::http::tests::{ok, store_server};
        let good = ok(
            r#"{"k":{"subproducts":[{"machine_name":"a_game","human_name":"A","downloads":[{"platform":"windows"}]}]}}"#,
            "",
        );
        let (base, seen) = store_server(vec![]).await;
        let http = StoreHttp::local(Default::default());
        let orders = Url::parse(&format!("{base}/api/v1/orders")).unwrap();
        let keys: Vec<String> = (0..200).map(|i| format!("key{i}")).collect();
        // Every batch fails (404): one request, then incomplete.
        let out = fetch_orders(&http, &orders, &keys).await.unwrap();
        assert!(!out.complete);
        assert_eq!(seen.lock().unwrap().len(), 1);
        drop(good);
        // A rejected session is an error, not an incomplete list.
        let (base, _) = store_server(vec![(
            "/api/v1/orders?all_tpkds=true&gamekeys=key0",
            "HTTP/1.1 401 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        )])
        .await;
        let orders = Url::parse(&format!("{base}/api/v1/orders")).unwrap();
        assert_eq!(
            fetch_orders(&http, &orders, &["key0".to_string()]).await,
            Err(FetchError::Rejected)
        );
    }

    #[test]
    fn windows_downloads_and_steam_keys_are_kept() {
        let mut games = HashMap::new();
        parse_orders(
            br#"{
              "k1":{"subproducts":[
                {"machine_name":"cool_game","human_name":"Cool Game","downloads":[{"platform":"windows"}]},
                {"machine_name":"mac_only","human_name":"Mac Only","downloads":[{"platform":"mac"}]},
                {"machine_name":"ebook","human_name":"A Book","downloads":[{"platform":"ebook"}]},
                {"machine_name":"bad name!","human_name":"Bad","downloads":[{"platform":"windows"}]}
              ],"tpkd_dict":{"all_tpks":[
                {"machine_name":"steam_game_steam","human_name":"Steam Game","key_type":"steam"},
                {"machine_name":"origin_game","human_name":"Origin Game","key_type":"origin"}
              ]}},
              "k2":{"subproducts":"oops"},
              "k3":null
            }"#,
            &mut games,
        )
        .unwrap();
        let mut ids: Vec<_> = games.keys().cloned().collect();
        ids.sort();
        assert_eq!(ids, ["cool_game", "steam_game_steam"]);
        let link = games["cool_game"].launch.clone().unwrap();
        assert_eq!(link, "owned-only:humble:cool_game");
        assert!(crate::launch::is_owned_only(&link));
        assert!(parse_orders(b"[]", &mut games).is_err());
        assert!(parse_orders(b"<html>", &mut games).is_err());
        let p = |u: &str| signed_in_page(&Url::parse(u).unwrap());
        assert!(p("https://www.humblebundle.com/home/library"));
        assert!(!p(
            "https://www.humblebundle.com/login?goto=%2Fhome%2Flibrary"
        ));
    }
}
