//! Launching games through their store routes, local executables and emulators.
//!
//! Emulator launches ([`build_emulator_argv`]) never build a command line string and never
//! use a shell: the profile's argument template is split into tokens first, placeholders are
//! then replaced inside those tokens, and every token becomes exactly one argv entry. A ROM
//! path containing spaces, quotes, `&`, `%` or `^` therefore can't add, split or inject
//! arguments, and only `.exe` emulators are started (Windows runs `.bat`/`.cmd` through
//! `cmd.exe`).

use crate::localscan::{classify_shortcut, ShortcutKind};
use crate::platform::{on_thread, resolve, shell_open, spawn_detached, spawn_detached_in};
use crate::state::AppState;
use crate::types::{non_empty, now_ms, EmulatorProfile, Game, GameRoute};
use crate::{db, emulators, local};
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const LAUNCH_SCHEMES: [&str; 9] = [
    "steam",
    "com.epicgames.launcher",
    "goggalaxy",
    "eaapp",
    "origin2",
    "battlenet",
    "uplay",
    "xbox",
    "ms-xbox-gamepass",
];

/// At most this many tokens in an emulator argument template.
pub const MAX_TEMPLATE_TOKENS: usize = 128;

/// Store launch links are limited to known store schemes without credentials or ports.
pub fn validate_launch_uri(raw: &str) -> Result<String> {
    let url = url::Url::parse(raw).map_err(|_| anyhow!("Launch link is not a valid URL."))?;
    if !LAUNCH_SCHEMES.contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        bail!("Launch links using {}: are not allowed.", url.scheme());
    }
    Ok(url.to_string())
}

/// Accepts a JSON string array, or (for older custom routes) whitespace-separated text.
pub fn parse_launch_arguments(value: Option<&str>) -> Vec<String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Vec::new();
    };
    if let Ok(args) = serde_json::from_str::<Vec<String>>(value) {
        return args;
    }
    value.split_whitespace().map(str::to_owned).collect()
}

/// Splits an emulator argument template into tokens.
///
/// - Unquoted whitespace separates tokens.
/// - `"..."` and `'...'` group text (the quotes are removed; the other quote character is
///   literal inside). Quoted and unquoted parts next to each other form one token:
///   `--path="C:\My Games"` → `--path=C:\My Games`. `""` is an empty token.
/// - Backslashes are always literal (Windows paths); there are no escapes.
/// - An unterminated quote or more than [`MAX_TEMPLATE_TOKENS`] tokens is an error.
pub fn tokenize_template(template: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    for c in template.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_token = true;
            }
            None if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            None => {
                current.push(c);
                in_token = true;
            }
        }
    }
    if quote.is_some() {
        bail!("The emulator arguments have an unclosed quote.");
    }
    if in_token {
        tokens.push(current);
    }
    if tokens.len() > MAX_TEMPLATE_TOKENS {
        bail!("The emulator arguments have too many parts (at most {MAX_TEMPLATE_TOKENS}).");
    }
    Ok(tokens)
}

/// Replaces `{rom}`, `{romdir}` and `{romname}` in one token in a single pass (a value that
/// itself contains `{rom}` is never substituted again). Other braces are left as they are.
fn substitute(token: &str, rom: &str, romdir: &str, romname: &str) -> (String, bool) {
    let mut out = String::with_capacity(token.len() + rom.len());
    let mut used_rom = false;
    let mut rest = token;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let (value, len) = if tail.starts_with("{romdir}") {
            (romdir, "{romdir}".len())
        } else if tail.starts_with("{romname}") {
            (romname, "{romname}".len())
        } else if tail.starts_with("{rom}") {
            used_rom = true;
            (rom, "{rom}".len())
        } else {
            ("{", 1)
        };
        out.push_str(value);
        rest = &tail[len..];
    }
    out.push_str(rest);
    (out, used_rom)
}

