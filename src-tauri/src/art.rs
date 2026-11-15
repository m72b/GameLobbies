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
//!
//! A hero or logo the user chose (table `game_page_custom`, see [`GamePageCustom`]) goes first
//! in its list; the automatic candidates stay behind it as fallbacks. Custom values are a
//! SteamGridDB https image URL or a `lobby-cover://<name>` file in the covers folder (copied
//! there by [`import_image_file`]). Replaced files are left on disk.

use crate::db::{self, CachedArt};
use crate::state::AppState;
use crate::steamgriddb::{self, steam_app_id, GridPrefs};
use crate::types::{now_ms, Game, GameArt, GamePageCustom};
use crate::validation::{self, LOBBY_COVER_PREFIX, PAGE_IMAGE_EXTENSIONS};
use anyhow::{anyhow, bail, Result};
use rusqlite::Connection;
use std::io::{Read, Write};
use std::path::Path;

/// Largest hero or logo file that can be chosen.
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Which custom image of the game page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Hero,
    Logo,
}

impl Slot {
    fn name(self) -> &'static str {
        match self {
            Self::Hero => "hero",
            Self::Logo => "logo",
        }
    }

    fn field(self, custom: &mut GamePageCustom) -> &mut Option<String> {
        match self {
            Self::Hero => &mut custom.hero,
            Self::Logo => &mut custom.logo,
        }
    }
}

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

/// Steam's double-size hero (3840x1240). Newer apps have it; older ones answer 404, and the
/// page then falls back to the next candidate, the 1920x620 `library_hero.jpg`.
fn steam_hero_2x(app_id: &str) -> String {
    format!("https://cdn.akamai.steamstatic.com/steam/apps/{app_id}/library_hero_2x.jpg")
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
        art.hero.push(steam_hero_2x(app_id));
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

/// Puts the chosen hero/logo first in its list (once), keeping the rest as fallbacks.
pub fn with_custom(mut art: GameArt, custom: GamePageCustom) -> GameArt {
    for (list, chosen) in [(&mut art.hero, &custom.hero), (&mut art.logo, &custom.logo)] {
        if let Some(url) = chosen {
            list.retain(|u| u != url);
            list.insert(0, url.clone());
        }
    }
    art.custom = custom;
    art
}

/// Validates a custom hero/logo value for storing: a SteamGridDB https image URL, or a
/// `lobby-cover://<name>` whose file exists (as a regular file) in `covers_dir`.
pub fn resolve_image_value(raw: &str, covers_dir: &Path) -> Result<String> {
    let value = validation::page_art_value(raw)?;
    if let Some(name) = validation::lobby_cover_name(&value) {
        let exists = std::fs::symlink_metadata(covers_dir.join(name)).is_ok_and(|m| m.is_file());
        if !exists {
            bail!("That image file no longer exists.");
        }
    }
    Ok(value)
}

fn update(
    conn: &Connection,
    game_id: &str,
    change: impl FnOnce(&mut GamePageCustom),
) -> Result<GamePageCustom> {
    let mut custom = db::page_custom(conn, game_id)?;
    change(&mut custom);
    if !db::put_page_custom(conn, game_id, &custom)? {
        bail!("Game not found.");
    }
    Ok(custom)
}

/// `art_set_hero` / `art_set_logo`: `None` goes back to the automatic art.
pub fn set_image(
    conn: &Connection,
    covers_dir: &Path,
    game_id: &str,
    slot: Slot,
    value: Option<&str>,
) -> Result<GamePageCustom> {
    let value = value
        .map(|v| resolve_image_value(v, covers_dir))
        .transpose()?;
    update(conn, game_id, |custom| *slot.field(custom) = value)
}

/// `art_set_logo_layout`.
pub fn set_logo_layout(
    conn: &Connection,
    game_id: &str,
    position: &str,
    scale: f64,
) -> Result<GamePageCustom> {
    let (position, scale) = validation::logo_layout(position, scale)?;
    update(conn, game_id, |custom| {
        custom.logo_position = position;
        custom.logo_scale = scale;
    })
}

/// The image format from the file's first bytes, as the extension to store it under.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"avif" | b"avis" | b"mif1" | b"msf1")
    {
        Some("avif")
    } else {
        None
    }
}

