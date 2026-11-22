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
    let kept = cached(&conn, "a").unwrap().unwrap();
    assert_eq!(
        (kept.qid.as_deref(), kept.source.as_str()),
        (Some("Q2"), "user")
    );
    // Its time moves on, so it isn't fetched again at once.
    assert_eq!(kept.fetched_at, 5);
    assert!(is_fresh(&kept, 5));
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
        &[(
            chosen.src.clone(),
            "lobby-cover://cache-x.png".into(),
            "hero",
        )],
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
fn cover_candidates_are_portrait_and_small_ones_are_marked() {
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

#[tokio::test]
async fn an_edition_whose_game_is_missing_is_asked_for_once() {
    let body = serde_json::json!({"entities": {
        "Q500": {"type": "item", "id": "Q500", "labels": {"en": {"language": "en", "value": "Game: Deluxe Edition"}},
                 "claims": {
                     "P31": [{"rank": "normal", "mainsnak": {"snaktype": "value", "datavalue": {"value": {"id": "Q60997816"}}}}],
                     "P629": [{"rank": "normal", "mainsnak": {"snaktype": "value", "datavalue": {"value": {"id": "Q999"}}}}]
                 }},
        "Q999": {"id": "Q999", "missing": ""}
    }})
    .to_string();
    let (base, log) = serve(Arc::new(move |req: &Req| match req.path() {
        "/wikidata" if req.param("props").as_deref() == Some("labels") => {
            json_reply(r#"{"entities":{}}"#)
        }
        "/wikidata" => json_reply(&body),
        "/wikipedia" => json_reply(r#"{"query":{"pages":[]}}"#),
        "/commons" => json_reply(r#"{"query":{"pages":[]}}"#),
        _ => (404, vec![], vec![]),
    }))
    .await;
    let limiter = Limiter::default();
    let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
    let known = GameInput {
        known: Some("Q500".into()),
        ..input("g", "Game", None)
    };
    let out = resolve_batch(&api, &[known]).await.unwrap();
    // The edition stands for itself when its game is gone.
    assert_eq!(out[0].decision, Decision::Match("Q500".into()));
    let entity_requests = log
        .lock()
        .unwrap()
        .iter()
        .filter(|r| {
            r.path() == "/wikidata"
                && r.param("action").as_deref() == Some("wbgetentities")
                && r.param("props").as_deref() != Some("labels")
        })
        .count();
    assert_eq!(entity_requests, 2, "the edition, then its game once");
}

#[test]
fn automatic_results_never_replace_or_claim_a_users_choice() {
    let db = library(&["a"]);
    let conn = db.lock();
    let mut chosen = row_for(
        &outcome("a", Decision::Match("Q2".into()), "known", "Two"),
        None,
        1,
    )
    .unwrap();
    chosen.source = "user".into();
    chosen.matched_by = "user".into();
    assert!(put(&conn, "a", &chosen).unwrap());
    let previous = cached(&conn, "a").unwrap();
    // The row for an automatic result is automatic, whatever was stored.
    let row = row_for(
        &outcome("a", Decision::Match("Q3".into()), "title", "Three"),
        previous.as_ref(),
        2,
    )
    .unwrap();
    assert_eq!(row.source, "auto");
    // A refresh that found another item (say it raced the user's choice) is not stored...
    assert!(!store(
        &conn,
        &outcome("a", Decision::Match("Q3".into()), "title", "Three"),
        2
    )
    .unwrap());
    assert!(!store(
        &conn,
        &outcome(
            "a",
            Decision::Ambiguous(vec!["Q3".into(), "Q4".into()]),
            "title",
            ""
        ),
        3
    )
    .unwrap());
    let kept = cached(&conn, "a").unwrap().unwrap();
    assert_eq!(
        (
            kept.qid.as_deref(),
            kept.source.as_str(),
            kept.status.as_str()
        ),
        (Some("Q2"), "user", "matched")
    );
    assert_eq!(metadata_of(&kept).unwrap().label, "Two");
    // ...a refresh of the chosen item is, and stays the user's.
    assert!(store(
        &conn,
        &outcome("a", Decision::Match("Q2".into()), "known", "Two again"),
        4
    )
    .unwrap());
    let kept = cached(&conn, "a").unwrap().unwrap();
    assert_eq!(
        (kept.source.as_str(), kept.matched_by.as_str()),
        ("user", "user")
    );
    assert_eq!(metadata_of(&kept).unwrap().label, "Two again");
}

#[test]
fn commons_images_show_only_with_a_credit_and_are_chosen_only_with_one() {
    let dir = std::env::temp_dir().join(format!("gl-wiki-{}", uuid::Uuid::new_v4()));
    let db = library(&["a"]);
    let conn = db.lock();
    let credited = logo("Credited.png");
    let bare = logo("Bare.png");
    put_images(&conn, &[&credited], 1).unwrap();
    let art = crate::types::GameArt {
        hero: vec![
            bare.src.clone(),
            "https://cdn2.steamgriddb.com/hero/a.png".into(),
        ],
        logo: vec![credited.src.clone(), bare.src.clone()],
        ..Default::default()
    };
    let art = crate::art::with_credits(&conn, &dir, art);
    assert_eq!(art.hero, vec!["https://cdn2.steamgriddb.com/hero/a.png"]);
    assert_eq!(art.logo, vec![credited.src.clone()]);
    assert_eq!(art.credits.len(), 1);
    assert_eq!(art.credits[0].kind, "logo");
    // Choosing: only with a credit record.
    let slot = crate::art::Slot::Logo;
    assert!(crate::art::set_image(&conn, &dir, "a", slot, Some(&bare.src)).is_err());
    assert_eq!(
        crate::art::set_image(&conn, &dir, "a", slot, Some(&credited.src))
            .unwrap()
            .logo
            .as_deref(),
        Some(credited.src.as_str())
    );
    // Credits carry the slot they are shown in.
    let hero = credits_for(&conn, &[(credited.src.clone(), "x".into(), "hero")]);
    assert_eq!(hero[0].kind, "hero");
}

#[tokio::test]
async fn chosen_images_are_rechecked_on_commons() {
    let (base, _) = serve(Arc::new(fixtures)).await;
    let limiter = Limiter::default();
    let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
    let db = library(&["a"]);
    let kept = "https://upload.wikimedia.org/wikipedia/commons/thumb/d/d6/Half-Life_2_text_logo.svg/500px-Half-Life_2_text_logo.svg.png".to_string();
    let gone =
        "https://upload.wikimedia.org/wikipedia/commons/a/ab/Papers_Please_cover.jpg".to_string();
    put_images(
        &db.lock(),
        &[&CommonsImage {
            src: gone.clone(),
            ..logo("Papers Please cover.jpg")
        }],
        1,
    )
    .unwrap();
    let verdicts = recheck(&api, &db, &[kept.clone(), gone.clone()])
        .await
        .unwrap();
    assert!(verdicts[&kept]);
    assert!(!verdicts[&gone]);
    let conn = db.lock();
    let credit = image(&conn, &kept).unwrap();
    assert_eq!(
        (credit.file.as_str(), credit.licence.as_str()),
        ("Half-Life 2 text logo.svg", "Public domain")
    );
    assert!(
        image(&conn, &gone).is_none(),
        "a file Commons no longer has loses its credit"
    );
    assert_eq!(
        file_of_url(&kept).as_deref(),
        Some("Half-Life 2 text logo.svg")
    );
    assert_eq!(
        file_of_url("https://upload.wikimedia.org/wikipedia/commons/a/ab/Doom_%E2%80%93_logo.png")
            .as_deref(),
        Some("Doom – logo.png")
    );
    assert_eq!(
        file_of_url("https://upload.wikimedia.org/wikipedia/en/a/ab/X.png"),
        None
    );
}

#[test]
fn a_rate_limit_pauses_every_request() {
    let state = WikimediaState::default();
    assert!(!state.rate_limited());
    assert!(state.api().is_ok());
    let _ = state.note::<()>(Err(WikiError::Http(500)));
    assert!(!state.rate_limited());
    let _ = state.note::<()>(Err(WikiError::RateLimited));
    assert!(state.rate_limited());
    assert_eq!(state.api().err(), Some(WikiError::RateLimited));
}

fn info_page(title: &str, licence: &str) -> serde_json::Value {
    let name = title.trim_start_matches("File:").replace(' ', "_");
    serde_json::json!({
        "ns": 6, "title": title, "imagerepository": "local",
        "imageinfo": [{
            "width": 400, "height": 100, "mime": "image/png",
            "url": format!("https://upload.wikimedia.org/wikipedia/commons/a/ab/{name}"),
            "descriptionurl": format!("https://commons.wikimedia.org/wiki/{}", title.replace(' ', "_")),
            "extmetadata": {"License": {"value": licence}, "LicenseShortName": {"value": ""}}
        }]
    })
}

#[test]
fn only_explicit_answers_decide_about_a_file() {
    let want = |names: &[&str]| {
        names
            .iter()
            .map(|n| licence::file_key(n))
            .collect::<Vec<_>>()
    };
    // API errors and answers without pages are errors, never "gone".
    assert!(parse_imageinfo(
        r#"{"error":{"code":"internal_api_error","info":"x"}}"#,
        &want(&["A.png"])
    )
    .is_err());
    assert!(parse_imageinfo(r#"{"batchcomplete":true}"#, &want(&["A.png"])).is_err());
    assert!(parse_imageinfo(r#"{"query":{}}"#, &want(&["A.png"])).is_err());
    let body = serde_json::json!({"query": {
        "redirects": [{"from": "File:Old name.png", "to": "File:New name.png"}],
        "pages": [
            info_page("File:New name.png", "cc0"),
            info_page("File:Nc.png", "cc-by-nc-4.0"),
            {"ns": 6, "title": "File:Deleted.png", "missing": true, "imagerepository": ""},
            {"ns": 6, "title": "File:No info.png", "imagerepository": "local"}
        ]
    }})
    .to_string();
    let got = parse_imageinfo(
        &body,
        &want(&[
            "Old name.png",
            "Nc.png",
            "Deleted.png",
            "No info.png",
            "Not asked about.png",
        ]),
    )
    .unwrap();
    // A renamed file follows the redirect.
    assert!(matches!(&got["Old name.png"], FileVerdict::Free(i) if i.file == "New name.png"));
    assert_eq!(got["Nc.png"], FileVerdict::Refused);
    assert_eq!(got["Deleted.png"], FileVerdict::Refused);
    // Nothing definite: no verdict.
    assert!(!got.contains_key("No info.png"));
    assert!(!got.contains_key("Not asked about.png"));
}

#[tokio::test]
async fn odd_commons_answers_leave_chosen_images_and_credits_in_place() {
    for body in [
        r#"{"error":{"code":"readonly","info":"x"}}"#,
        r#"{"batchcomplete":true}"#,
        r#"{"query":{"pages":[{"ns":6,"title":"File:Chosen.png","imagerepository":"local"}]}}"#,
    ] {
        let reply = body.to_string();
        let (base, _) = serve(Arc::new(move |_: &Req| json_reply(&reply))).await;
        let limiter = Limiter::default();
        let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
        let db = library(&["a"]);
        let chosen = logo("Chosen.png");
        put_images(&db.lock(), &[&chosen], 1).unwrap();
        if let Ok(verdicts) = recheck(&api, &db, std::slice::from_ref(&chosen.src)).await {
            assert!(verdicts.is_empty(), "{body}");
        }
        assert!(
            image(&db.lock(), &chosen.src).is_some(),
            "{body}: the credit stays"
        );
    }
}

#[tokio::test]
async fn renamed_files_keep_their_credit_and_explicit_answers_still_remove() {
    let body = serde_json::json!({"query": {
        "redirects": [{"from": "File:Chosen.png", "to": "File:Chosen renamed.png"}],
        "pages": [
            info_page("File:Chosen renamed.png", "pd"),
            info_page("File:Now nc.png", "cc-by-nc-sa-4.0"),
            {"ns": 6, "title": "File:Deleted.png", "missing": true, "imagerepository": ""}
        ]
    }})
    .to_string();
    let (base, log) = serve(Arc::new(move |_: &Req| json_reply(&body))).await;
    let limiter = Limiter::default();
    let api = Api::new(Endpoints::local(&base), &limiter).unwrap();
    let db = library(&["a"]);
    let urls: Vec<String> = ["Chosen.png", "Now nc.png", "Deleted.png"]
        .iter()
        .map(|f| logo(f).src)
        .collect();
    for f in ["Chosen.png", "Now nc.png", "Deleted.png"] {
        put_images(&db.lock(), &[&logo(f)], 1).unwrap();
    }
    let verdicts = recheck(&api, &db, &urls).await.unwrap();
    assert!(verdicts[&urls[0]]);
    assert!(!verdicts[&urls[1]] && !verdicts[&urls[2]]);
    let conn = db.lock();
    // The chosen display URL keeps its (renamed file's) credit.
    assert_eq!(image(&conn, &urls[0]).unwrap().file, "Chosen renamed.png");
    assert!(image(&conn, &urls[1]).is_none() && image(&conn, &urls[2]).is_none());
    assert_eq!(
        log.lock().unwrap()[0].param("redirects").as_deref(),
        Some("1")
    );
}

#[test]
fn credit_lookups_wait_an_hour_per_url() {
    let state = WikimediaState::default();
    let start = Instant::now();
    let a = "https://upload.wikimedia.org/wikipedia/commons/a/ab/A.png".to_string();
    let b = "https://upload.wikimedia.org/wikipedia/commons/a/ab/B.png".to_string();
    assert_eq!(
        state.claim_credit_lookups(vec![a.clone(), a.clone()], start),
        vec![a.clone()]
    );
    // Within the hour: only the new URL.
    let soon = start + Duration::from_secs(60);
    assert_eq!(
        state.claim_credit_lookups(vec![a.clone(), b.clone()], soon),
        vec![b.clone()]
    );
    assert!(state
        .claim_credit_lookups(vec![a.clone(), b.clone()], soon)
        .is_empty());
    // After it: again.
    let later = start + CREDIT_RETRY_AFTER + Duration::from_secs(61);
    assert_eq!(
        state.claim_credit_lookups(vec![a.clone(), b.clone()], later),
        vec![a, b]
    );
}

#[test]
fn uncredited_commons_art_is_listed_for_a_background_lookup() {
    let db = library(&["a"]);
    let conn = db.lock();
    let bare = logo("Bare.png");
    let credited = logo("Credited.png");
    put_images(&conn, &[&credited], 1).unwrap();
    conn.execute(
        "INSERT INTO game_page_custom(gameId,hero,logo,updatedAt) VALUES('a',?1,?2,1)",
        [&credited.src, &bare.src],
    )
    .unwrap();
    assert_eq!(crate::art::uncredited(&conn, "a"), vec![bare.src]);
}
