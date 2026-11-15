//! API keys stored DPAPI-encrypted (current Windows user) in the `secrets` table.
//!
//! Plaintext keys never leave the backend: the renderer only ever sees `…Set: bool` flags,
//! and no key is written to logs, error messages or URLs.

use crate::db;
use crate::state::AppState;
use anyhow::{anyhow, bail, Result};

/// Secret names of removed features, deleted by the database migration.
pub const RETIRED_SECRETS: &[&str] = &["steam-api-key-dpapi"];
const STEAMGRIDDB_API_KEY: &str = "steamgriddb-api-key-dpapi";
const IGDB_CLIENT_ID: &str = "igdb-client-id-dpapi";
const IGDB_CLIENT_SECRET: &str = "igdb-client-secret-dpapi";

fn has(state: &AppState, name: &str) -> Result<bool> {
    Ok(db::read_secret(&state.db.lock(), name)?.is_some())
}

fn read(state: &AppState, name: &str, service: &str) -> Result<Option<String>> {
    let Some(sealed) = db::read_secret(&state.db.lock(), name)? else {
        return Ok(None);
    };
    let plain = crate::platform::unprotect(&sealed).map_err(|_| {
        anyhow!("Windows secure storage is unavailable; {service} credentials cannot be decrypted.")
    })?;
    String::from_utf8(plain)
        .map(Some)
        .map_err(|_| anyhow!("The stored {service} API key is unreadable; enter it again."))
}

fn write(state: &AppState, name: &str, service: &str, key: &str) -> Result<()> {
    let sealed = crate::platform::protect(key.as_bytes()).map_err(|_| {
        anyhow!("Windows secure storage is unavailable; the {service} API key was not saved.")
    })?;
    db::write_secret(&state.db.lock(), name, &sealed)
}

/// Which credentials are stored, as booleans only (for the diagnostics report).
pub struct StoredFlags {
    pub steamgriddb_key: bool,
    pub igdb_client_id: bool,
    pub igdb_client_secret: bool,
}

pub fn stored_flags(conn: &rusqlite::Connection) -> Result<StoredFlags> {
    let has = |name: &str| -> Result<bool> { Ok(db::read_secret(conn, name)?.is_some()) };
    Ok(StoredFlags {
        steamgriddb_key: has(STEAMGRIDDB_API_KEY)?,
        igdb_client_id: has(IGDB_CLIENT_ID)?,
        igdb_client_secret: has(IGDB_CLIENT_SECRET)?,
    })
}

// ---------------------------------------------------------------------------
// SteamGridDB

/// Trims the input; `None` means "clear the stored key". SteamGridDB keys are 32 hex
/// characters; anything that is not short ASCII alphanumeric text is rejected. The error
/// never echoes the input.
pub fn validate_steamgriddb_key(raw: &str) -> Result<Option<String>> {
    if raw.len() > 1024 {
        bail!("The SteamGridDB API key is too long.");
    }
    let key = raw.trim();
    if key.is_empty() {
        return Ok(None);
    }
    if key.len() > 128 || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("The SteamGridDB API key must be up to 128 letters and digits.");
    }
    Ok(Some(key.to_string()))
}

pub fn has_steamgriddb_api_key(state: &AppState) -> Result<bool> {
    has(state, STEAMGRIDDB_API_KEY)
}

pub fn get_steamgriddb_api_key(state: &AppState) -> Result<Option<String>> {
    read(state, STEAMGRIDDB_API_KEY, "SteamGridDB")
}

/// Saves the key, or clears it when the input is empty.
pub fn set_steamgriddb_api_key(state: &AppState, value: &str) -> Result<()> {
    match validate_steamgriddb_key(value)? {
        Some(key) => write(state, STEAMGRIDDB_API_KEY, "SteamGridDB", &key),
        None => db::delete_secret(&state.db.lock(), STEAMGRIDDB_API_KEY),
    }
}

// ---------------------------------------------------------------------------
// IGDB (Twitch application credentials)

