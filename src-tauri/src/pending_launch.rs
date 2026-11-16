//! Launches waiting for an "open first" custom launcher to open.
//!
//! Some launchers must be open and signed in before their games' files can be used. When a game
//! of such a launcher (`launchers::StartMode::OpenFirst`) is played while its program isn't
//! running, `launch::launch_game` starts the program on its own (no arguments) and keeps the
//! launch here; the renderer shows "Waiting for ..." and the user's Continue only confirms the
//! pending id (`library_launch_continue`). The renderer can't change what runs: a pending launch
//! holds the game id, the launcher's program and start mode, and a copy of the route that was
//! checked, and Continue starts the game from that same route (re-read from the library and
//! checked again, directly from its own file), or fails when the route, the program or the start
//! mode changed. Launchers that run games through their program never wait: they are started
//! once, with the game.
//!
//! Like native picks (`picks`), a pending launch is one-shot (taken by the first Continue),
//! short-lived ([`PENDING_LIFETIME`]), and there is at most one per game (a new one replaces it).

use crate::launchers::{CustomLauncher, StartMode};
use crate::types::{Game, GameRoute};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long Continue stays possible (time to start a launcher and sign in).
pub const PENDING_LIFETIME: Duration = Duration::from_secs(5 * 60);

/// A launch waiting for its launcher.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub game_id: String,
    pub launcher_id: String,
    pub launcher_name: String,
    /// The launcher's program as it was when the launch was checked (Continue refuses to run a
    /// launcher whose program changed since).
    pub program: String,
    /// The launcher's start mode when the launch was checked (Continue refuses when it changed).
    pub start_mode: StartMode,
    /// The route that was checked (Continue launches the stored route that still equals it).
    pub route: GameRoute,
}

#[derive(Debug)]
struct Entry {
    token: String,
    pending: Pending,
    at: Instant,
}

#[derive(Debug, Default)]
pub struct PendingLaunches {
    /// By game id: one per game.
    entries: Mutex<HashMap<String, Entry>>,
}

impl PendingLaunches {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Keeps a launch and returns its id (random, 128 bits). Replaces the game's previous one.
    pub fn begin(&self, pending: Pending) -> String {
        self.begin_at(pending, Instant::now())
    }

    fn begin_at(&self, pending: Pending, now: Instant) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let mut entries = self.lock();
        entries.retain(|_, e| alive(e, now));
        entries.insert(
            pending.game_id.clone(),
            Entry {
                token: token.clone(),
                pending,
                at: now,
            },
        );
        token
    }

    /// The pending launch `token` names, while it lasts (it stays pending).
    pub fn peek(&self, token: &str) -> Option<Pending> {
        self.peek_at(token, Instant::now())
    }

    fn peek_at(&self, token: &str, now: Instant) -> Option<Pending> {
        let mut entries = self.lock();
        entries.retain(|_, e| alive(e, now));
        entries
            .values()
            .find(|e| e.token == token)
            .map(|e| e.pending.clone())
    }

    /// Takes game `game_id`'s pending launch `token` (once): an unknown, used, expired or
    /// replaced id, or one of another game, is refused.
    pub fn take(&self, game_id: &str, token: &str) -> Result<Pending> {
        self.take_at(game_id, token, Instant::now())
    }

    fn take_at(&self, game_id: &str, token: &str, now: Instant) -> Result<Pending> {
        let mut entries = self.lock();
        entries.retain(|_, e| alive(e, now));
        match entries.get(game_id) {
            Some(e) if e.token == token => {}
            _ => bail!(
                "This launch is no longer waiting (it expired or was cancelled). Press Play again."
            ),
        }
        Ok(entries
            .remove(game_id)
            .map(|e| e.pending)
            .expect("checked above"))
    }

    /// Drops pending launch `token`; true when it was there.
    pub fn cancel(&self, token: &str) -> bool {
        let mut entries = self.lock();
        let before = entries.len();
        entries.retain(|_, e| e.token != token);
        before != entries.len()
    }
}

fn alive(e: &Entry, now: Instant) -> bool {
    now.saturating_duration_since(e.at) < PENDING_LIFETIME
}

/// Whether Continue may go on with the launcher as it is now (`None`: deleted): still the program
/// that was opened, and still opened first. Continue starts the game directly, which is only what
/// the user chose while the launcher works that way (`launch_game` checks everything else again).
pub fn check_launcher(pending: &Pending, now: Option<&CustomLauncher>) -> Result<()> {
    let same = now.is_some_and(|l| {
        l.executable == pending.program
            && l.start_mode == pending.start_mode
            && l.start_mode == StartMode::OpenFirst
    });
    if !same {
        bail!(
            "{} changed since you pressed Play. Press Play again.",
            pending.launcher_name
        );
    }
    Ok(())
}

