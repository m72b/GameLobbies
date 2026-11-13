//! Store library importers. Each returns normalized games with one or more launch routes.

pub mod epic;
pub mod gog;
pub mod registry;
pub mod steam;
pub mod xbox;

use crate::types::{Game, GameRoute};
use std::collections::BTreeMap;

/// Builds a game with a single route — the common shape every importer produces.
pub fn single_route_game(
    id: String,
    title: String,
    store: &str,
    cover: Option<String>,
    route: GameRoute,
) -> Game {
    Game {
        id,
        title,
        cover: cover.clone(),
        default_cover: cover,
        store: store.into(),
        routes: vec![route],
        ..Default::default()
    }
}

pub fn states(source: &str, installed: bool) -> Option<BTreeMap<String, bool>> {
    Some([(source.to_string(), installed)].into())
}