/// The argv for an emulator launch (program excluded).
///
/// The template is tokenized ([`tokenize_template`]) before anything is substituted, and
/// each token becomes one argument:
/// - `{rom}` → the ROM path, `{romdir}` → its folder, `{romname}` → its file name without
///   the extension. All are inserted literally (no quoting, no escaping needed).
/// - A placeholder may also sit inside a larger token (`--path={rom}`,
///   `"--rom={rom}"`): the result is still one argument, `--path=<path>`. This is deliberate:
///   some emulators need `key=value` arguments, and argument boundaries come only from the
///   template, never from the path.
/// - When no token uses `{rom}`, the ROM path is appended as the last argument.
/// - A token that is exactly `{romname}` is refused when the file name starts with `-` or
///   `/`: as a whole argument it would be read as an option. (`{rom}` and `{romdir}` are
///   absolute paths and can't start that way.)
pub fn build_emulator_argv(template: &str, rom: &Path) -> Result<Vec<String>> {
    let rom_text = rom.to_string_lossy();
    let romdir = rom
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let romname = rom
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut used_rom = false;
    let mut argv = Vec::new();
    for token in tokenize_template(template)? {
        if token == "{romname}" && (romname.starts_with('-') || romname.starts_with('/')) {
            bail!("The game file name \"{romname}\" starts with \"-\" or \"/\", so it can't be passed on its own as {{romname}}. Rename the file or change the emulator arguments.");
        }
        let (arg, used) = substitute(&token, &rom_text, &romdir, &romname);
        used_rom |= used;
        argv.push(arg);
    }
    if !used_rom {
        argv.push(rom_text.into_owned());
    }
    Ok(argv)
}

/// The profile a ROM route launches with: its assigned profile, or (when none is assigned or
/// the assigned one was deleted) the default for its system.
pub fn emulator_for_route(
    profiles: &[EmulatorProfile],
    route: &GameRoute,
) -> Result<EmulatorProfile> {
    if let Some(profile) = route
        .emulator_id
        .as_deref()
        .and_then(|id| profiles.iter().find(|p| p.id == id))
    {
        return Ok(profile.clone());
    }
    route
        .system
        .as_deref()
        .and_then(|s| emulators::default_for_system(profiles, s))
        .cloned()
        .ok_or_else(|| {
            anyhow!("No emulator is set up for this game. Add an emulator folder or choose an emulator for it.")
        })
}

/// Everything an emulator launch needs, checked: (emulator exe, argv, working folder).
pub fn prepare_emulator_launch(
    profile: &EmulatorProfile,
    route: &GameRoute,
) -> Result<(PathBuf, Vec<String>, PathBuf)> {
    if !profile.enabled {
        bail!(
            "{} is disabled. Enable it in Settings or choose another emulator.",
            profile.name
        );
    }
    if emulators::needs_configuration(profile) {
        bail!(
            "{} needs to be configured first: edit its arguments (replace CORE_NAME with a core).",
            profile.name
        );
    }
    let exe = Path::new(&profile.executable);
    if !exe.is_absolute() || !profile.executable.to_lowercase().ends_with(".exe") {
        bail!(
            "{}'s executable must be a full path to an .exe file.",
            profile.name
        );
    }
    if !exe.is_file() {
        bail!(
            "{}'s executable was not found: {}",
            profile.name,
            profile.executable
        );
    }
    let rom = Path::new(&route.launch_target);
    if !rom.is_absolute() || crate::tracker::normalize_path(&route.launch_target).is_none() {
        bail!("The game file path is not a full path.");
    }
    if !rom.exists() {
        bail!("The game file was not found: {}", route.launch_target);
    }
    let argv = build_emulator_argv(&profile.args, rom)?;
    let cwd = exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("{}'s folder is invalid.", profile.name))?;
    Ok((exe.to_path_buf(), argv, cwd))
}

/// How a `local` route starts.
#[derive(Debug, PartialEq)]
pub enum LocalLaunch {
    /// An `.exe`, started directly in its folder.
    Exe(PathBuf),
    /// A `.lnk` the user confirmed (still pointing at what they confirmed), opened by the shell.
    Shortcut(PathBuf),
    /// A validated store link.
    Uri(String),
}

