//! A store account's session, sealed with DPAPI (current Windows user) in the `secrets` table as
//! `account-<store>-session-dpapi`. Only Rust reads it; the renderer learns whether one exists,
//! the diagnostics report says yes/no, backups never contain it, and `Debug` shows no value.

use super::cookies::{CookieJar, StoredCookie};
use super::AccountStore;
use crate::db;
use anyhow::{anyhow, bail, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// The longest user agent kept from the sign-in window.
const MAX_USER_AGENT: usize = 512;

/// What a cookie store needs to make requests as the signed-in user.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Session {
    /// Battle.net: the store's cookies from the sign-in window.
    #[serde(rename_all = "camelCase")]
    Cookies {
        cookies: Vec<StoredCookie>,
        /// The sign-in window's user agent, sent with every request (stores tie sessions to it).
        user_agent: String,
        /// The account name the store reported, for the sign-in notice.
        #[serde(default)]
        account: Option<String>,
        saved_at: i64,
    },
    /// GOG: OAuth tokens (GOG Galaxy's public client).
    #[serde(rename_all = "camelCase")]
    Tokens {
        access: String,
        refresh: String,
        /// Unix ms.
        expires_at: i64,
        user_id: String,
        #[serde(default)]
        account: Option<String>,
        saved_at: i64,
    },
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cookies { cookies, .. } => {
                write!(
                    f,
                    "Session::Cookies({} cookies, values hidden)",
                    cookies.len()
                )
            }
            Self::Tokens { user_id, .. } => {
                write!(f, "Session::Tokens(user {user_id}, values hidden)")
            }
        }
    }
}

impl Session {
    pub fn cookies(
        jar: &CookieJar,
        user_agent: &str,
        account: Option<String>,
        saved_at: i64,
    ) -> Self {
        Self::Cookies {
            cookies: jar.cookies().to_vec(),
            user_agent: clean_user_agent(user_agent),
            account,
            saved_at,
        }
    }

    pub fn tokens(tokens: &super::gog::Tokens, account: Option<String>, saved_at: i64) -> Self {
        Self::Tokens {
            access: tokens.access.clone(),
            refresh: tokens.refresh.clone(),
            expires_at: tokens.expires_at,
            user_id: tokens.user_id.clone(),
            account,
            saved_at,
        }
    }

    /// The jar (filtered again to the store's domains) and the user agent (a token session has
    /// an empty jar and GameLobbies's own user agent).
    pub fn jar(&self, allowed: &'static [&'static str]) -> (CookieJar, String) {
        match self {
            Self::Cookies {
                cookies,
                user_agent,
                ..
            } => (
                CookieJar::new(allowed, cookies.clone()),
                clean_user_agent(user_agent),
            ),
            Self::Tokens { .. } => (CookieJar::default(), clean_user_agent("")),
        }
    }

    #[cfg(test)]
    pub fn account(&self) -> Option<&str> {
        match self {
            Self::Cookies { account, .. } | Self::Tokens { account, .. } => account.as_deref(),
        }
    }
}

/// Printable ASCII, at most [`MAX_USER_AGENT`] characters; otherwise GameLobbies's own.
pub fn clean_user_agent(raw: &str) -> String {
    let ok = !raw.trim().is_empty()
        && raw.len() <= MAX_USER_AGENT
        && raw.bytes().all(|b| (0x20..0x7f).contains(&b));
    if ok {
        raw.trim().to_string()
    } else {
        concat!("GameLobbies/", env!("CARGO_PKG_VERSION")).to_string()
    }
}

pub fn secret_name(store: AccountStore) -> String {
    format!("account-{}-session-dpapi", store.id())
}

pub fn exists(conn: &Connection, store: AccountStore) -> Result<bool> {
    Ok(db::read_secret(conn, &secret_name(store))?.is_some())
}

pub fn save(conn: &Connection, store: AccountStore, session: &Session) -> Result<()> {
    let plain = serde_json::to_vec(session)?;
    let sealed = crate::platform::protect(&plain).map_err(|_| {
        anyhow!(
            "Windows secure storage is unavailable; the {} sign-in was not saved.",
            store.name()
        )
    })?;
    db::write_secret(conn, &secret_name(store), &sealed)
}

