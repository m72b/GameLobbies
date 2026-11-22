//! Pipeline and cache tests. Network calls go to a local server answering with trimmed copies
//! of real API responses (`testdata/`); no test reaches Wikimedia.

use super::http::tests::{json_reply, serve, Req};
use super::*;
use crate::types::GameRoute;
use std::sync::Arc;

const ENTITIES: &str = include_str!("testdata/entities.json");
const LABELS: &str = include_str!("testdata/labels.json");
const SEARCH: &str = include_str!("testdata/search_doom.json");
const SPARQL: &str = include_str!("testdata/sparql.json");
const EXTRACTS: &str = include_str!("testdata/extracts.json");
const COMMONS: &str = include_str!("testdata/commons.json");

fn fixtures(req: &Req) -> http::tests::Reply {
    let action = req.param("action").unwrap_or_default();
    match req.path() {
        "/sparql" => (
            200,
            vec![(
                "Content-Type".into(),
                "application/sparql-results+json;charset=utf-8".into(),
            )],
            SPARQL.as_bytes().to_vec(),
        ),
        "/wikidata" if action == "wbsearchentities" => json_reply(SEARCH),
        "/wikidata" if req.param("props").as_deref() == Some("labels") => json_reply(LABELS),
        "/wikidata" => json_reply(ENTITIES),
        "/commons" => json_reply(COMMONS),
        "/wikipedia" => json_reply(EXTRACTS),
        _ => (404, vec![], vec![]),
    }
}

