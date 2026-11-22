//! Public client credentials of store launchers that GameLobbies uses to sign in where the store
//! offers no app registration for third parties. These are not secrets: they are the values
//! the stores' own desktop clients ship with, published in the source code of open-source
//! launchers. Using them makes GameLobbies talk to the store as that client would, which the
//! store has not approved: it is unofficial and can stop working at any time.

/// GOG Galaxy's public OAuth client (the same values the open-source launchers Heroic, Lutris
/// and minigalaxy use for GOG sign-in).
pub const GOG_CLIENT_ID: &str = "46899977096215655";
/// See [`GOG_CLIENT_ID`]. Public, shipped with GOG Galaxy; not a credential of the user.
pub const GOG_CLIENT_SECRET: &str =
    "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";
/// Where GOG sends the browser after sign-in; the user pastes that address into GameLobbies.
pub const GOG_REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";