/// Checks a `local` route's target right before launching: a store link must pass
/// [`validate_launch_uri`]; an `.exe` must be an absolute existing file; a `.lnk` must
/// still be a plain `.exe` shortcut (then that `.exe` is started) or match exactly what the
/// user confirmed. Anything else is refused. Reads shortcuts (COM): call it off the async
/// runtime threads.
pub fn local_launch(
    target: &str,
    confirmations: &HashMap<String, (String, String)>,
) -> Result<LocalLaunch> {
    if target.contains("://") {
        return Ok(LocalLaunch::Uri(validate_launch_uri(target)?));
    }
    let lower = target.to_lowercase();
    if lower.ends_with(".exe") {
        if !emulators::executable_ok(target) {
            bail!("The game's executable was not found: {target}");
        }
        return Ok(LocalLaunch::Exe(PathBuf::from(target)));
    }
    if lower.ends_with(".lnk") {
        let path = Path::new(target);
        return match classify_shortcut(path, confirmations) {
            ShortcutKind::Exe(exe) => Ok(LocalLaunch::Exe(exe)),
            ShortcutKind::Confirmed => Ok(LocalLaunch::Shortcut(path.to_path_buf())),
            ShortcutKind::NeedsConfirmation { target, arguments } => bail!(
                "This shortcut changed since you confirmed it (it now runs {target} {arguments}). Rescan and confirm it again in Review."
            ),
            ShortcutKind::Invalid(reason) => bail!("{reason}"),
            ShortcutKind::Unreadable => bail!("The shortcut was not found or can't be read."),
            ShortcutKind::Uri(_) => bail!("This shortcut can't be started."),
        };
    }
    bail!("This game can only start an .exe, a confirmed shortcut or a store link. Add it again with \"Add local game\".")
}

fn is_rom_route(route: &GameRoute) -> bool {
    route.source_id == local::ROM_SOURCE || route.emulator_id.is_some()
}

/// Starts a local PC executable in its own folder. Falls back to the shell when Windows
/// requires elevation (error 740), so games that need administrator rights still start.
async fn launch_local_exe(path: &Path) -> Result<()> {
    match spawn_detached_in(path, &[], path.parent()) {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(740) => {
            shell_open(path.to_string_lossy().into_owned()).await
        }
        Err(e) => Err(anyhow!("Could not start {}: {e}", path.display())),
    }
}

