//! API keys stored DPAPI-encrypted (current Windows user) in the `secrets` table.
//!
//! Plaintext keys never leave the backend: the renderer only ever sees `…Set: bool` flags,
//! and no key is written to logs, error messages or URLs (except the Steam Web API, which
//! only accepts its key as a query parameter).

use crate::db;
use crate::state::AppState;
use anyhow::{anyhow, bail, Result};

/// Stored under a new key: the Electron build's `safeStorage` blobs can't be read here,
/// so users re-enter their Steam API key once after upgrading.
const STEAM_API_KEY: &str = "steam-api-key-dpapi";
const STEAMGRIDDB_API_KEY: &str = "steamgriddb-api-key-dpapi";

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

// ---------------------------------------------------------------------------
// Steam Web API

pub fn has_steam_api_key(state: &AppState) -> Result<bool> {
    has(state, STEAM_API_KEY)
}

pub fn get_steam_api_key(state: &AppState) -> Result<Option<String>> {
    read(state, STEAM_API_KEY, "Steam API")
}

/// An empty value keeps the stored key (the connections form leaves the field blank).
pub fn set_steam_api_key(state: &AppState, value: &str) -> Result<()> {
    let key = value.trim();
    if key.is_empty() {
        return Ok(());
    }
    write(state, STEAM_API_KEY, "Steam", key)
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
}
