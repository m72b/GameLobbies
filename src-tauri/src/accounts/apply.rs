//! Shared rules for applying a store's list of owned games to the library: the mass-removal
//! guard (also used by the Steam owned-games import) and the apply step of the cookie-based
//! store accounts (GOG, Battle.net, Humble).
//!
//! Account games are added only when the library has nothing for them yet: a game with the same
//! id, a route of the same store with the same id (an installed game the store's own scan found),
//! or a game of the same store with the same title is left exactly as it is. New entries are
//! owned, not installed, with an `<store>-account` route; titles, covers, favourites, notes and
//! routes of existing games are never changed. Only games this import created can be taken out
//! again, only after a complete list, and behind the mass-removal guard.

use std::collections::HashSet;

/// Smallest earlier list the mass-removal guard applies to.
pub const MASS_REMOVAL_MIN: usize = 20;
/// How long a held-back removal must stand before a later fetch may apply it.
pub const PENDING_MIN_AGE_MS: i64 = 6 * 60 * 60 * 1000;

/// What a fetch may take out of the library now, and what it holds back.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemovalPlan {
    pub to_drop: HashSet<String>,
    pub held: HashSet<String>,
    /// When the held set was first held back (0 = nothing held).
    pub pending_since: i64,
}

/// The mass-removal guard. `previous`: ids the import added before; `gone`: those of them the
/// new list no longer has; `was_pending` / `pending_since`: what an earlier fetch held back.
///
/// A list that drops more than half of an earlier list of [`MASS_REMOVAL_MIN`] or more is more
/// likely degraded than true: nothing is removed until a later fetch, at least
/// [`PENDING_MIN_AGE_MS`] after the first, reports (nearly, 90 %) the same ids gone. The clock
/// starts when a removal is first held; a different held set restarts it.
pub fn plan_removal(
    previous: &HashSet<String>,
    gone: &HashSet<String>,
    was_pending: &HashSet<String>,
    pending_since: i64,
    now: i64,
) -> RemovalPlan {
    let suspicious = previous.len() >= MASS_REMOVAL_MIN && gone.len() * 2 > previous.len();
    let (to_drop, held): (HashSet<String>, HashSet<String>) = if suspicious {
        let agreed: HashSet<String> = gone.intersection(was_pending).cloned().collect();
        let old_enough = !was_pending.is_empty()
            && pending_since > 0
            && now >= pending_since
            && now - pending_since >= PENDING_MIN_AGE_MS;
        if old_enough && agreed.len() * 10 >= was_pending.len() * 9 {
            let held = gone.difference(&agreed).cloned().collect();
            (agreed, held)
        } else {
            (HashSet::new(), gone.clone())
        }
    } else {
        (gone.clone(), HashSet::new())
    };
    let pending_since = if held.is_empty() {
        0
    } else if !was_pending.is_empty() && pending_since > 0 && held == *was_pending {
        pending_since
    } else {
        now
    };
    RemovalPlan {
        to_drop,
        held,
        pending_since,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(range: std::ops::RangeInclusive<usize>) -> HashSet<String> {
        range.map(|i| i.to_string()).collect()
    }

    #[test]
    fn small_or_partial_removals_apply_at_once() {
        let plan = plan_removal(&ids(1..=10), &ids(1..=9), &HashSet::new(), 0, 5);
        assert_eq!(plan.to_drop.len(), 9);
        let plan = plan_removal(&ids(1..=100), &ids(1..=40), &HashSet::new(), 0, 5);
        assert_eq!(
            (plan.to_drop.len(), plan.held.len(), plan.pending_since),
            (40, 0, 0)
        );
    }

    #[test]
    fn a_mass_removal_waits_for_an_agreeing_fetch_six_hours_later() {
        let previous = ids(1..=100);
        let gone = ids(2..=100);
        let first = plan_removal(&previous, &gone, &HashSet::new(), 0, 1000);
        assert!(first.to_drop.is_empty());
        assert_eq!((first.held.len(), first.pending_since), (99, 1000));
        let early = plan_removal(
            &previous,
            &gone,
            &first.held,
            1000,
            1000 + PENDING_MIN_AGE_MS - 1,
        );
        assert!(early.to_drop.is_empty());
        assert_eq!(early.pending_since, 1000);
        let later = plan_removal(
            &previous,
            &gone,
            &first.held,
            1000,
            1000 + PENDING_MIN_AGE_MS,
        );
        assert_eq!(
            (later.to_drop.len(), later.held.len(), later.pending_since),
            (99, 0, 0)
        );
        // A different held set restarts the clock.
        let other = plan_removal(&previous, &ids(50..=100), &first.held, 1000, 9_000_000_000);
        assert!(other.to_drop.is_empty());
        assert_eq!(other.pending_since, 9_000_000_000);
    }
}
