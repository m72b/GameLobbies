//! "Launch on boot": a per-user Windows Run entry (HKCU only, never HKLM, so it needs no
//! administrator rights and the installer's uninstaller, which removes the same value, cleans it up).
//!
//! The entry runs `"<this exe>" --autostart`. The setting is the source of truth: at startup and
//! on every settings save the real registry value is read and made to match it (so an update that
//! moved the exe gets a corrected path, and an entry written by an older version is repaired).

use std::path::Path;

/// The Run value's name. The NSIS uninstaller deletes the value with this name (the product name).
pub const VALUE_NAME: &str = "Game Lobby";
/// Added to the command line of a start made by Windows at sign-in: start minimized.
pub const FLAG: &str = "--autostart";

/// The command line stored in the Run entry: the quoted exe path and [`FLAG`].
pub fn run_value(exe: &Path) -> String {
    let path = exe.to_string_lossy();
    // `current_exe` can return the verbatim form on Windows; the Run key wants the plain path.
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    format!("\"{path}\" {FLAG}")
}

/// Whether this process was started by the Run entry.
pub fn started_by_autostart<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == FLAG)
}

/// What to do with the Run value so that it matches the setting.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Already as wanted.
    Keep,
    /// Write this command line (missing, or pointing somewhere else).
    Write(String),
    /// Delete the value.
    Remove,
}

/// Compares the setting with the value found in the registry (`None` = no value).
pub fn plan(enabled: bool, current: Option<&str>, exe: &Path) -> Plan {
    match (enabled, current) {
        (true, current) => {
            let wanted = run_value(exe);
            if current == Some(wanted.as_str()) {
                Plan::Keep
            } else {
                Plan::Write(wanted)
            }
        }
        (false, Some(_)) => Plan::Remove,
        (false, None) => Plan::Keep,
    }
}

/// Makes the registry match `enabled` for the running exe. Logs and returns the error text when
/// the registry could not be changed; the setting itself is kept either way.
pub fn reconcile(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Could not find the app path: {e}"))?;
    let current = registry::read().map_err(|e| format!("Could not read the startup entry: {e}"))?;
    match plan(enabled, current.as_deref(), &exe) {
        Plan::Keep => Ok(()),
        Plan::Write(value) => {
            log::info!("Launch on boot: writing the startup entry.");
            registry::write(&value).map_err(|e| format!("Could not write the startup entry: {e}"))
        }
        Plan::Remove => {
            log::info!("Launch on boot: removing the startup entry.");
            registry::remove().map_err(|e| format!("Could not remove the startup entry: {e}"))
        }
    }
}

#[cfg(windows)]
mod registry {
    use super::VALUE_NAME;
    use std::io;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn read() -> io::Result<Option<String>> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = match hkcu.open_subkey_with_flags(RUN_KEY, KEY_READ) {
            Ok(key) => key,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match key.get_value::<String, _>(VALUE_NAME) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn write(value: &str) -> io::Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY)?;
        key.set_value(VALUE_NAME, &value)
    }

    pub fn remove() -> io::Result<()> {
        let key = match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_SET_VALUE)
        {
            Ok(key) => key,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match key.delete_value(VALUE_NAME) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(not(windows))]
mod registry {
    use std::io;

    pub fn read() -> io::Result<Option<String>> {
        Ok(None)
    }
    pub fn write(_: &str) -> io::Result<()> {
        Ok(())
    }
    pub fn remove() -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_run_value_quotes_the_exe_path_and_adds_the_flag() {
        let exe = PathBuf::from(r"C:\Users\Some One\AppData\Local\Game Lobby\game-lobby.exe");
        assert_eq!(
            run_value(&exe),
            r#""C:\Users\Some One\AppData\Local\Game Lobby\game-lobby.exe" --autostart"#
        );
        // The verbatim prefix is dropped.
        assert_eq!(
            run_value(Path::new(r"\\?\C:\Apps\gl.exe")),
            r#""C:\Apps\gl.exe" --autostart"#
        );
        assert_eq!(VALUE_NAME, "Game Lobby");
    }

    #[test]
    fn reconcile_plan_follows_the_setting() {
        let exe = Path::new(r"C:\Apps\gl.exe");
        let good = run_value(exe);
        // On: write when missing or stale, keep when exact.
        assert_eq!(plan(true, None, exe), Plan::Write(good.clone()));
        assert_eq!(plan(true, Some(&good), exe), Plan::Keep);
        assert_eq!(
            plan(true, Some(r#""C:\Old\gl.exe" --autostart"#), exe),
            Plan::Write(good.clone())
        );
        assert_eq!(
            plan(true, Some(r"C:\Apps\gl.exe"), exe),
            Plan::Write(good.clone())
        );
        // Off: remove whatever is there, do nothing when nothing is.
        assert_eq!(plan(false, Some(&good), exe), Plan::Remove);
        assert_eq!(plan(false, Some("something else"), exe), Plan::Remove);
        assert_eq!(plan(false, None, exe), Plan::Keep);
    }

    #[test]
    fn the_autostart_flag_is_recognised_only_as_a_whole_argument() {
        assert!(started_by_autostart(["game-lobby.exe", "--autostart"]));
        assert!(!started_by_autostart(["game-lobby.exe"]));
        assert!(!started_by_autostart(["game-lobby.exe", "--autostarted"]));
        assert!(!started_by_autostart(Vec::<String>::new()));
    }

    #[test]
    fn only_the_current_user_hive_is_used() {
        let source = include_str!("autostart.rs");
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("HKEY_CURRENT_USER"));
        assert!(!code.contains("HKEY_LOCAL_MACHINE"));
    }
}