pub async fn launch_game(app: &AppHandle, game: &Game, route_index: usize) -> Result<()> {
    let route = game
        .routes
        .get(route_index)
        .ok_or_else(|| anyhow!("No launch route is configured."))?;
    let state = app.state::<AppState>();
    if is_rom_route(route) {
        let (exe, argv, cwd) = {
            let raw = db::emulator_profiles(&state.db.lock())?;
            // Availability is checked after the database lock is released.
            let profiles = emulators::refresh_available(raw);
            let profile = emulator_for_route(&profiles, route)?;
            prepare_emulator_launch(&profile, route)?
        };
        let pid = spawn_detached_in(&exe, &argv, Some(&cwd))
            .map_err(|e| anyhow!("Could not start {}: {e}", exe.display()))?;
        let settings = db::get_settings(&state.db.lock())?;
        let watching =
            state
                .tracker
                .watch_process(&game.id, pid, &exe.to_string_lossy(), &settings);
        if !watching {
            db::mark_game_played(&state.db.lock(), &game.id, now_ms())?;
        }
        return Ok(());
    }
    let target = route.launch_target.as_str();
    if route.source_id == local::LOCAL_SOURCE {
        let confirmations = db::shortcut_confirmations(&state.db.lock())?;
        let owned = target.to_string();
        let how = on_thread(move || local_launch(&owned, &confirmations)).await??;
        match how {
            LocalLaunch::Exe(exe) => launch_local_exe(&exe).await?,
            LocalLaunch::Shortcut(lnk) => shell_open(lnk.to_string_lossy().into_owned()).await?,
            LocalLaunch::Uri(uri) => shell_open(uri).await?,
        }
    } else if target.contains("://") {
        shell_open(validate_launch_uri(target)?).await?;
    } else if target.starts_with("shell:AppsFolder\\") {
        let windows_dir = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        spawn_detached(&windows_dir.join("explorer.exe"), &[target.to_string()])?;
    } else if let Some(args) = non_empty(&route.launch_args) {
        let executable = resolve(target);
        if !executable.exists() {
            bail!("Launch target was not found.");
        }
        if route.source_id == "epic" {
            let configured = db::get_connection_values(&state.db.lock())?.legendary_path;
            if configured.is_empty()
                || !executable
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&resolve(&configured).to_string_lossy())
            {
                bail!("The configured Legendary executable changed. Update it in Library settings and scan again.");
            }
        }
        spawn_detached(&executable, &parse_launch_arguments(Some(args)))?;
    } else {
        let path = resolve(target);
        if !path.exists() {
            bail!("Launch target was not found.");
        }
        shell_open(path.to_string_lossy().into_owned()).await?;
    }
    if route.installed != Some(false) {
        let settings = db::get_settings(&state.db.lock())?;
        let watching = state.tracker.watch(&state.db, &game.id, &settings);
        if !watching {
            db::mark_game_played(&state.db.lock(), &game.id, now_ms())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localscan::tests::TempDir;

    #[test]
    fn launch_links_are_limited_to_store_schemes() {
        assert_eq!(
            validate_launch_uri("steam://run/570").unwrap(),
            "steam://run/570"
        );
        assert_eq!(
            validate_launch_uri("com.epicgames.launcher://apps/example?action=launch").unwrap(),
            "com.epicgames.launcher://apps/example?action=launch"
        );
        assert!(validate_launch_uri("javascript://alert(1)").is_err());
        assert!(validate_launch_uri("https://example.com").is_err());
        assert!(validate_launch_uri("steam://user:pass@host/run/570").is_err());
        assert!(validate_launch_uri("steam://host:99/run/570").is_err());
    }

    #[test]
    fn parses_json_or_plain_arguments() {
        assert_eq!(
            parse_launch_arguments(Some(r#"["launch","My Game"]"#)),
            ["launch", "My Game"]
        );
        assert_eq!(parse_launch_arguments(Some("-a  -b")), ["-a", "-b"]);
        assert!(parse_launch_arguments(None).is_empty());
    }

    #[test]
    fn templates_tokenize_with_quotes_and_literal_backslashes() {
        assert_eq!(
            tokenize_template("-b -e {rom}").unwrap(),
            ["-b", "-e", "{rom}"]
        );
        assert_eq!(
            tokenize_template(r#"-L "cores\snes 9x.dll"  {rom}"#).unwrap(),
            ["-L", r"cores\snes 9x.dll", "{rom}"]
        );
        assert_eq!(
            tokenize_template(r#"--path="C:\My Games" 'it"s' "" x"#).unwrap(),
            [r"--path=C:\My Games", "it\"s", "", "x"]
        );
        assert!(tokenize_template("").unwrap().is_empty());
        assert!(tokenize_template("\"open").is_err());
        assert!(tokenize_template("'open").is_err());
        assert!(tokenize_template(&"a ".repeat(MAX_TEMPLATE_TOKENS + 1)).is_err());
    }

    #[test]
    fn rom_paths_are_whole_arguments_whatever_they_contain() {
        for rom in [
            r"D:\Roms\Simple.iso",
            r"D:\My Roms\Game (USA) [!].iso",
            r#"D:\Roms\Quote"d & more.iso"#,
            r"D:\Roms\100% & ^done %PATH%.iso",
            r"D:\Roms\a & calc.exe & b.iso",
            r#"D:\Roms\" --evil "x.iso"#,
            r"D:\Roms\{rom}\{romdir}.iso",
        ] {
            let path = Path::new(rom);
            let argv = build_emulator_argv("-b -e {rom}", path).unwrap();
            assert_eq!(argv, ["-b", "-e", rom], "{rom}");
            // No {rom}: appended last, still one argument.
            let argv = build_emulator_argv("--fullscreen", path).unwrap();
            assert_eq!(argv, ["--fullscreen", rom]);
            // Inside a larger token: one argument with the path inside.
            let argv = build_emulator_argv("--path={rom} -x", path).unwrap();
            assert_eq!(argv, [format!("--path={rom}"), "-x".into()]);
        }
        let path = Path::new(r"D:\Roms\PS1\Final Fantasy VII.m3u");
        assert_eq!(
            build_emulator_argv(r#"--dir "{romdir}" --name={romname} "{rom}""#, path).unwrap(),
            [
                "--dir",
                r"D:\Roms\PS1",
                "--name=Final Fantasy VII",
                r"D:\Roms\PS1\Final Fantasy VII.m3u"
            ]
        );
        // {romdir} alone doesn't count as the ROM: it is still appended.
        assert_eq!(
            build_emulator_argv("{romdir}", path).unwrap(),
            [r"D:\Roms\PS1", r"D:\Roms\PS1\Final Fantasy VII.m3u"]
        );
        // A bare {romname} that looks like an option is refused; inside a token it's fine.
        let dashed = Path::new(r"D:\Roms\GBA\-fullscreen.gba");
        assert!(build_emulator_argv("{romname}", dashed).is_err());
        assert!(build_emulator_argv("{romname}", Path::new(r"D:\Roms\GBA\-x.gba")).is_err());
        assert_eq!(
            build_emulator_argv("--name={romname}", dashed).unwrap(),
            ["--name=-fullscreen", r"D:\Roms\GBA\-fullscreen.gba"]
        );
        assert_eq!(
            build_emulator_argv("{rom}", dashed).unwrap(),
            [r"D:\Roms\GBA\-fullscreen.gba"]
        );
        // Unknown braces stay literal.
        assert_eq!(
            build_emulator_argv("{core} {rom}", path).unwrap()[0],
            "{core}"
        );
        assert!(build_emulator_argv("\"{rom}", path).is_err());
        assert_eq!(
            build_emulator_argv("", path).unwrap(),
            [path.to_string_lossy()]
        );
    }

    fn profile(exe: &Path) -> EmulatorProfile {
        let mut p = emulators::new_profile(exe, crate::catalog::known_emulator("mgba").unwrap());
        p.id = "mgba".into();
        emulators::with_status(p)
    }

    fn rom_route(rom: &Path, emulator: Option<&str>) -> GameRoute {
        GameRoute {
            source_id: "rom".into(),
            store: "Emulator".into(),
            launch_target: rom.to_string_lossy().into(),
            system: Some("gba".into()),
            emulator_id: emulator.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn emulator_launches_check_the_profile_and_files() {
        let dir = TempDir::new("launch");
        let exe = dir.file("mGBA/mGBA.exe");
        let rom = dir.file("Roms/GBA/Game & Watch 100% ^x.gba");
        let good = profile(&exe);
        let (program, argv, cwd) = prepare_emulator_launch(&good, &rom_route(&rom, None)).unwrap();
        assert_eq!(program, exe);
        assert_eq!(argv, [rom.to_string_lossy()]);
        assert_eq!(cwd, exe.parent().unwrap());

        let disabled = EmulatorProfile {
            enabled: false,
            ..good.clone()
        };
        let err = prepare_emulator_launch(&disabled, &rom_route(&rom, None)).unwrap_err();
        assert!(err.to_string().contains("disabled"), "{err}");

        let missing = EmulatorProfile {
            executable: dir.path().join("gone/mGBA.exe").to_string_lossy().into(),
            ..good.clone()
        };
        let err = prepare_emulator_launch(&missing, &rom_route(&rom, None)).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");

        let bat = dir.file("mGBA/run.bat");
        let batch = EmulatorProfile {
            executable: bat.to_string_lossy().into(),
            ..good.clone()
        };
        assert!(prepare_emulator_launch(&batch, &rom_route(&rom, None)).is_err());
        let relative = EmulatorProfile {
            executable: "mGBA.exe".into(),
            ..good.clone()
        };
        assert!(prepare_emulator_launch(&relative, &rom_route(&rom, None)).is_err());

        let mut retro =
            emulators::new_profile(&exe, crate::catalog::known_emulator("retroarch").unwrap());
        retro = emulators::with_status(retro);
        let err = prepare_emulator_launch(&retro, &rom_route(&rom, None)).unwrap_err();
        assert!(err.to_string().contains("configured"), "{err}");

        let gone_rom = dir.path().join("Roms/GBA/missing.gba");
        assert!(prepare_emulator_launch(&good, &rom_route(&gone_rom, None)).is_err());
        assert!(prepare_emulator_launch(&good, &rom_route(Path::new("rel.gba"), None)).is_err());
        let bad_args = EmulatorProfile {
            args: "\"{rom}".into(),
            ..good.clone()
        };
        assert!(prepare_emulator_launch(&bad_args, &rom_route(&rom, None)).is_err());
    }

    #[test]
    fn routes_pick_their_assigned_or_default_emulator() {
        let dir = TempDir::new("launch-pick");
        let exe = dir.file("mGBA/mGBA.exe");
        let rom = dir.file("GBA/x.gba");
        let profiles = vec![profile(&exe)];
        assert_eq!(
            emulator_for_route(&profiles, &rom_route(&rom, None))
                .unwrap()
                .id,
            "mgba"
        );
        assert_eq!(
            emulator_for_route(&profiles, &rom_route(&rom, Some("mgba")))
                .unwrap()
                .id,
            "mgba"
        );
        // A deleted assignment falls back to the default.
        assert_eq!(
            emulator_for_route(&profiles, &rom_route(&rom, Some("deleted")))
                .unwrap()
                .id,
            "mgba"
        );
        let mut no_system = rom_route(&rom, None);
        no_system.system = None;
        assert!(emulator_for_route(&profiles, &no_system).is_err());
        let mut ps3 = rom_route(&rom, None);
        ps3.system = Some("ps3".into());
        assert!(emulator_for_route(&profiles, &ps3).is_err());
        // A disabled assigned profile is returned (and refused by prepare_emulator_launch).
        let mut disabled = profiles.clone();
        disabled[0].enabled = false;
        let chosen = emulator_for_route(&disabled, &rom_route(&rom, Some("mgba"))).unwrap();
        assert!(prepare_emulator_launch(&chosen, &rom_route(&rom, None)).is_err());
    }
    #[test]
    fn local_routes_launch_only_exes_store_links_and_confirmed_shortcuts() {
        let dir = TempDir::new("launch-local");
        let exe = dir.file("Game/game.exe");
        let none = HashMap::new();
        assert_eq!(
            local_launch(&exe.to_string_lossy(), &none).unwrap(),
            LocalLaunch::Exe(exe.clone())
        );
        assert_eq!(
            local_launch("steam://rungameid/1", &none).unwrap(),
            LocalLaunch::Uri("steam://rungameid/1".into())
        );
        for bad in [
            "https://evil.example/x.exe".to_string(),
            "file:///C:/Windows/System32/calc.exe".to_string(),
            dir.file("Game/run.bat").to_string_lossy().into_owned(),
            dir.file("Game/link.url").to_string_lossy().into_owned(),
            dir.path()
                .join("Game/missing.exe")
                .to_string_lossy()
                .into_owned(),
            "game.exe".to_string(),
        ] {
            assert!(local_launch(&bad, &none).is_err(), "{bad}");
        }
        #[cfg(windows)]
        {
            use crate::localscan::key_of;
            use crate::platform::create_shortcut;
            let plain = dir.path().join("Plain.lnk");
            create_shortcut(&plain, &exe.to_string_lossy(), "").unwrap();
            assert!(matches!(
                local_launch(&plain.to_string_lossy(), &none).unwrap(),
                LocalLaunch::Exe(_)
            ));
            let args = dir.path().join("Args.lnk");
            create_shortcut(&args, &exe.to_string_lossy(), "-x").unwrap();
            assert!(
                local_launch(&args.to_string_lossy(), &none).is_err(),
                "not confirmed"
            );
            let (target, arguments) = crate::platform::resolve_shortcut(&args).unwrap();
            let confirmed: HashMap<String, (String, String)> =
                [(key_of(&args), (target, arguments))].into();
            assert_eq!(
                local_launch(&args.to_string_lossy(), &confirmed).unwrap(),
                LocalLaunch::Shortcut(args.clone())
            );
            // Edited after confirming: refused.
            create_shortcut(&args, &exe.to_string_lossy(), "-y").unwrap();
            assert!(local_launch(&args.to_string_lossy(), &confirmed).is_err());
        }
    }
}