pub fn load(conn: &Connection, store: AccountStore) -> Result<Option<Session>> {
    let Some(sealed) = db::read_secret(conn, &secret_name(store))? else {
        return Ok(None);
    };
    let plain = crate::platform::unprotect(&sealed).map_err(|_| {
        anyhow!(
            "Windows secure storage is unavailable; the {} sign-in can't be read.",
            store.name()
        )
    })?;
    match serde_json::from_slice(&plain) {
        Ok(session) => Ok(Some(session)),
        Err(_) => bail!(
            "The saved {} sign-in is unreadable; sign in again.",
            store.name()
        ),
    }
}

pub fn delete(conn: &Connection, store: AccountStore) -> Result<()> {
    db::delete_secret(conn, &secret_name(store))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOG: &[&str] = &["gog.com"];

    fn session() -> Session {
        let cookie = StoredCookie::from_webview(
            "gog-al",
            "SECRETVALUE123",
            ".gog.com",
            "/",
            true,
            true,
            None,
            GOG,
        )
        .unwrap();
        Session::cookies(
            &CookieJar::new(GOG, vec![cookie]),
            "Mozilla/5.0 Edg/130",
            Some("gamer".into()),
            5,
        )
    }

    #[test]
    fn sessions_are_sealed_per_store_and_never_shown() {
        let database = db::Db::memory().unwrap();
        let conn = database.lock();
        assert!(!exists(&conn, AccountStore::Gog).unwrap());
        save(&conn, AccountStore::Gog, &session()).unwrap();
        assert!(exists(&conn, AccountStore::Gog).unwrap());
        assert!(!exists(&conn, AccountStore::Battlenet).unwrap());
        // Token sessions are sealed the same way.
        let tokens = crate::accounts::gog::Tokens {
            access: "ACCESSVALUE0123456789".into(),
            refresh: "REFRESHVALUE0123456789".into(),
            expires_at: 9,
            user_id: "42".into(),
        };
        let t = Session::tokens(&tokens, Some("gamer".into()), 1);
        save(&conn, AccountStore::Battlenet, &t).unwrap();
        let raw = db::read_secret(&conn, "account-battlenet-session-dpapi")
            .unwrap()
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("ACCESSVALUE"));
        assert_eq!(load(&conn, AccountStore::Battlenet).unwrap().unwrap(), t);
        assert!(!format!("{t:?}").contains("ACCESSVALUE") && !format!("{t:?}").contains("REFRESH"));
        delete(&conn, AccountStore::Battlenet).unwrap();
        // Sealed: the stored bytes don't contain the value.
        let raw = db::read_secret(&conn, "account-gog-session-dpapi")
            .unwrap()
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("SECRETVALUE123"));
        let back = load(&conn, AccountStore::Gog).unwrap().unwrap();
        assert_eq!(back, session());
        assert_eq!(back.account(), Some("gamer"));
        let (jar, ua) = back.jar(GOG);
        assert_eq!(jar.cookies().len(), 1);
        assert_eq!(ua, "Mozilla/5.0 Edg/130");
        let shown = format!("{back:?}");
        assert!(!shown.contains("SECRETVALUE123") && shown.contains("values hidden"));
        assert!(!crate::redact::redact(&shown).contains("SECRETVALUE123"));
        delete(&conn, AccountStore::Gog).unwrap();
        assert!(load(&conn, AccountStore::Gog).unwrap().is_none());
        // The names are not retired names, and match the sign-out cleanup.
        assert_eq!(
            secret_name(AccountStore::Battlenet),
            "account-battlenet-session-dpapi"
        );
        assert!(!crate::secrets::RETIRED_SECRETS.contains(&"account-gog-session-dpapi"));
    }

    #[test]
    fn odd_user_agents_are_replaced() {
        assert_eq!(
            clean_user_agent("Mozilla/5.0 (Windows NT 10.0)"),
            "Mozilla/5.0 (Windows NT 10.0)"
        );
        for bad in ["", "x\ny", &"a".repeat(600), "naïve"] {
            assert!(clean_user_agent(bad).starts_with("GameLobbies/"), "{bad:?}");
        }
    }
}
