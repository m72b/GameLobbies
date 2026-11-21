//! Battle.net account (cookie session from the sign-in window): the games of the account,
//! from the endpoints Blizzard's own account pages use (unofficial; they may change):
//!
//! - `https://account.battle.net/api/games-and-subs`: `gameAccounts` (the probe, too);
//! - `https://account.battle.net/api/classic-games`: `classicGames`.
//!
//! Battle.net gives no playtime. A game is matched by its English name to a fixed table of
//! Battle.net app codes (`battlenet://<code>`); an unknown game is listed as owned with no way
//! to start it, never with a guessed link.

use super::apply::{FetchOutcome, OwnedGame};
use super::http::{clean_name, FetchError, StoreHttp, MAX_GAMES};
use crate::launch::OWNED_ONLY_PREFIX;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use url::Url;

pub const API_HOSTS: &[&str] = &["account.battle.net"];
pub const COOKIE_DOMAINS: &[&str] = &["battle.net"];
pub const START_URL: &str = "https://account.battle.net/overview";

/// Battle.net app codes by game name ([`crate::catalog::title_key`] of the English name). Only
/// codes the Battle.net app is known to accept in `battlenet://<code>`.
const CODES: &[(&str, &str)] = &[
    ("World of Warcraft", "WoW"),
    ("Diablo III", "D3"),
    ("Diablo III: Reaper of Souls", "D3"),
    ("StarCraft II", "S2"),
    ("Hearthstone", "WTCG"),
    ("Heroes of the Storm", "Hero"),
    ("Overwatch", "Pro"),
    ("Overwatch 2", "Pro"),
    ("Warcraft III: Reforged", "W3"),
    ("StarCraft: Remastered", "S1"),
    ("Diablo II: Resurrected", "OSI"),
    ("Diablo IV", "Fen"),
    ("Call of Duty: Black Ops 4", "VIPR"),
    ("Call of Duty: Modern Warfare", "ODIN"),
    ("Call of Duty: Black Ops Cold War", "ZEUS"),
    ("Call of Duty: Vanguard", "FORE"),
    ("Crash Bandicoot 4: It's About Time", "WLBY"),
    ("Blizzard Arcade Collection", "RTRO"),
];

/// Whether the sign-in window reached the signed-in account overview.
pub fn signed_in_page(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("account.battle.net")
        && url.path().trim_end_matches('/') == "/overview"
}

fn code_for(name: &str) -> Option<&'static str> {
    let key = crate::catalog::title_key(name);
    CODES
        .iter()
        .find(|(known, _)| crate::catalog::title_key(known) == key)
        .map(|(_, code)| *code)
}

/// The store id of a game: its app code, or `title-<slug>` for a game without a known code.
fn store_id(name: &str) -> Option<(String, Option<String>)> {
    if let Some(code) = code_for(name) {
        return Some((code.to_string(), Some(format!("battlenet://{code}"))));
    }
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug: String = slug.chars().take(64).collect();
    (!slug.is_empty()).then(|| (format!("title-{slug}"), None))
}

/// The games named in `list` (objects with `localizedGameName`).
fn games_of(list: &[Value], out: &mut HashMap<String, OwnedGame>) {
    for entry in list.iter().take(MAX_GAMES) {
        let Some(title) = entry
            .get("localizedGameName")
            .and_then(Value::as_str)
            .and_then(clean_name)
        else {
            continue;
        };
        let Some((id, link)) = store_id(&title) else {
            continue;
        };
        let launch = link.or_else(|| Some(format!("{OWNED_ONLY_PREFIX}battlenet:{id}")));
        out.entry(id.clone()).or_insert(OwnedGame {
            store_id: id,
            title,
            launch,
            playtime_seconds: None,
            last_played_ms: None,
        });
    }
}

/// `games-and-subs`: the game accounts (an error when the list is missing: not signed in).
pub fn parse_games_and_subs(body: &[u8]) -> Result<Vec<Value>, FetchError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| FetchError::Invalid("it is not the expected JSON."))?;
    value
        .get("gameAccounts")
        .and_then(Value::as_array)
        .cloned()
        .ok_or(FetchError::Rejected)
}

/// The BattleTag `games-and-subs` reports, if it does (`battleTag` / `battletag`).
pub fn parse_battletag(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    ["battleTag", "battletag", "BattleTag"]
        .iter()
        .find_map(|k| value.get(*k).and_then(Value::as_str))
        .and_then(super::http::display_name)
}

