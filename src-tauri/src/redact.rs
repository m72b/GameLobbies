//! Defensive redaction for anything that leaves the app as text (the log file, the copied
//! diagnostics report).
//!
//! No code path is meant to format a secret into a log line; this is the second line of
//! defence. It removes `key=value` / `key: value` pairs whose name looks like a credential,
//! bearer tokens, long hex strings and long mixed letter/digit strings (API keys, access
//! tokens, base64 blobs such as DPAPI data), and replaces the user's profile folder with
//! `%USERPROFILE%`.

use regex::{Captures, Regex};
use std::sync::OnceLock;

pub const PROFILE_PLACEHOLDER: &str = "%USERPROFILE%";
const REDACTED: &str = "[redacted]";

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid redaction pattern"))
}

/// A credential-looking name followed by `=` or `:` and a value (JSON and URL query forms
/// included); the value is removed.
fn named_secret() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(
        &CELL,
        r#"(?i)\b((?:[a-z0-9]+[_-])*(?:api[_-]?key|apikey|secret|token|password|passwd|authorization|credential|dpapi|signature|signing[_-]?key|private[_-]?key)[a-z0-9_-]*)(["']?\s*[=:]\s*["']?)(?:bearer\s+)?[^\s"',&;)}\]]+"#,
    )
}

fn bearer() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}")
}

fn long_hex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, r"\b[0-9a-fA-F]{32,}\b")
}

/// A run of 24 or more base64/base64url characters.
fn long_token() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, r"[A-Za-z0-9_+=-]{24,}")
}

fn user_dir() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, r#"(?i)\b[a-z]:[\\/]+users[\\/]+[^\\/\s"'<>|:*?]+"#)
}

/// Replaces the profile folder `profile` (any letter case, `\` or `/`) and any other
/// `X:\Users\<name>` folder with `%USERPROFILE%`.
fn redact_paths(text: &str, profile: Option<&str>) -> String {
    let mut out = text.to_string();
    if let Some(profile) = profile.map(|p| p.trim_end_matches(['\\', '/'])) {
        if !profile.is_empty() {
            let pattern = profile
                .split(['\\', '/'])
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(r"[\\/]+");
            if let Ok(re) = Regex::new(&format!("(?i){pattern}")) {
                out = re.replace_all(&out, PROFILE_PLACEHOLDER).into_owned();
            }
        }
    }
    user_dir()
        .replace_all(&out, PROFILE_PLACEHOLDER)
        .into_owned()
}

/// Redacts one line or a whole report. `profile` is the user's profile folder.
pub fn redact_with(text: &str, profile: Option<&str>) -> String {
    let text =
        named_secret().replace_all(text, |c: &Captures| format!("{}{}{REDACTED}", &c[1], &c[2]));
    let text = bearer().replace_all(&text, |c: &Captures| format!("{} {REDACTED}", &c[1]));
    let text = long_hex().replace_all(&text, REDACTED);
    // Mixed letters and digits only: long plain words and paths stay readable.
    let text = long_token().replace_all(&text, |c: &Captures| {
        let run = &c[0];
        let has_digit = run.bytes().any(|b| b.is_ascii_digit());
        let has_letter = run.bytes().any(|b| b.is_ascii_alphabetic());
        if has_digit && has_letter {
            REDACTED.to_string()
        } else {
            run.to_string()
        }
    });
    redact_paths(&text, profile)
}

/// [`redact_with`] using this process's `USERPROFILE`.
pub fn redact(text: &str) -> String {
    redact_with(text, std::env::var("USERPROFILE").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX_KEY: &str = "0123456789abcdef0123456789abcdef";
    const TOKEN: &str = "abcdefghij0123456789abcdefghij";

    fn clean(text: &str) -> String {
        redact_with(text, Some(r"C:\Users\Alice"))
    }

    #[test]
    fn named_credentials_lose_their_values() {
        for line in [
            format!("api_key={HEX_KEY}"),
            format!("steamgriddb-api-key-dpapi: {TOKEN}"),
            format!(r#"{{"client_secret":"{TOKEN}","x":1}}"#),
            format!(
                "https://id.twitch.tv/oauth2/token?client_id=a1&client_secret={TOKEN}&grant_type=x"
            ),
            format!("Authorization: Bearer {TOKEN}"),
            format!("access_token = {TOKEN}"),
            "password=hunter2".to_string(),
        ] {
            let out = clean(&line);
            assert!(!out.contains(HEX_KEY), "{out}");
            assert!(!out.contains(TOKEN), "{out}");
            assert!(!out.contains("hunter2"), "{out}");
            assert!(out.contains(REDACTED), "{out}");
        }
        // Surrounding text survives.
        assert_eq!(
            clean("a=1 token=abc&b=2"),
            format!("a=1 token={REDACTED}&b=2")
        );
    }

    #[test]
    fn bare_keys_and_blobs_are_caught_by_shape() {
        let blob = "AQAAANCMnd8BFdERjHoAwE+Cl+sBAAAA0123456789abcdefghijklmnop==";
        for secret in [HEX_KEY, TOKEN, "Zm9vYmFyMDEyMzQ1Njc4OWFiY2RlZmdoaWo=", blob] {
            let out = clean(&format!("failed with {secret} here"));
            assert!(!out.contains(secret), "{out}");
        }
        assert!(clean(&format!("Bearer {TOKEN}")).contains(&format!("Bearer {REDACTED}")));
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for text in [
            "Scanned Steam: 42 games found.",
            "Could not scan Epic: The system cannot find the file specified. (os error 2)",
            "Using the development data folder D:\\lobby-dev",
            "version 0.3.0 on Windows 11 Pro (build 26200)",
            "a_very_long_identifier_without_digits_in_it",
        ] {
            assert_eq!(clean(text), text);
        }
    }

    #[test]
    fn the_user_profile_path_is_replaced() {
        assert_eq!(
            clean(r"opened C:\Users\Alice\AppData\Roaming\app.gamelobby.desktop\library.sqlite"),
            r"opened %USERPROFILE%\AppData\Roaming\app.gamelobby.desktop\library.sqlite"
        );
        assert_eq!(
            clean("c:/users/alice/Documents/x"),
            "%USERPROFILE%/Documents/x"
        );
        // Another account's folder is hidden too.
        assert_eq!(clean(r"D:\Users\Bob\Games"), r"%USERPROFILE%\Games");
        // Paths elsewhere stay.
        assert_eq!(clean(r"D:\Games\Steam"), r"D:\Games\Steam");
        // A profile from the environment with odd separators and case.
        assert_eq!(
            redact_with(r"x C:\USERS\carol\a", Some("c:/Users/Carol/")),
            r"x %USERPROFILE%\a"
        );
    }
}
