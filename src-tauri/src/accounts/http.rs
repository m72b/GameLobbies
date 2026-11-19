//! Shared pieces for talking to store web services: fixed-text errors, size-capped bodies and
//! title cleaning. Moved here unchanged from `steam_owned`, which still uses them the same way.
//!
//! Errors never carry a response body, a URL or `reqwest`'s own error text (it can contain the
//! request URL, and with it a key or a code).

/// Largest response body read by default. A large Steam library is about 150 bytes per game;
/// this allows far more than any real account.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Longest title kept from a store response.
pub const MAX_NAME_CHARS: usize = 200;

/// Why a request to a store failed. Every message is fixed text.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// HTTP 401/403: wrong or revoked credentials.
    Rejected,
    /// The service answered without the data (for Steam: private game details).
    Private,
    RateLimited,
    Network(&'static str),
    Status(u16),
    Invalid(&'static str),
}

impl FetchError {
    /// The message for `service` (a store's display name). Services with their own wording
    /// for [`FetchError::Rejected`] and [`FetchError::Private`] map those themselves (see
    /// `steam_owned`).
    pub fn message_for(&self, service: &str) -> String {
        match self {
            Self::Rejected => format!("{service} did not accept the saved credentials."),
            Self::Private => format!("{service} did not share the list of games."),
            Self::RateLimited => {
                format!("{service} is limiting requests. Game Lobby tries again later.")
            }
            Self::Network(what) => format!("Could not reach {service}: {what}"),
            Self::Status(code) => {
                format!("{service} returned an error (HTTP {code}). Game Lobby tries again later.")
            }
            Self::Invalid(what) => format!("{service} returned an unreadable response: {what}"),
        }
    }

    /// Whether only new credentials can fix it (no automatic retry).
    pub fn needs_new_credentials(&self) -> bool {
        matches!(self, Self::Rejected)
    }
}

/// A transport failure as fixed text. The error's own text is never used: it can carry the
/// request URL (with a key or a code in it).
pub fn network_error(e: &reqwest::Error) -> FetchError {
    FetchError::Network(if e.is_timeout() {
        "the request timed out."
    } else if e.is_connect() {
        "no connection. Check that this PC is online."
    } else {
        "the connection failed."
    })
}

/// The body, refused once it passes [`MAX_RESPONSE_BYTES`].
pub async fn read_capped(response: reqwest::Response) -> Result<Vec<u8>, FetchError> {
    read_capped_to(response, MAX_RESPONSE_BYTES).await
}

/// The body, refused once it passes `max` bytes (checked against `Content-Length` first and
/// again while reading, so a server that lies about the length is cut off too).
pub async fn read_capped_to(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, FetchError> {
    if response
        .content_length()
        .is_some_and(|len| len as usize > max)
    {
        return Err(FetchError::Invalid("the response is too large."));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| network_error(&e))? {
        if body.len() + chunk.len() > max {
            return Err(FetchError::Invalid("the response is too large."));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Plain one-line text: control and bidi characters dropped, whitespace collapsed, capped at
/// [`MAX_NAME_CHARS`].
pub fn clean_name(raw: &str) -> Option<String> {
    let text = crate::validation::plain_text(raw);
    let text: String = text.chars().take(MAX_NAME_CHARS).collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_fixed_text_naming_the_service() {
        assert_eq!(
            FetchError::RateLimited.message_for("Steam"),
            "Steam is limiting requests. Game Lobby tries again later."
        );
        assert_eq!(
            FetchError::Status(502).message_for("GOG"),
            "GOG returned an error (HTTP 502). Game Lobby tries again later."
        );
        assert_eq!(
            FetchError::Network("x").message_for("Steam"),
            "Could not reach Steam: x"
        );
        assert!(FetchError::Rejected.needs_new_credentials());
        assert!(!FetchError::Private.needs_new_credentials());
    }

    #[test]
    fn names_are_cleaned_and_capped() {
        assert_eq!(clean_name("  A \n B\u{202e}\u{7} "), Some("A B".into()));
        assert_eq!(clean_name(" \t"), None);
        assert_eq!(clean_name(&"x".repeat(500)).unwrap().len(), MAX_NAME_CHARS);
    }
}