/// A new covers-folder name for a game's hero/logo: `hero-<id>-<uuid>.<ext>`, using only
/// characters [`validation::lobby_cover_name`] accepts.
pub fn image_file_name(game_id: &str, slot: Slot, ext: &str) -> String {
    let clean: String = game_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    format!("{}-{clean}-{}.{ext}", slot.name(), uuid::Uuid::new_v4())
}

/// Copies a user-chosen image into `covers_dir` under a new name and returns its
/// `lobby-cover://` value. Accepts PNG, JPEG, WebP, GIF and AVIF files (by extension and content)
/// up to [`MAX_IMAGE_BYTES`].
pub fn import_image_file(
    covers_dir: &Path,
    game_id: &str,
    slot: Slot,
    source: &Path,
) -> Result<String> {
    let name = import_image_as(
        covers_dir,
        source,
        &ImageRules {
            extensions: PAGE_IMAGE_EXTENSIONS,
            kinds: "PNG, JPEG, WebP, GIF or AVIF",
            max_bytes: MAX_IMAGE_BYTES,
        },
        |ext| image_file_name(game_id, slot, ext),
    )?;
    Ok(format!("{LOBBY_COVER_PREFIX}{name}"))
}

/// What [`import_image_as`] accepts.
pub struct ImageRules {
    /// Lower-case extensions (file name AND content); content types are named as
    /// [`crate::covercache::sniff`] names them (`jpg` for JPEG).
    pub extensions: &'static [&'static str],
    /// For the error message, like "PNG, JPEG, WebP or GIF".
    pub kinds: &'static str,
    pub max_bytes: u64,
}

/// Copies a user-chosen image into `covers_dir` under the new bare name `name(ext)` (`ext` is
/// the format found in the file's first bytes) and returns that name. The file name's
/// extension and its content must both be one of `rules.extensions`, and it must be a regular
/// file of at most `rules.max_bytes`. SVG and other text formats are never images here.
pub fn import_image_as(
    covers_dir: &Path,
    source: &Path,
    rules: &ImageRules,
    name: impl FnOnce(&str) -> String,
) -> Result<String> {
    let ext_ok = source
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| rules.extensions.contains(&e.to_ascii_lowercase().as_str()));
    if !ext_ok {
        bail!("Choose a {} image.", rules.kinds);
    }
    let max = rules.max_bytes;
    let too_large = || anyhow!("Choose an image smaller than {} MB.", max / (1024 * 1024));
    let unreadable = || anyhow!("Could not read that image.");
    let meta = std::fs::metadata(source).map_err(|_| unreadable())?;
    if !meta.is_file() {
        return Err(unreadable());
    }
    if meta.len() > max {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(source)
        .and_then(|f| f.take(max + 1).read_to_end(&mut bytes))
        .map_err(|_| unreadable())?;
    if bytes.len() as u64 > max {
        return Err(too_large());
    }
    let ext = crate::covercache::sniff(&bytes)
        .filter(|ext| rules.extensions.contains(ext))
        .ok_or_else(|| anyhow!("That file is not a supported image."))?;
    std::fs::create_dir_all(covers_dir)?;
    let name = name(ext);
    let path = covers_dir.join(&name);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut f| f.write_all(&bytes));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&path);
        return Err(anyhow!("Could not save the image: {e}"));
    }
    Ok(name)
}