/// `classic-games`: the classic games (missing list: none).
pub fn parse_classic(body: &[u8]) -> Result<Vec<Value>, FetchError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| FetchError::Invalid("it is not the expected JSON."))?;
    Ok(value
        .get("classicGames")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub fn outcome(accounts: &[Value], classic: Option<&[Value]>) -> FetchOutcome {
    let mut games = HashMap::new();
    games_of(accounts, &mut games);
    if let Some(classic) = classic {
        games_of(classic, &mut games);
    }
    let mut games: Vec<OwnedGame> = games.into_values().collect();
    games.sort_by(|a, b| a.store_id.cmp(&b.store_id));
    let owned_ids: HashSet<String> = games.iter().map(|g| g.store_id.clone()).collect();
    FetchOutcome {
        games,
        owned_ids,
        complete: classic.is_some(),
        playtime_complete: false,
    }
}

/// Checks that the session works; the BattleTag when the store gives it.
pub async fn probe(http: &StoreHttp) -> Result<Option<String>, FetchError> {
    let url = Url::parse("https://account.battle.net/api/games-and-subs").expect("valid URL");
    let body = http.get(url).await?;
    parse_games_and_subs(&body)?;
    Ok(parse_battletag(&body))
}

pub async fn fetch(http: &StoreHttp) -> Result<FetchOutcome, FetchError> {
    let accounts = parse_games_and_subs(
        &http
            .get(Url::parse("https://account.battle.net/api/games-and-subs").expect("valid URL"))
            .await?,
    )?;
    let classic = http
        .get(Url::parse("https://account.battle.net/api/classic-games").expect("valid URL"))
        .await
        .and_then(|body| parse_classic(&body))
        .ok();
    Ok(outcome(&accounts, classic.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_get_a_known_code_or_no_link() {
        let accounts = parse_games_and_subs(
            br#"{"gameAccounts":[
                {"titleId":5730135,"localizedGameName":"World of Warcraft","gameAccountStatus":"Good"},
                {"localizedGameName":"DIABLO III"},
                {"localizedGameName":"Overwatch 2"},
                {"localizedGameName":"Some New Game!"},
                {"localizedGameName":"   "},
                {"localizedGameName":42},
                {"titleId":1}
            ]}"#,
        )
        .unwrap();
        let classic =
            parse_classic(br#"{"classicGames":[{"localizedGameName":"Warcraft III: Reforged"}]}"#)
                .unwrap();
        let out = outcome(&accounts, Some(&classic));
        assert!(out.complete);
        let by_id: HashMap<&str, &OwnedGame> =
            out.games.iter().map(|g| (g.store_id.as_str(), g)).collect();
        assert_eq!(by_id["WoW"].launch.as_deref(), Some("battlenet://WoW"));
        assert_eq!(by_id["Pro"].title, "Overwatch 2");
        assert!(by_id.contains_key("W3"));
        let new = by_id["title-some-new-game"];
        assert_eq!(
            new.launch.as_deref(),
            Some("owned-only:battlenet:title-some-new-game")
        );
        assert_eq!(out.games.len(), 5, "{:?}", by_id.keys());
        for game in &out.games {
            let link = game.launch.as_deref().unwrap();
            if link.starts_with("battlenet://") {
                assert_eq!(crate::launch::validate_launch_uri(link).unwrap(), link);
                assert!(crate::validation::renderer_link_ok(link), "{link}");
            } else {
                assert!(crate::launch::is_owned_only(link));
                assert!(crate::launch::validate_launch_uri(link).is_err());
            }
        }
        // Without the classic list the result is not complete (nothing is removed).
        assert!(!outcome(&accounts, None).complete);
    }

    #[test]
    fn the_battletag_is_shown_when_present_and_plain() {
        assert_eq!(
            parse_battletag(br#"{"battleTag":"Hero#1234","gameAccounts":[]}"#),
            Some("Hero#1234".into())
        );
        assert_eq!(parse_battletag(br#"{"gameAccounts":[]}"#), None);
        assert_eq!(parse_battletag(br#"{"battleTag":"<script>"}"#), None);
        assert_eq!(parse_battletag(b"nope"), None);
    }

    #[test]
    fn hostile_or_signed_out_answers_are_errors() {
        assert_eq!(
            parse_games_and_subs(br#"{"error":"unauthorized"}"#),
            Err(FetchError::Rejected)
        );
        assert!(matches!(
            parse_games_and_subs(b"<html>login</html>"),
            Err(FetchError::Invalid(_))
        ));
        assert!(parse_classic(b"{}").unwrap().is_empty());
        let page = |u: &str| signed_in_page(&Url::parse(u).unwrap());
        assert!(page("https://account.battle.net/overview"));
        assert!(!page("https://account.battle.net/login/en/"));
        assert!(!page("https://eu.battle.net/overview"));
        for (_, code) in CODES {
            let link = format!("battlenet://{code}");
            assert!(crate::validation::renderer_link_ok(&link), "{link}");
        }
    }
}