/// Trims a Twitch Client ID or Client Secret; `None` when empty. Both are 30 lowercase
/// letters and digits today; anything that is not short ASCII alphanumeric text is rejected.
/// The error names the field but never echoes the input.
pub fn validate_igdb_credential(raw: &str, what: &str) -> Result<Option<String>> {
    if raw.len() > 1024 {
        bail!("The IGDB {what} is too long.");
    }
    let value = raw.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > 128 || !value.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("The IGDB {what} must be up to 128 letters and digits.");
    }
    Ok(Some(value.to_string()))
}

pub fn has_igdb_client_id(state: &AppState) -> Result<bool> {
    has(state, IGDB_CLIENT_ID)
}

pub fn has_igdb_client_secret(state: &AppState) -> Result<bool> {
    has(state, IGDB_CLIENT_SECRET)
}

/// Both values, or `None` unless both are saved. Backend only; never sent to the renderer.
pub fn get_igdb_credentials(state: &AppState) -> Result<Option<(String, String)>> {
    let Some(id) = read(state, IGDB_CLIENT_ID, "IGDB")? else {
        return Ok(None);
    };
    let Some(secret) = read(state, IGDB_CLIENT_SECRET, "IGDB")? else {
        return Ok(None);
    };
    Ok(Some((id, secret)))
}

/// Saves whichever values are non-empty; an empty field keeps the stored value (the form
/// never shows saved values). Both inputs are validated before anything is written.
pub fn set_igdb_credentials(state: &AppState, client_id: &str, client_secret: &str) -> Result<()> {
    let id = validate_igdb_credential(client_id, "Client ID")?;
    let secret = validate_igdb_credential(client_secret, "Client Secret")?;
    if let Some(id) = id {
        write(state, IGDB_CLIENT_ID, "IGDB", &id)?;
    }
    if let Some(secret) = secret {
        write(state, IGDB_CLIENT_SECRET, "IGDB", &secret)?;
    }
    Ok(())
}

pub fn clear_igdb_credentials(state: &AppState) -> Result<()> {
    let conn = state.db.lock();
    db::delete_secret(&conn, IGDB_CLIENT_ID)?;
    db::delete_secret(&conn, IGDB_CLIENT_SECRET)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steamgriddb_keys_are_trimmed_and_checked() {
        assert_eq!(
            validate_steamgriddb_key("  0123456789abcdef0123456789ABCDEF \n").unwrap(),
            Some("0123456789abcdef0123456789ABCDEF".into())
        );
        assert_eq!(validate_steamgriddb_key("").unwrap(), None);
        assert_eq!(validate_steamgriddb_key("   ").unwrap(), None);
        for bad in [
            "abc def",
            "abc-def",
            "key\"; drop",
            "ключ",
            &"a".repeat(129),
        ] {
            let e = validate_steamgriddb_key(bad).unwrap_err().to_string();
            assert!(!e.contains(bad), "error must not echo the key");
        }
        assert!(validate_steamgriddb_key(&"a".repeat(128)).is_ok());
        assert!(validate_steamgriddb_key(&" ".repeat(2000)).is_err());
    }

    #[test]
    fn igdb_credentials_are_trimmed_and_checked_without_echoing() {
        assert_eq!(
            validate_igdb_credential(
                " abcdefghij0123456789abcdefghij
",
                "Client ID"
            )
            .unwrap(),
            Some("abcdefghij0123456789abcdefghij".into())
        );
        assert_eq!(
            validate_igdb_credential("  ", "Client Secret").unwrap(),
            None
        );
        for bad in [
            "sec ret",
            "secret&grant_type=x",
            "s\"x",
            "сек",
            &"a".repeat(129),
        ] {
            let e = validate_igdb_credential(bad, "Client Secret")
                .unwrap_err()
                .to_string();
            assert!(!e.contains(bad), "error must not echo the secret");
            assert!(e.contains("Client Secret"));
        }
    }
}