/// Art for one game. Only fails on a local database error; an unknown game has no art.
pub async fn get(state: &AppState, game_id: &str) -> Result<GameArt> {
    // The database lock is released before any network request.
    let (game, cached, prefs, custom) = {
        let conn = state.db.lock();
        (
            db::get_game(&conn, game_id)?,
            db::cached_art(&conn, game_id)?,
            GridPrefs::from_settings(&db::get_settings(&conn)?),
            db::page_custom(&conn, game_id)?,
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
                    log::warn!("Could not cache game art: {e:#}");
                }
                Some((entry.hero, entry.logo))
            }
            Ok(None) => stale.map(|c| (c.hero, c.logo)),
            Err(e) => {
                log::warn!("SteamGridDB art lookup failed: {e:#}");
                stale.map(|c| (c.hero, c.logo))
            }
        },
    };
    Ok(with_custom(combine(&game, sgdb), custom))
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
                "https://cdn.akamai.steamstatic.com/steam/apps/400/library_hero_2x.jpg",
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

    use crate::db::Db;
    use crate::types::{GamePageCustom, LogoPosition};
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("game-lobby-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn library() -> Db {
        let db = Db::memory().unwrap();
        db::save_game(
            &db.lock(),
            &crate::types::GamePatch::from(&game(vec![route("steam", "400")])),
        )
        .unwrap();
        db
    }

    #[test]
    fn a_custom_choice_goes_first_and_the_rest_stay_as_fallbacks() {
        let g = game(vec![route("steam", "400")]);
        let sgdb = (
            vec![
                "https://cdn2.steamgriddb.com/hero/1.png".to_string(),
                "https://cdn2.steamgriddb.com/hero/2.png".to_string(),
            ],
            vec!["https://cdn2.steamgriddb.com/logo/1.png".to_string()],
        );
        let custom = GamePageCustom {
            hero: Some("https://cdn2.steamgriddb.com/hero/2.png".into()),
            logo: Some("lobby-cover://logo-g-1.png".into()),
            logo_position: LogoPosition::Center,
            logo_scale: 1.5,
        };
        let art = with_custom(combine(&g, Some(sgdb.clone())), custom.clone());
        assert_eq!(
            art.hero,
            [
                "https://cdn2.steamgriddb.com/hero/2.png",
                "https://cdn.akamai.steamstatic.com/steam/apps/400/library_hero_2x.jpg",
                "https://cdn.akamai.steamstatic.com/steam/apps/400/library_hero.jpg",
                "https://cdn2.steamgriddb.com/hero/1.png",
            ]
        );
        assert_eq!(
            art.logo,
            [
                "lobby-cover://logo-g-1.png",
                "https://cdn.akamai.steamstatic.com/steam/apps/400/logo.png",
                "https://cdn2.steamgriddb.com/logo/1.png",
            ]
        );
        assert_eq!(art.custom, custom);
        // No customization: the automatic order, defaults reported.
        let plain = with_custom(combine(&g, Some(sgdb)), GamePageCustom::default());
        assert_eq!(
            plain.hero[0],
            "https://cdn.akamai.steamstatic.com/steam/apps/400/library_hero_2x.jpg"
        );
        let json = serde_json::to_value(&plain).unwrap();
        assert_eq!(
            json["custom"],
            serde_json::json!({ "logoPosition": "bottom-left", "logoScale": 1.0 })
        );
        let json = serde_json::to_value(&art).unwrap();
        assert_eq!(json["custom"]["logoPosition"], "center");
        assert_eq!(
            json["custom"]["hero"],
            "https://cdn2.steamgriddb.com/hero/2.png"
        );
    }

    #[test]
    fn custom_images_must_be_steamgriddb_urls_or_existing_cover_files() {
        let dir = TempDir::new();
        std::fs::write(dir.0.join("hero-g-1.png"), PNG).unwrap();
        std::fs::create_dir(dir.0.join("folder.png")).unwrap();
        std::fs::write(dir.0.join("secret.txt"), b"x").unwrap();
        let ok = "https://cdn2.steamgriddb.com/hero/a.png";
        assert_eq!(resolve_image_value(ok, &dir.0).unwrap(), ok);
        assert_eq!(
            resolve_image_value(" lobby-cover://hero-g-1.png ", &dir.0).unwrap(),
            "lobby-cover://hero-g-1.png"
        );
        for bad in [
            "https://evil.example/hero.png",
            "http://cdn2.steamgriddb.com/hero/a.png",
            "https://cdn2.steamgriddb.com:444/hero/a.png",
            "https://cdn2.steamgriddb.com/hero/a.webm",
            "https://cdn.akamai.steamstatic.com/steam/apps/400/library_hero.jpg",
            "file:///C:/Windows/win.ini",
            "lobby-cover://missing.png",
            "lobby-cover://folder.png",
            "lobby-cover://secret.txt",
            "lobby-cover://../library.sqlite",
            "lobby-cover://..%2Flibrary.png",
            "lobby-cover://sub/hero-g-1.png",
            "lobby-cover://sub\\hero-g-1.png",
            "lobby-cover://C:hero-g-1.png",
            "lobby-cover://.hidden.png",
            "lobby-cover://a..png",
            "lobby-cover://CON.png",
            "lobby-cover://com1.png",
            "lobby-cover://",
            "",
        ] {
            assert!(
                resolve_image_value(bad, &dir.0).is_err(),
                "{bad} should be rejected"
            );
        }

        let db = library();
        let conn = db.lock();
        let custom = set_image(&conn, &dir.0, "g", Slot::Hero, Some(ok)).unwrap();
        assert_eq!(custom.hero.as_deref(), Some(ok));
        let custom = set_image(
            &conn,
            &dir.0,
            "g",
            Slot::Logo,
            Some("lobby-cover://hero-g-1.png"),
        )
        .unwrap();
        assert_eq!(custom.hero.as_deref(), Some(ok), "the other slot is kept");
        assert_eq!(db::page_custom(&conn, "g").unwrap(), custom);
        assert!(set_image(
            &conn,
            &dir.0,
            "g",
            Slot::Hero,
            Some("https://evil.example/x.png")
        )
        .is_err());
        assert_eq!(
            db::page_custom(&conn, "g").unwrap(),
            custom,
            "a rejected value changes nothing"
        );
        assert!(set_image(&conn, &dir.0, "ghost", Slot::Hero, Some(ok)).is_err());
        assert_eq!(
            db::page_custom(&conn, "ghost").unwrap(),
            GamePageCustom::default()
        );
        // Resetting both slots (default layout) removes the row.
        set_image(&conn, &dir.0, "g", Slot::Hero, None).unwrap();
        let custom = set_image(&conn, &dir.0, "g", Slot::Logo, None).unwrap();
        assert!(custom.is_default());
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM game_page_custom", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn logo_layout_is_validated() {
        let db = library();
        let conn = db.lock();
        let custom = set_logo_layout(&conn, "g", "top-center", 1.75).unwrap();
        assert_eq!(
            (custom.logo_position, custom.logo_scale),
            (LogoPosition::TopCenter, 1.75)
        );
        for position in ["bottom-left", "top-center", "center", "bottom-center"] {
            assert!(set_logo_layout(&conn, "g", position, 1.0).is_ok());
        }
        assert!(set_logo_layout(&conn, "g", "center", 0.5).is_ok());
        assert!(set_logo_layout(&conn, "g", "center", 2.0).is_ok());
        for (position, scale) in [
            ("left", 1.0),
            ("Center", 1.0),
            ("", 1.0),
            ("center", 0.49),
            ("center", 2.01),
            ("center", f64::NAN),
            ("center", f64::INFINITY),
            ("center", -1.0),
        ] {
            assert!(
                set_logo_layout(&conn, "g", position, scale).is_err(),
                "{position} {scale}"
            );
        }
        let stored = db::page_custom(&conn, "g").unwrap();
        assert_eq!(
            (stored.logo_position, stored.logo_scale),
            (LogoPosition::Center, 2.0)
        );
        assert!(set_logo_layout(&conn, "ghost", "center", 1.0).is_err());
    }

    #[test]
    fn chosen_files_are_checked_and_copied_under_a_safe_new_name() {
        let src = TempDir::new();
        let covers = TempDir::new();
        let covers_dir = covers.0.join("covers");
        let png = src.0.join("Logo Final.PNG");
        std::fs::write(&png, PNG).unwrap();
        let value = import_image_file(&covers_dir, "steam:400/../x", Slot::Logo, &png).unwrap();
        let name = validation::lobby_cover_name(&value).expect("a valid lobby-cover value");
        assert!(
            name.starts_with("logo-steam_400____x-") && name.ends_with(".png"),
            "{name}"
        );
        assert_eq!(std::fs::read(covers_dir.join(name)).unwrap(), PNG);
        assert_eq!(resolve_image_value(&value, &covers_dir).unwrap(), value);

        // The stored extension follows the content, not the file name.
        let jpeg_named_png = src.0.join("photo.png");
        std::fs::write(&jpeg_named_png, [0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]).unwrap();
        let value = import_image_file(&covers_dir, "g", Slot::Hero, &jpeg_named_png).unwrap();
        assert!(value.starts_with("lobby-cover://hero-g-") && value.ends_with(".jpg"));

        // GIFs are kept as animated hero art, named by content.
        let gif_src = src.0.join("loop.GIF");
        std::fs::write(&gif_src, b"GIF89a     ;").unwrap();
        let value = import_image_file(&covers_dir, "g", Slot::Hero, &gif_src).unwrap();
        assert!(value.ends_with(".gif"), "{value}");
        assert_eq!(resolve_image_value(&value, &covers_dir).unwrap(), value);

        let long_id = "x".repeat(500);
        let value = import_image_file(&covers_dir, &long_id, Slot::Hero, &png).unwrap();
        assert!(validation::lobby_cover_name(&value).is_some());

        let text = src.0.join("notes.txt");
        std::fs::write(&text, PNG).unwrap();
        let fake = src.0.join("fake.webp");
        std::fs::write(&fake, b"<html>not an image</html>").unwrap();
        let big = src.0.join("big.png");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        drop(file);
        for (path, message) in [
            (&text, "Choose a PNG"),
            (&fake, "not a supported image"),
            (&big, "smaller than 20 MB"),
            (&src.0.join("missing.png"), "Could not read"),
        ] {
            let e = import_image_file(&covers_dir, "g", Slot::Hero, path).unwrap_err();
            assert!(e.to_string().contains(message), "{e}");
        }
        let dir_named_png = src.0.join("dir.png");
        std::fs::create_dir(&dir_named_png).unwrap();
        assert!(import_image_file(&covers_dir, "g", Slot::Hero, &dir_named_png).is_err());
        assert_eq!(std::fs::read_dir(&covers_dir).unwrap().count(), 4);
    }

    /// `import_image_as` keeps only sniffed types in its list: every type the sniffer can
    /// return must be one page art accepts, or a real image would be refused as "not supported".
    #[test]
    fn every_sniffed_type_is_an_accepted_page_image() {
        let samples: [&[u8]; 5] = [
            PNG,
            &[0xFF, 0xD8, 0xFF, 0xE0],
            b"RIFF\x10\0\0\0WEBPVP8 ",
            b"\0\0\0\x1cftypavif\0\0\0\0",
            b"GIF89a  ",
        ];
        let mut seen = Vec::new();
        for bytes in samples {
            let ext = crate::covercache::sniff(bytes).expect("an image");
            assert!(PAGE_IMAGE_EXTENSIONS.contains(&ext), "{ext}");
            seen.push(ext);
        }
        assert_eq!(seen, ["png", "jpg", "webp", "avif", "gif"]);
        // SVG and other text formats are never sniffed as images.
        assert_eq!(crate::covercache::sniff(b"<svg xmlns=\"x\"/>"), None);
        assert_eq!(
            crate::covercache::sniff(b"<?xml version=\"1.0\"?><svg/>"),
            None
        );
    }

    #[test]
    fn image_formats_are_recognised_by_content() {
        assert_eq!(sniff_image(PNG), Some("png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xDB]), Some("jpg"));
        assert_eq!(sniff_image(b"RIFF\x10\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff_image(b"\0\0\0\x1cftypavif\0\0\0\0"), Some("avif"));
        assert_eq!(sniff_image(b"\0\0\0\x1cftypmp42\0\0\0\0"), None);
        assert_eq!(sniff_image(b"GIF89a"), None);
        assert_eq!(crate::covercache::sniff(b"GIF89a  "), Some("gif"));
        assert_eq!(sniff_image(b""), None);
    }
}
