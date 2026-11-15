//! Launches waiting for a custom launcher to open.
//!
//! Some launchers need a while to start, sign in and accept being woken up. When a game is played
//! through a launcher whose program isn't running, `launch::launch_game` starts the program on
//! its own (no game arguments) and keeps the launch here; the renderer shows "Waiting for ..."
//! and the user's Continue only confirms the pending id (`library_launch_continue`). The renderer
//! can't change what runs: a pending launch holds the game id and a copy of the route that was
//! checked, and Continue launches that same route again (re-read from the library and checked
//! again), or fails when it changed.
//!
//! Like native picks (`picks`), a pending launch is one-shot (taken by the first Continue),
//! short-lived ([`PENDING_LIFETIME`]), and there is at most one per game (a new one replaces it).

use crate::types::GameRoute;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(game: &str) -> Pending {
        Pending {
            game_id: game.into(),
            launcher_id: "heroic".into(),
            launcher_name: "Heroic".into(),
            program: r"D:\Launchers\Heroic\Heroic.exe".into(),
            route: GameRoute {
                source_id: "launcher:heroic".into(),
                store: "Custom".into(),
                launch_target: "hk".into(),
                ..Default::default()
            },
        }
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