fn input(id: &str, title: &str, steam: Option<&str>) -> GameInput {
    GameInput {
        game_id: id.into(),
        title: title.into(),
        steam: steam.map(str::to_string),
        platform: Some("Q1406"),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_batch_matches_by_ids_and_title_with_shared_requests() {
    let (base, log) = serve(Arc::new(fixtures)).await;
    let limiter = Limiter::default();
    let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
    let inputs = [
        input("hl2", "Half-Life 2", Some("220")),
        input("fo3", "Fallout 3 GOTY", Some("22370")),
        input("doom", "DOOM", None),
        input("papers", "Papers, Please", Some("239030")),
    ];
    let out = resolve_batch(&api, &inputs).await.unwrap();
    let by = |id: &str| out.iter().find(|o| o.game_id == id).unwrap();

    let hl2 = by("hl2");
    assert_eq!(hl2.decision, Decision::Match("Q193581".into()));
    assert_eq!(hl2.matched_by, "steam");
    let m = hl2.metadata.as_ref().unwrap();
    assert_eq!(m.label, "Half-Life 2");
    assert_eq!(m.developers, vec!["Valve Corporation"]);
    assert_eq!(m.platforms, vec!["Microsoft Windows"]);
    assert_eq!(m.genres, vec!["first-person shooter"]);
    assert_eq!(m.release.as_deref(), Some("2004-11-16"));
    assert_eq!(m.logos.len(), 1);
    assert!(m.logos[0]
        .src
        .ends_with("500px-Half-Life_2_text_logo.svg.png"));
    assert_eq!(m.logos[0].licence, "Public domain");
    let article = m.article.as_ref().unwrap();
    assert!(article.text.starts_with("Half-Life 2 is a 2004"));
    assert!(!article.text.contains("Second paragraph"));
    assert_eq!(article.url, "https://en.wikipedia.org/wiki/Half-Life_2");

    // Steam 22370 belongs to the GOTY edition and the game: the game, with its free logo and a
    // small free cover (offered, low trust).
    let fo3 = by("fo3");
    assert_eq!(fo3.decision, Decision::Match("Q221164".into()));
    let m = fo3.metadata.as_ref().unwrap();
    assert_eq!(m.label, "Fallout 3");
    assert_eq!(m.covers.len(), 1);
    assert_eq!(m.covers[0].file, "Fallout 3 cover (cropped).png");
    assert_eq!(m.covers[0].licence, "CC BY 3.0");
    assert!(m.covers[0].attribution_required);
    assert_eq!(m.covers[0].artist.as_deref(), Some("Xbox México"));
    assert!(low_trust_cover(&m.covers[0]));

    // Two games are called Doom: the user chooses.
    let doom = by("doom");
    assert!(matches!(&doom.decision, Decision::Ambiguous(list) if list.len() == 2));
    assert!(doom.metadata.is_none());
    let years: Vec<_> = doom.candidates.iter().map(|c| c.year).collect();
    assert_eq!(years, vec![Some(1993), Some(2016)]);
    assert_eq!(
        doom.candidates[0].description.as_deref(),
        Some("1993 video game")
    );

    // The Wikipedia-only (non-free) cover file is not used; the free logo is.
    let papers = by("papers");
    let m = papers.metadata.as_ref().unwrap();
    assert!(m.covers.is_empty());
    assert_eq!(m.logos.len(), 1);

    let log = log.lock().unwrap();
    assert_eq!(
        log.iter().filter(|r| r.path() == "/sparql").count(),
        1,
        "one SPARQL query"
    );
    assert_eq!(log.iter().filter(|r| r.path() == "/commons").count(), 1);
    assert_eq!(log.iter().filter(|r| r.path() == "/wikipedia").count(), 1);
    for req in log.iter().filter(|r| r.path() != "/sparql") {
        assert_eq!(req.param("maxlag").as_deref(), Some("5"), "{}", req.target);
    }
    let sparql = log.iter().find(|r| r.path() == "/sparql").unwrap();
    let query = sparql.param("query").unwrap();
    assert!(
        query.contains("\"220\"") && query.contains("\"22370\"") && query.contains("\"239030\"")
    );
    let search = log
        .iter()
        .find(|r| r.param("action").as_deref() == Some("wbsearchentities"))
        .unwrap();
    assert_eq!(search.param("search").as_deref(), Some("DOOM"));
}

#[tokio::test]
async fn a_known_year_settles_a_tie_and_known_items_are_fetched_directly() {
    let (base, log) = serve(Arc::new(fixtures)).await;
    let limiter = Limiter::default();
    let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
    let doom = GameInput {
        year: Some(1993),
        ..input("doom", "Doom", None)
    };
    let out = resolve_batch(&api, &[doom]).await.unwrap();
    assert_eq!(out[0].decision, Decision::Match("Q189784".into()));
    let m = out[0].metadata.as_ref().unwrap();
    // A free box art, portrait and large enough to offer, but small: never set automatically.
    assert_eq!(m.covers.len(), 1);
    assert!(low_trust_cover(&m.covers[0]));
    assert_eq!(m.article.as_ref().unwrap().title, "Doom (1993 video game)");

    // A known item (the user's choice): no SPARQL and no search.
    log.lock().unwrap().clear();
    let known = GameInput {
        known: Some("Q20711315".into()),
        ..input("doom", "Doom", Some("2280"))
    };
    let out = resolve_batch(&api, &[known]).await.unwrap();
    assert_eq!(out[0].decision, Decision::Match("Q20711315".into()));
    assert_eq!(out[0].matched_by, "known");
    {
        let log = log.lock().unwrap();
        assert!(log.iter().all(|r| r.path() != "/sparql"));
        assert!(log
            .iter()
            .all(|r| r.param("action").as_deref() != Some("wbsearchentities")));
    }
    // A known item that is not a game (a film) is no match.
    let film = GameInput {
        known: Some("Q1241843".into()),
        ..input("x", "Doom", None)
    };
    assert_eq!(
        resolve_batch(&api, &[film]).await.unwrap()[0].decision,
        Decision::None
    );
}

#[tokio::test]
async fn a_failing_service_fails_the_batch_and_nothing_is_guessed() {
    let (base, _) = serve(Arc::new(|req: &Req| match req.path() {
        "/commons" => (500, vec![], vec![]),
        _ => fixtures(req),
    }))
    .await;
    let limiter = Limiter::default();
    let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
    let err = resolve_batch(&api, &[input("hl2", "Half-Life 2", Some("220"))])
        .await
        .unwrap_err();
    assert_eq!(err, WikiError::Http(500));
    assert!(err.is_outage());
}

// -- cache ----------------------------------------------------------------

fn library(ids: &[&str]) -> db::Db {
    let db = db::Db::memory().unwrap();
    for id in ids {
        db::save_game(
            &db.lock(),
            &GamePatch {
                title: Some(format!("Game {id}")),
                store: Some("Steam".into()),
                ..GamePatch::new(id.to_string())
            },
        )
        .unwrap();
    }
    db
}

fn outcome(id: &str, decision: Decision, by: &str, label: &str) -> Outcome {
    let metadata = match &decision {
        Decision::Match(q) => Some(WikiMetadata {
            qid: q.clone(),
            label: label.into(),
            shape: SHAPE,
            logos: vec![logo("Logo.png")],
            ..Default::default()
        }),
        _ => None,
    };
    Outcome {
        game_id: id.into(),
        decision,
        matched_by: by.into(),
        metadata,
        candidates: Vec::new(),
    }
}

fn logo(file: &str) -> CommonsImage {
    CommonsImage {
        file: file.into(),
        src: format!("https://upload.wikimedia.org/wikipedia/commons/a/ab/{file}"),
        width: 500,
        height: 100,
        original_width: 500,
        original_height: 100,
        licence: "Public domain".into(),
        source_page: format!("https://commons.wikimedia.org/wiki/File:{file}"),
        ..Default::default()
    }
}

#[test]
fn freshness_follows_the_result_and_the_shape() {
    let now = 100 * 24 * 3_600_000;
    let row = |status: &str, json: Option<&str>, age: i64| CachedMatch {
        qid: None,
        status: status.into(),
        matched_by: "steam".into(),
        source: "auto".into(),
        json: json.map(str::to_string),
        candidates: None,
        fetched_at: now - age,
    };
    let day = 24 * 3_600_000;
    let current = r#"{"qid":"Q1","label":"x","shape":1}"#;
    assert!(is_fresh(&row("matched", Some(current), 29 * day), now));
    assert!(!is_fresh(&row("matched", Some(current), 31 * day), now));
    // An older shape is refetched.
    assert!(!is_fresh(
        &row("matched", Some(r#"{"qid":"Q1","label":"x"}"#), day),
        now
    ));
    // No match and ambiguous results are retried after a week.
    assert!(is_fresh(&row("none", None, 6 * day), now));
    assert!(!is_fresh(&row("none", None, 8 * day), now));
    assert!(!is_fresh(&row("ambiguous", None, 8 * day), now));
    // A date far in the future is not trusted.
    assert!(!is_fresh(&row("none", None, -3 * day), now));
}

#[test]
fn user_choices_survive_automatic_results() {
    let db = library(&["a"]);
    let conn = db.lock();
    // Automatic match, then the user picks another item.
    assert!(store(
        &conn,
        &outcome("a", Decision::Match("Q1".into()), "steam", "One"),
        1
    )
    .unwrap());
    let mut row = row_for(
        &outcome("a", Decision::Match("Q2".into()), "known", "Two"),
        None,
        2,
    )
    .unwrap();
    row.source = "user".into();
    row.matched_by = "user".into();
    assert!(put(&conn, "a", &row).unwrap());
    // An automatic result for another item never replaces the choice...
    let auto = row_for(
        &outcome("a", Decision::Match("Q3".into()), "title", "Three"),
        None,
        3,
    )
    .unwrap();
    assert!(!put(&conn, "a", &auto).unwrap());
    assert!(!put(
        &conn,
        "a",
        &row_for(&outcome("a", Decision::None, "title", ""), None, 3).unwrap()
    )
    .unwrap());
    let kept = cached(&conn, "a").unwrap().unwrap();
    assert_eq!(
        (kept.qid.as_deref(), kept.source.as_str()),
        (Some("Q2"), "user")
    );
    // ...a refresh of the chosen item updates it and stays the user's...
    assert!(store(
        &conn,
        &outcome("a", Decision::Match("Q2".into()), "known", "Two, renamed"),
        4
    )
    .unwrap());
    let kept = cached(&conn, "a").unwrap().unwrap();
    assert_eq!(kept.source, "user");
    assert_eq!(kept.matched_by, "user");
    assert_eq!(metadata_of(&kept).unwrap().label, "Two, renamed");
    // ...and a chosen item that is gone keeps the choice as it was.
    assert!(!store(&conn, &outcome("a", Decision::None, "known", ""), 5).unwrap());
    assert_eq!(cached(&conn, "a").unwrap().unwrap().fetched_at, 4);
    // Games that don't exist get no row; deleting a game removes its row.
    assert!(!store(&conn, &outcome("ghost", Decision::None, "title", ""), 1).unwrap());
    db::remove_game(&conn, "a").unwrap();
    assert!(cached(&conn, "a").unwrap().is_none());
}

#[test]
fn no_match_and_ambiguous_results_are_cached_with_candidates() {
    let db = library(&["d"]);
    let conn = db.lock();
    let mut o = outcome(
        "d",
        Decision::Ambiguous(vec!["Q1".into(), "Q2".into()]),
        "title",
        "",
    );
    o.candidates = vec![
        Candidate {
            qid: "Q1".into(),
            label: "Doom".into(),
            description: Some("1993 video game".into()),
            year: Some(1993),
        },
        Candidate {
            qid: "bad".into(),
            label: "x".into(),
            description: None,
            year: None,
        },
    ];
    store(&conn, &o, now_ms()).unwrap();
    let row = cached(&conn, "d").unwrap().unwrap();
    assert_eq!(row.status, "ambiguous");
    assert!(is_fresh(&row, now_ms()));
    let v = view(Some(row), true, false, None, &conn, &|u| u.to_string());
    assert!(v.checked && v.metadata.is_none());
    assert_eq!(v.status.as_deref(), Some("ambiguous"));
    // Odd stored values are dropped on the way out.
    assert_eq!(v.candidates.len(), 1);
    assert_eq!(v.candidates[0].label, "Doom");
}

#[test]
fn views_resanitize_and_hide_everything_when_turned_off() {
    let db = library(&["a"]);
    let conn = db.lock();
    let mut o = outcome(
        "a",
        Decision::Match("Q1".into()),
        "steam",
        "Half\u{202e}-Life",
    );
    if let Some(m) = o.metadata.as_mut() {
        let mut bad = logo("Bad.png");
        bad.src = "https://upload.wikimedia.org/wikipedia/en/a/ab/Bad.png".into();
        m.logos.push(bad);
        m.release = Some("2004-11-16<script>".into());
    }
    store(&conn, &o, now_ms()).unwrap();
    let row = cached(&conn, "a").unwrap();
    let v = view(row.clone(), true, false, None, &conn, &|u| {
        format!("local:{u}")
    });
    let m = v.metadata.as_ref().unwrap();
    assert_eq!(m.label, "Half-Life");
    assert_eq!(m.release, None);
    assert_eq!(
        v.logos.len(),
        1,
        "a file outside the Commons folder is dropped"
    );
    assert!(v.logos[0]
        .display
        .starts_with("local:https://upload.wikimedia.org/"));
    assert_eq!(
        v.item_url.as_deref(),
        Some("https://www.wikidata.org/wiki/Q1")
    );
    // Off: nothing but the credit of a cover already in use.
    let mut cover = logo("Cover.png");
    cover.original_height = 900;
    put_images(&conn, &[&cover], 1).unwrap();
    let off = view(row, false, false, Some(&cover.src), &conn, &|u| {
        u.to_string()
    });
    assert!(off.metadata.is_none() && off.logos.is_empty() && !off.enabled);
    assert_eq!(off.cover_credit.as_ref().unwrap().image.file, "Cover.png");
    // The setting is on by default, also for stored settings from older versions.
    assert!(enabled(&conn));
    assert_eq!(logo_urls(&conn, "a").len(), 1);
    let mut old = serde_json::to_value(crate::types::Settings::default()).unwrap();
    old.as_object_mut().unwrap().remove("wikimediaEnabled");
    let old: crate::types::Settings = serde_json::from_value(old).unwrap();
    assert!(old.wikimedia_enabled);
    db::put_settings(
        &conn,
        crate::validation::settings_patch(&serde_json::json!({"wikimediaEnabled": false})).unwrap(),
    )
    .unwrap();
    assert!(!enabled(&conn));
    assert!(logo_urls(&conn, "a").is_empty());
    assert!(
        crate::validation::settings_patch(&serde_json::json!({"wikimediaEnabled": "yes"})).is_err()
    );
}

#[test]
fn image_records_keep_credits_and_referenced_urls_protect_cached_files() {
    let db = library(&["a", "b"]);
    let conn = db.lock();
    store(
        &conn,
        &outcome("a", Decision::Match("Q1".into()), "steam", "A"),
        1,
    )
    .unwrap();
    let chosen = logo("Chosen.png");
    put_images(&conn, &[&chosen], 1).unwrap();
    conn.execute(
        "INSERT INTO game_page_custom(gameId,logo,updatedAt) VALUES('b',?,1)",
        [&chosen.src],
    )
    .unwrap();
    let urls = referenced_image_urls(&conn).unwrap();
    assert!(urls.contains(&logo("Logo.png").src));
    assert!(urls.contains(&chosen.src));
    // Credits are found by URL and checked again.
    assert_eq!(image(&conn, &chosen.src).unwrap().licence, "Public domain");
    assert!(image(
        &conn,
        "https://upload.wikimedia.org/wikipedia/en/a/ab/Chosen.png"
    )
    .is_none());
    let credits = credits_for(
        &conn,
        &[(chosen.src.clone(), "lobby-cover://cache-x.png".into())],
    );
    assert_eq!(credits[0].display, "lobby-cover://cache-x.png");
    // A chosen logo outlives the match.
    delete(&conn, "a").unwrap();
    assert!(image(&conn, &chosen.src).is_some());
}

#[test]
fn only_offered_images_can_be_used() {
    let db = library(&["a"]);
    let conn = db.lock();
    let mut o = outcome("a", Decision::Match("Q1".into()), "steam", "A");
    o.metadata.as_mut().unwrap().covers.push(logo("Cover.png"));
    store(&conn, &o, 1).unwrap();
    assert!(offered(&conn, "a", &logo("Logo.png").src, "logo").is_ok());
    assert!(offered(&conn, "a", &logo("Cover.png").src, "cover").is_ok());
    assert!(offered(&conn, "a", &logo("Cover.png").src, "logo").is_err());
    assert!(offered(
        &conn,
        "a",
        "https://cdn2.steamgriddb.com/logo/x.png",
        "logo"
    )
    .is_err());
    assert!(offered(&conn, "zzz", &logo("Logo.png").src, "logo").is_err());
}

#[test]
fn inputs_come_from_the_library_and_the_igdb_cache() {
    let db = db::Db::memory().unwrap();
    let conn = db.lock();
    let game = Game {
        id: "steam:220".into(),
        title: "Half-Life 2".into(),
        store: "Steam".into(),
        routes: vec![GameRoute {
            source_id: "steam".into(),
            store: "Steam".into(),
            app_id: Some("220".into()),
            launch_target: "steam://rungameid/220".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    db::save_game(&conn, &GamePatch::from(&game)).unwrap();
    db::put_metadata(
        &conn,
        &game.id,
        &db::CachedMetadata {
            igdb_id: Some(72),
            json: Some(r#"{"igdbId":72,"name":"Half-Life 2","url":"https://www.igdb.com/games/half-life-2","releaseDate":1100563200000}"#.into()),
            fetched_at: 1,
            source: "auto".into(),
        },
    )
    .unwrap();
    let i = input_for(&conn, &game);
    assert_eq!(i.steam.as_deref(), Some("220"));
    assert_eq!(i.igdb.as_deref(), Some("half-life-2"));
    assert_eq!(i.year, Some(2004));
    assert_eq!(i.platform, Some("Q1406"));
    assert_eq!(i.known, None);
    // An earlier match is fetched directly.
    store(
        &conn,
        &outcome(
            &game.id,
            Decision::Match("Q193581".into()),
            "steam",
            "Half-Life 2",
        ),
        1,
    )
    .unwrap();
    assert_eq!(input_for(&conn, &game).known.as_deref(), Some("Q193581"));
    assert_eq!(
        igdb_slug("https://www.igdb.com/games/doom"),
        Some("doom".into())
    );
    assert_eq!(igdb_slug("https://www.igdb.com/companies/valve"), None);
    assert_eq!(year_of_ms(0), 1970);
    assert_eq!(year_of_ms(1_704_067_199_000), 2023);
    assert_eq!(year_of_ms(1_704_067_200_000), 2024);
    assert_eq!(year_of_ms(-1), 1969);
}

#[test]
fn covers_are_only_set_for_games_without_any() {
    let mut g = Game::default();
    assert!(no_cover(&g));
    g.default_cover = Some(" ".into());
    assert!(no_cover(&g));
    g.cover = Some("https://cdn2.steamgriddb.com/grid/a.png".into());
    assert!(!no_cover(&g));
    let mut cover = logo("Box.png");
    cover.original_width = 400;
    cover.original_height = 600;
    assert!(cover_shaped(&cover) && !low_trust_cover(&cover));
    cover.original_height = 599;
    assert!(low_trust_cover(&cover));
    cover.original_height = 300;
    assert!(!cover_shaped(&cover), "landscape is never a cover");
    assert!(is_cover_file("Doom classic box art.png") && is_cover_file("X cover (cropped).png"));
    assert!(!is_cover_file("2023-04-16 necronomi-con.jpg"));
}

#[test]
fn the_cover_cache_and_page_art_take_commons_uploads_only() {
    let ok = "https://upload.wikimedia.org/wikipedia/commons/thumb/d/d6/L.svg/500px-L.svg.png";
    let url = |u: &str| url::Url::parse(u).unwrap();
    assert!(crate::covercache::is_allowed(&url(ok)));
    for bad in [
        "https://upload.wikimedia.org/wikipedia/en/a/ab/Cover.jpg",
        "https://upload.wikimedia.org/wikipedia/commons/a/ab/L.svg",
        "http://upload.wikimedia.org/wikipedia/commons/a/ab/L.png",
        "https://upload.wikimedia.org/other/L.png",
        "https://commons.wikimedia.org/wiki/File:L.png",
    ] {
        assert!(!crate::covercache::is_allowed(&url(bad)), "{bad}");
        assert!(crate::validation::page_art_value(bad).is_err(), "{bad}");
    }
    // The other hosts are unchanged.
    assert!(crate::covercache::is_allowed(&url(
        "https://cdn2.steamgriddb.com/grid/a.png"
    )));
    assert!(crate::covercache::is_allowed(&url(
        "https://images.igdb.com/igdb/image/upload/t_1080p/abc.jpg"
    )));
    assert!(!crate::covercache::is_allowed(&url(
        "https://evil.example/a.png"
    )));
    assert_eq!(crate::validation::page_art_value(ok).unwrap(), ok);
    // Commons logos come after Steam's and SteamGridDB's, and a chosen logo stays first.
    let art = crate::types::GameArt {
        logo: vec!["https://cdn.akamai.steamstatic.com/steam/apps/1/logo.png".into()],
        ..Default::default()
    };
    let art = crate::art::with_wikimedia(art, vec![ok.into()]);
    assert_eq!(art.logo[1], ok);
    let custom = crate::types::GamePageCustom {
        logo: Some("lobby-cover://logo-a-1.png".into()),
        ..Default::default()
    };
    assert_eq!(
        crate::art::with_custom(art, custom).logo[0],
        "lobby-cover://logo-a-1.png"
    );
}
