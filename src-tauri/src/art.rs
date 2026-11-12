//! Game page art (`art_get`): hero banners and logos as candidate URLs in priority order.
//!
//! 1. Steam's own library assets when the game has a Steam route with a numeric app ID.
//! 2. SteamGridDB heroes and logos (when a key is saved), filtered by the user's
//!    animated/NSFW/humor preferences. These are cached per game in `game_art` for
//!    [`ART_TTL_MS`]; changing those preferences invalidates the entry.
//!
//! Missing art is never an error: without a key, on a 404, or when SteamGridDB can't be
//! reached, whatever is available (Steam candidates, a stale cache entry) is returned. The UI
//! tries each URL in turn and moves on when one fails to load.

use crate::db::{self, CachedArt};
use crate::state::AppState;
use crate::steamgriddb::{self, steam_app_id, GridPrefs};
use crate::types::{now_ms, Game, GameArt};
use anyhow::Result;

/// How long SteamGridDB results stay cached.
pub const ART_TTL_MS: i64 = 30 * 24 * 3_600_000;
/// Tolerated clock skew before a cache entry dated in the future is treated as stale.
const FUTURE_SKEW_MS: i64 = 24 * 3_600_000;

/// Steam's library hero and logo for an app ID.
pub fn steam_art(app_id: &str) -> (String, String) {
    let base = format!("https://cdn.akamai.steamstatic.com/steam/apps/{app_id}");
    (
        format!("{base}/library_hero.jpg"),
        format!("{base}/logo.png"),
    )
}

/// Whether a cache entry can be used without refetching.
pub fn is_fresh(cached: &CachedArt, prefs_key: &str, now: i64) -> bool {
    cached.prefs == prefs_key
        && cached.fetched_at <= now + FUTURE_SKEW_MS
        && now - cached.fetched_at < ART_TTL_MS
}

/// Steam candidates first, then SteamGridDB's; duplicates removed, order kept.
pub fn combine(game: &Game, sgdb: Option<(Vec<String>, Vec<String>)>) -> GameArt {
    let mut art = GameArt::default();
    if let Some(app_id) = steam_app_id(game) {
        let (hero, logo) = steam_art(app_id);
        art.hero.push(hero);
        art.logo.push(logo);
    }
    if let Some((hero, logo)) = sgdb {
        for (list, extra) in [(&mut art.hero, hero), (&mut art.logo, logo)] {
            for url in extra {
                if !list.contains(&url) {
                    list.push(url);
                }
            }
        }
    }
    art
}

/// Art for one game. Only fails on a local database error; an unknown game has no art.
pub async fn get(state: &AppState, game_id: &str) -> Result<GameArt> {
    // The database lock is released before any network request.
    let (game, cached, prefs) = {
        let conn = state.db.lock();
        (
            db::get_game(&conn, game_id)?,
            db::cached_art(&conn, game_id)?,
            GridPrefs::from_settings(&db::get_settings(&conn)?),
        )
    };
    let Some(game) = game else {
        return Ok(GameArt::default());
    };
    let key = prefs.art_cache_key();
    let now = now_ms();
    // Entries fetched with other preferences are never shown.
    let cached = cached.filter(|c| c.prefs == key);
    let sgdb = match cached {
        Some(c) if is_fresh(&c, &key, now) => Some((c.hero, c.logo)),
        stale => match steamgriddb::fetch_art(state, &game, &prefs).await {
            Ok(Some((hero, logo))) => {
                let entry = CachedArt {
                    hero,
                    logo,
                    prefs: key,
                    fetched_at: now,
                };
                if let Err(e) = db::put_cached_art(&state.db.lock(), game_id, &entry) {
                    eprintln!("Could not cache game art: {e:#}");
                }
                Some((entry.hero, entry.logo))
            }
            Ok(None) => stale.map(|c| (c.hero, c.logo)),
            Err(e) => {
                eprintln!("SteamGridDB art lookup failed: {e:#}");
                stale.map(|c| (c.hero, c.logo))
            }
        },
    };
    Ok(combine(&game, sgdb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GameRoute;

    fn game(routes: Vec<GameRoute>) -> Game {
        Game {
            id: "g".into(),
            title: "Portal".into(),
            store: "Steam".into(),
            routes,
            ..Default::default()
        }
    }

    fn route(source: &str, app_id: &str) -> GameRoute {
        GameRoute {
            source_id: source.into(),
            store: "Steam".into(),
            app_id: Some(app_id.into()),
            launch_target: "x".into(),
            ..Default::default()
        }
    }

    #[test]
    fn steam_candidates_come_first_then_steamgriddb_without_duplicates() {
        let sgdb = (
            vec![
                "https://cdn2.steamgriddb.com/hero/1.png".to_string(),
                "https://cdn2.steamgriddb.com/hero/1.png".to_string(),
                "https://cdn2.steamgriddb.com/hero/2.png".to_string(),
            ],
            vec!["https://cdn2.steamgriddb.com/logo/1.png".to_string()],
        );
        let art = combine(
            &game(vec![route("epic", "abc"), route("steam", "400")]),
            Some(sgdb.clone()),
        );
        assert_eq!(
            art.hero,
            [
                "https://cdn.akamai.steamstatic.com/steam/apps/400/library_hero.jpg",
                "https://cdn2.steamgriddb.com/hero/1.png",
                "https://cdn2.steamgriddb.com/hero/2.png"
            ]
        );
        assert_eq!(
            art.logo,
            [
                "https://cdn.akamai.steamstatic.com/steam/apps/400/logo.png",
                "https://cdn2.steamgriddb.com/logo/1.png"
            ]
        );
        // No Steam route (or a malformed app ID): SteamGridDB only; nothing at all is fine.
        let art = combine(&game(vec![route("steam", "4/../x")]), Some(sgdb));
        assert_eq!(art.hero.len(), 2);
        assert_eq!(art.logo, ["https://cdn2.steamgriddb.com/logo/1.png"]);
        assert_eq!(combine(&game(vec![]), None), GameArt::default());
    }

    #[test]
    fn cache_entries_expire_and_follow_the_preferences() {
        let now = 1_000 * ART_TTL_MS;
        let entry = |fetched_at: i64| CachedArt {
            prefs: "k".into(),
            fetched_at,
            ..Default::default()
        };
        assert!(is_fresh(&entry(now), "k", now));
        assert!(is_fresh(&entry(now - ART_TTL_MS + 1), "k", now));
        assert!(!is_fresh(&entry(now - ART_TTL_MS), "k", now));
        assert!(!is_fresh(&entry(now), "other", now));
        assert!(is_fresh(&entry(now + 1000), "k", now));
        assert!(!is_fresh(&entry(now + 2 * FUTURE_SKEW_MS), "k", now));
    }
}