/// Where the route a pending launch checked is in `game` now: the same launcher, target and
/// launch options (its install state may change meanwhile). `None` when it changed: Continue
/// then refuses instead of launching something else (`launch_game` checks everything again
/// anyway, launch options against their confirmation).
pub fn route_index(game: &Game, checked: &GameRoute) -> Option<usize> {
    game.routes.iter().position(|r| {
        r.source_id == checked.source_id
            && r.launch_target == checked.launch_target
            && r.launch_args == checked.launch_args
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(game: &str) -> Pending {
        Pending {
            game_id: game.into(),
            launcher_id: "heroic".into(),
            launcher_name: "Heroic".into(),
            program: r"D:\Launchers\Heroic\Heroic.exe".into(),
            start_mode: StartMode::OpenFirst,
            route: GameRoute {
                source_id: "launcher:heroic".into(),
                store: "Custom".into(),
                launch_target: "hk".into(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn continue_finds_only_the_route_that_was_checked() {
        let checked = pending("a").route;
        let with = |args: Option<&str>, installed: Option<bool>| GameRoute {
            launch_args: args.map(str::to_owned),
            installed,
            ..checked.clone()
        };
        let game = |routes: Vec<GameRoute>| Game {
            id: "a".into(),
            routes,
            ..Default::default()
        };
        let other = GameRoute {
            source_id: "steam".into(),
            ..Default::default()
        };
        assert_eq!(
            route_index(&game(vec![other.clone(), with(None, Some(true))]), &checked),
            Some(1),
            "install state may change"
        );
        // Launch options changed (or were added) since Play: not the route that was checked.
        assert_eq!(
            route_index(&game(vec![with(Some(r#"["-x"]"#), None)]), &checked),
            None
        );
        let with_args = with(Some(r#"["-x"]"#), None);
        assert_eq!(
            route_index(&game(vec![with(Some(r#"["-y"]"#), None)]), &with_args),
            None
        );
        assert_eq!(route_index(&game(vec![with(None, None)]), &with_args), None);
        assert_eq!(route_index(&game(vec![other]), &checked), None);
    }

    #[test]
    fn continue_needs_the_same_open_first_launcher() {
        let p = pending("a");
        let now = CustomLauncher {
            id: "heroic".into(),
            name: "Heroic".into(),
            icon_kind: "builtin".into(),
            icon_value: "rocket".into(),
            color: "#4f8cff".into(),
            executable: p.program.clone(),
            args_template: String::new(),
            target_kind: "file".into(),
            target_kinds: vec!["file".into()],
            working_dir: None,
            start_mode: StartMode::OpenFirst,
            sort_order: 0,
            created_at: 1,
            available: false,
        };
        assert!(check_launcher(&p, Some(&now)).is_ok());
        assert!(check_launcher(&p, None).is_err(), "deleted");
        // Switched to "through" while waiting: Continue must not start the game directly.
        let through = CustomLauncher {
            start_mode: StartMode::Through,
            ..now.clone()
        };
        assert!(check_launcher(&p, Some(&through)).is_err());
        let moved = CustomLauncher {
            executable: r"D:\Other\Heroic.exe".into(),
            ..now.clone()
        };
        assert!(check_launcher(&p, Some(&moved)).is_err());
        // A pending launch recorded for "through" (none are made any more) never continues.
        let old = Pending {
            start_mode: StartMode::Through,
            ..p.clone()
        };
        assert!(check_launcher(&old, Some(&through)).is_err());
    }

    #[test]
    fn a_pending_launch_is_taken_once() {
        let launches = PendingLaunches::default();
        let token = launches.begin(pending("a"));
        assert_eq!(token.len(), 32);
        assert_eq!(launches.peek(&token).unwrap().game_id, "a");
        assert_eq!(launches.take("a", &token).unwrap(), pending("a"));
        assert!(launches.take("a", &token).is_err(), "one-shot");
        assert!(launches.peek(&token).is_none());
        assert!(launches.take("a", "").is_err());
        assert!(launches.take("a", "not-a-token").is_err());
    }

    #[test]
    fn a_pending_launch_is_bound_to_its_game() {
        let launches = PendingLaunches::default();
        let a = launches.begin(pending("a"));
        let b = launches.begin(pending("b"));
        assert!(launches.take("b", &a).is_err(), "a's id can't launch b");
        assert!(launches.take("a", &b).is_err(), "b's id can't launch a");
        // A refused attempt doesn't use the right game's launch up.
        assert_eq!(launches.take("a", &a).unwrap().game_id, "a");
        assert_eq!(launches.take("b", &b).unwrap().game_id, "b");
    }

    #[test]
    fn a_pending_launch_expires_and_is_replaced_per_game() {
        let launches = PendingLaunches::default();
        let start = Instant::now();
        let token = launches.begin_at(pending("a"), start);
        let almost = start + PENDING_LIFETIME - Duration::from_secs(1);
        assert!(launches.peek_at(&token, almost).is_some());
        assert!(launches
            .take_at("a", &token, start + PENDING_LIFETIME)
            .is_err());
        assert!(
            launches.peek_at(&token, start).is_none(),
            "expired ones are dropped"
        );

        // One per game: a second Play replaces the first.
        let first = launches.begin_at(pending("a"), start);
        let second = launches.begin_at(pending("a"), start);
        assert_ne!(first, second);
        assert!(launches.take_at("a", &first, start).is_err());
        assert!(launches.take_at("a", &second, start).is_ok());

        // Cancel drops it.
        let token = launches.begin(pending("a"));
        assert!(launches.cancel(&token));
        assert!(!launches.cancel(&token));
        assert!(launches.take("a", &token).is_err());
    }
}
