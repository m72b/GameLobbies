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
use crate::{db, emulators, launchers, local};
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

/// Store launch links are limited to known store schemes in `<scheme>://<host>...` form,
/// without credentials, ports, backslashes or control characters.
///
/// The `://` form is required: an opaque link such as `xbox:x\..\..\Windows\calc.exe` parses
/// as a URL, but Windows would read the same text as a relative file path.
pub fn validate_launch_uri(raw: &str) -> Result<String> {
    if raw.chars().any(|c| c.is_control() || c == '\\') {
        bail!("Launch links can't contain backslashes or control characters.");
    }
    let url = url::Url::parse(raw).map_err(|_| anyhow!("Launch link is not a valid URL."))?;
    let scheme = url.scheme();
    if !LAUNCH_SCHEMES.contains(&scheme) {
        bail!("Launch links using {scheme}: are not allowed.");
    }
    let prefix = format!("{scheme}://");
    if url.cannot_be_a_base()
        || url.host_str().is_none_or(str::is_empty)
        || !raw
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(&prefix))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        bail!("Launch links must look like {scheme}://... with no user name or port.");
    }
    Ok(url.to_string())
}

/// A target that isn't a valid launch link must be an absolute drive path (`C:\Games\x.exe`)
/// before it is resolved or started. `link` is the target's link check, used as the error
/// for link-like text.
fn require_drive_path(target: &str, link: Result<String>) -> Result<()> {
    if emulators::drive_path_ok(target) {
        return Ok(());
    }
    match link {
        Err(e) if target.contains(':') => Err(e),
        _ => bail!("The launch target is not a full path or a supported store link."),
    }
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
    tokenize_arguments(template, "emulator")
}

/// [`tokenize_template`] for any program's arguments; `noun` names the program in messages.
pub(crate) fn tokenize_arguments(template: &str, noun: &str) -> Result<Vec<String>> {
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
        bail!("The {noun} arguments have an unclosed quote.");
    }
    if in_token {
        tokens.push(current);
    }
    if tokens.len() > MAX_TEMPLATE_TOKENS {
        bail!("The {noun} arguments have too many parts (at most {MAX_TEMPLATE_TOKENS}).");
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

const PLACEHOLDERS: [&str; 3] = ["{rom}", "{romdir}", "{romname}"];
/// Characters a shell would treat specially. No shell is ever used, but a template that needs
/// them is almost certainly an attempt to run something else, so it is refused.
const SHELL_CHARACTERS: [char; 7] = ['&', '|', '<', '>', '^', '%', '`'];

/// Option names (without their `--`, `-` or `/`, lower case) that make a Chromium/Electron or
/// Node based program (Heroic, Playnite add-ons, many launchers) run another program, load code
/// or accept remote control. A name that starts with one of these is refused, as is any
/// `*cmd-prefix` option. No emulator in the catalog needs them.
const DEBUGGING_OPTIONS: [&str; 16] = [
    "inspect",
    "remote-debugging",
    "gpu-launcher",
    "no-sandbox",
    "load-extension",
    "disable-web-security",
    "renderer-cmd-prefix",
    "js-flags",
    "browser-subprocess-path",
    "proxy-server",
    "proxy-pac-url",
    "host-rules",
    "host-resolver-rules",
    "ignore-certificate-errors",
    "disable-gpu-sandbox",
    "require",
];

/// The refused option `piece` names (see [`DEBUGGING_OPTIONS`]), compared case-insensitively;
/// Chromium reads `--x`, `-x` and `/x` alike on Windows.
/// Refuses a template that uses one of the [`DEBUGGING_OPTIONS`] (also checked right before
/// an emulator launch, so a profile saved before this check can't use them either).
pub(crate) fn check_no_debugging_options(template: &str, noun: &str) -> Result<()> {
    for token in tokenize_arguments(template, noun)? {
        if let Some(option) = token.split('=').find_map(debugging_option) {
            bail!("The {noun} argument \"{token}\" is not allowed: {option} options can run other programs or open the {noun} to remote control.");
        }
    }
    Ok(())
}

fn debugging_option(piece: &str) -> Option<String> {
    let lower = piece.trim().to_ascii_lowercase();
    let name = lower
        .strip_prefix("--")
        .or_else(|| lower.strip_prefix('-'))
        .or_else(|| lower.strip_prefix('/'))?;
    let refused =
        DEBUGGING_OPTIONS.iter().any(|o| name.starts_with(o)) || name.contains("cmd-prefix");
    refused.then(|| format!("--{name}"))
}

/// Checks an emulator argument template that the renderer sent (`emulators_save`), on top of
/// what [`build_emulator_argv`] already guarantees at launch (no shell, the ROM path is always
/// exactly one argument).
///
/// The executable is fixed by the user's own pick, but arguments can still make a real
/// emulator load other code (RetroArch's `-L <core.dll>`, script or config options). So every
/// file path in the template must stay inside the emulator's own folder:
/// - an absolute path (`C:\RetroArch\cores\x.dll`) must be inside the executable's folder;
/// - a relative path (`cores\x.dll`, resolved from that folder, which is the working folder at
///   launch) must start with a letter, digit, `_` or `.` and have no `..` segment;
/// - network paths (`\\host\share`, `//host`), root-relative paths (`\x.dll`), drive-relative
///   paths (`C:x.dll`), URLs and options with a path glued on (`-Lcores\x.dll`) are refused;
/// - text that starts with a placeholder (`{romdir}\x.cfg`) is about the game file and may not
///   contain `..`.
///
/// Each token is checked in parts split at `=`, `;` and `,` (`--core=cores\x.dll`). A part
/// like `/fullscreen` (`/` then a letter, then only letters, digits, `_` and `-`) is an
/// option, not a path; `/evil.dll` is a path. A placeholder may not be glued to an option or
/// `@` (`-L{rom}`, `@{rom}`); see the residual risk noted in `check_template_piece`. Control
/// characters, [`SHELL_CHARACTERS`] and non-ASCII characters other than letters and digits
/// (look-alikes of `\ / : " .`) are refused. A template without `{rom}` stays valid:
/// the ROM path is then appended as the last argument.
pub fn check_template(template: &str, executable: &str) -> Result<()> {
    check_arguments(
        template,
        executable,
        &PLACEHOLDERS,
        "emulator",
        r"cores\snes9x_libretro.dll",
    )
}

/// [`check_template`]'s rules for any program Game Lobby starts with an argument template
/// (emulators, custom launchers): `placeholders` are the template's own placeholders (a piece
/// starting with one is about the launched file; one glued to an option is refused), `noun`
/// names the program in messages and `example` is a path inside its folder.
pub(crate) fn check_arguments(
    template: &str,
    executable: &str,
    placeholders: &[&str],
    noun: &str,
    example: &str,
) -> Result<()> {
    if template.chars().any(char::is_control) {
        bail!("The {noun} arguments can't contain tabs, line breaks or other control characters.");
    }
    if let Some(c) = template.chars().find(|c| SHELL_CHARACTERS.contains(c)) {
        bail!("The {noun} arguments can't contain \"{c}\". Arguments are passed to the {noun} directly, without a command prompt.");
    }
    // Emulators with an ANSI command line get the arguments "best-fit" converted: look-alikes
    // such as U+FF3C (fullwidth \), U+2215 (division slash) or U+FF1A (fullwidth :) turn into
    // the real characters there and would slip past every path check below. Letters and digits
    // of any script are fine; other non-ASCII characters are refused.
    if let Some(c) = template
        .chars()
        .find(|c| !(c.is_ascii_graphic() || *c == ' ' || c.is_alphanumeric()))
    {
        bail!(
            "The {noun} arguments can't contain \"{c}\" (U+{:04X}). Use plain ASCII punctuation.",
            c as u32
        );
    }
    let folder = Path::new(executable)
        .parent()
        .and_then(|p| crate::tracker::normalize_path(&p.to_string_lossy()))
        .ok_or_else(|| anyhow!("The {noun} executable must be a full path to an .exe file."))?;
    check_no_debugging_options(template, noun)?;
    for token in tokenize_arguments(template, noun)? {
        // Also at `;` and `,`: programs split option values there (`--x=a;\Windows\b.dll`).
        for piece in token.split(['=', ';', ',']) {
            check_template_piece(piece, &folder, placeholders, noun).map_err(|reason| {
                anyhow!("The {noun} argument \"{token}\" is not allowed: {reason} Paths in the arguments must be inside the {noun}'s folder, like {example}.")
            })?;
        }
    }
    Ok(())
}

fn check_template_piece(
    piece: &str,
    folder: &str,
    placeholders: &[&str],
    noun: &str,
) -> Result<(), String> {
    let is_separator = |c: char| c == '\\' || c == '/';
    let climbs = || piece.split(is_separator).any(|s| s.trim_end() == "..");
    if piece.is_empty() {
        return Ok(());
    }
    if placeholders.iter().any(|p| piece.starts_with(p)) {
        return if climbs() {
            Err("it leaves the game's folder with \"..\".".into())
        } else {
            Ok(())
        };
    }
    // A placeholder glued to a short option or a response-file prefix (`-L{rom}`,
    // `/x{romdir}`, `@{rom}`) hands the game file to that option, or makes the emulator read
    // it as a list of more arguments.
    //
    // Residual risk, by design: the placeholders always name the game file, and Game Lobby
    // can't know what each emulator does with each option. A template that passes the game
    // file to an option that loads code (`-L {rom}` as separate arguments, or a code-loading
    // `--key={rom}`) would run that file as code. Such templates must be written by hand in
    // Settings, and the game file itself comes from a scan or a native picker.
    if piece.starts_with(['-', '/', '@']) && placeholders.iter().any(|p| piece.contains(p)) {
        return Err(format!(
            "it glues the game file to an option (\"-L{}\") or to \"@\".",
            placeholders[0]
        ));
    }
    if piece.contains(r"\\") || piece.contains("//") {
        return Err("it names a network location.".into());
    }
    if piece.contains(':') {
        let bytes = piece.as_bytes();
        let absolute = bytes.len() > 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && is_separator(bytes[2] as char)
            && !piece[2..].contains(':');
        if !absolute {
            return Err("it has a \":\" that isn't part of a full path.".into());
        }
        return match crate::tracker::normalize_path(piece) {
            Some(key) if !climbs() && crate::tracker::is_within(folder, &key) => Ok(()),
            _ => Err(format!("it points outside the {noun}'s folder.")),
        };
    }
    if !piece.contains(is_separator) {
        return Ok(());
    }
    if is_slash_option(piece) {
        return Ok(());
    }
    let first = piece.chars().next().unwrap_or(' ');
    if !(first.is_alphanumeric() || first == '_' || first == '.') {
        return Err(format!("it is not a path inside the {noun}'s folder."));
    }
    if climbs() {
        return Err(format!("it leaves the {noun}'s folder with \"..\"."));
    }
    Ok(())
}

/// `/fullscreen` is an option; `/evil.dll` is a file at the root of the drive.
fn is_slash_option(piece: &str) -> bool {
    let mut rest = piece.chars();
    rest.next() == Some('/')
        && rest.next().is_some_and(|c| c.is_ascii_alphabetic())
        && rest.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The first relative path in `argv` (already accepted by [`check_arguments`]), split into
/// pieces like there: a piece with `\` or `/` that isn't a full path (no `:`) and isn't an
/// option like `/fullscreen`. [`check_arguments`] confines such a path against the program's
/// folder, so it is only meaningful when that folder is also the working folder.
pub(crate) fn first_relative_path(argv: &[String]) -> Option<&str> {
    argv.iter().map(String::as_str).find(|token| {
        token.split(['=', ';', ',']).any(|piece| {
            piece.contains(['\\', '/']) && !piece.contains(':') && !is_slash_option(piece)
        })
    })
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
    check_no_debugging_options(&profile.args, "emulator")?;
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

/// Checks a custom launcher route's target right before launching (and when one is added by
/// hand): a `.lnk` or `.url` file is passed to the launcher, which may well open it, so it must
/// be one Game Lobby would start itself. That is a plain `.exe` shortcut without arguments, a
/// `.url` with a valid store link, or a `.lnk` whose target and arguments are exactly what the
/// user confirmed. Anything else (arguments nobody confirmed, a shortcut changed since, scripts,
/// documents) is refused. Other targets pass (the launcher's own kind checks apply). Reads
/// shortcuts (COM): call it off the async runtime threads.
pub fn check_launcher_shortcut(
    target: &str,
    confirmations: &HashMap<String, (String, String)>,
) -> Result<()> {
    let lower = target.to_lowercase();
    let path = Path::new(target);
    if !(lower.ends_with(".lnk") || lower.ends_with(".url")) || path.is_dir() {
        return Ok(());
    }
    match classify_shortcut(path, confirmations) {
        ShortcutKind::Exe(_) | ShortcutKind::Uri(_) | ShortcutKind::Confirmed => Ok(()),
        ShortcutKind::NeedsConfirmation { target, arguments } => bail!(
            "This shortcut runs {target} {arguments}, which you haven't confirmed (or it changed since you did). Rescan the launcher's games folder, or add the game again, and confirm it under Review."
        ),
        ShortcutKind::Invalid(reason) => bail!("{reason}"),
        ShortcutKind::Unreadable => bail!("The shortcut was not found or can't be read."),
    }
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
    let link = validate_launch_uri(target);
    if let Ok(uri) = link {
        return Ok(LocalLaunch::Uri(uri));
    }
    require_drive_path(target, link)?;
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

/// How a store route (any source but `local` and ROM routes) starts.
#[derive(Debug, PartialEq)]
pub enum StoreLaunch {
    /// A link that passed [`validate_launch_uri`].
    Link(String),
    /// `shell:AppsFolder\<AUMID>` (Xbox / Microsoft Store apps), opened through Explorer.
    AppsFolder,
    /// An existing file at an absolute drive path (resolved).
    Path(PathBuf),
}

/// Decides a store route's launch: a valid link first (decided by parsing, never by looking
/// for `://`), then an Apps folder entry, else an absolute drive path that exists. Anything
/// else (relative paths, opaque `scheme:...` text, UNC paths) is refused before it is resolved.
pub fn store_launch(target: &str) -> Result<StoreLaunch> {
    let link = validate_launch_uri(target);
    if let Ok(uri) = link {
        return Ok(StoreLaunch::Link(uri));
    }
    if target.starts_with("shell:AppsFolder\\") {
        return Ok(StoreLaunch::AppsFolder);
    }
    require_drive_path(target, link)?;
    let path = resolve(target);
    if !path.exists() {
        bail!("Launch target was not found.");
    }
    Ok(StoreLaunch::Path(path))
}

fn is_rom_route(route: &GameRoute) -> bool {
    route.source_id == local::ROM_SOURCE || route.emulator_id.is_some()
}

/// Starts a local PC executable in its own folder with `args` (the route's checked launch
/// options, each one argv entry; no shell). Falls back to the shell when Windows requires
/// elevation (error 740), so games that need administrator rights still start; that way can't
/// pass arguments, so a game with launch options is refused there instead of starting without
/// them.
async fn launch_local_exe(path: &Path, args: &[String]) -> Result<()> {
    match spawn_detached_in(path, args, path.parent()) {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(740) && args.is_empty() => {
            shell_open(path.to_string_lossy().into_owned()).await
        }
        Err(e) if e.raw_os_error() == Some(740) => bail!(
            "{} needs administrator rights, and Windows can't pass launch options that way. Remove this route's launch options on the game page, or start the game as administrator yourself.",
            path.display()
        ),
        Err(e) => Err(anyhow!("Could not start {}: {e}", path.display())),
    }
}

/// Hands a process Game Lobby started for `game_id` to the tracker (the session lasts while
/// that process runs), or only marks the game played when it can't be tracked.
fn track_started_process(state: &AppState, game_id: &str, pid: u32, exe: &Path) -> Result<()> {
    let settings = db::get_settings(&state.db.lock())?;
    let watching = state
        .tracker
        .watch_process(game_id, pid, &exe.to_string_lossy(), &settings);
    if !watching {
        db::mark_game_played(&state.db.lock(), game_id, now_ms())?;
    }
    Ok(())
}

/// What a launch did.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum LaunchOutcome {
    /// The game was started.
    Started,
    /// The game's "open first" launcher wasn't running: its program was started on its own and
    /// the launch waits for the user's Continue (`library_launch_continue` with `pendingId`).
    #[serde(rename_all = "camelCase")]
    Waiting {
        pending_id: String,
        launcher_id: String,
        launcher_name: String,
    },
}

/// Whether to wait for an "open first" launcher that isn't running yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherWait {
    /// Start the launcher alone first when it isn't running ([`LaunchOutcome::Waiting`]).
    IfNotRunning,
    /// Start the game now (the user confirmed the launcher is open).
    Never,
}

/// Whether the launcher program is running now (by its exact executable path). When the process
/// list can't be read, it counts as running: the game is started without opening it first.
async fn launcher_program_running(program: &Path) -> bool {
    if !cfg!(windows) {
        return true;
    }
    match on_thread(crate::platform::running_processes).await {
        Ok(Ok(processes)) => launchers::program_running(&processes, program),
        Ok(Err(e)) => {
            log::warn!("Could not check whether the launcher is running: {e:#}");
            true
        }
        Err(e) => {
            log::warn!("Could not check whether the launcher is running: {e:#}");
            true
        }
    }
}

/// The route an "open first" launcher route starts the game with: exactly the route Game Lobby
/// would give the target without any launcher, so every check of that route applies (it never
/// gets less checking for coming through a launcher). Reads the disk and shortcuts (COM): call it
/// off the async runtime threads.
///
/// - The game's own `local` or `rom` route with the same target, when it has one (it keeps that
///   route's emulator choice);
/// - else what adding the file as a game would make (`local::game_for_file`): an `.exe`, a plain
///   or confirmed `.lnk`, a `.url` store link, or a ROM with the system its folders or extension
///   name. A file inside a folder-format game (a PS3 `USRDIR\EBOOT.BIN`) gets that system.
/// - Identifiers and folders are refused: Game Lobby doesn't start those by itself.
///
/// The launcher route's own launch options go with it (they are the game's `.exe` options, see
/// `links::launcher_host`), only when the direct route still starts that same file. Its install
/// state goes with it too.
pub fn direct_route(
    game: &Game,
    route: &GameRoute,
    confirmations: &HashMap<String, (String, String)>,
) -> Result<GameRoute> {
    let target = route.launch_target.as_str();
    if !emulators::drive_path_ok(target) || target.chars().any(char::is_control) {
        bail!("This game's target is not a file, so it can't be started directly. Set its launcher to start games through it, or add the game again from its file.");
    }
    let path = Path::new(target);
    if path.is_dir() {
        bail!("This game's target is a folder ({target}), and Game Lobby only starts a game directly from a file: its .exe, a shortcut or a ROM. Link the game to its .exe or ROM file instead, or set its launcher to start games through it.");
    }
    let key = crate::tracker::normalize_path(target);
    let own = game.routes.iter().find(|r| {
        (r.source_id == local::LOCAL_SOURCE || r.source_id == local::ROM_SOURCE)
            && key.is_some()
            && crate::tracker::normalize_path(&r.launch_target) == key
    });
    let mut direct = match own {
        Some(own) => own.clone(),
        None => match local::folder_format_system(path) {
            Some(system) => GameRoute {
                source_id: local::ROM_SOURCE.into(),
                store: "Emulator".into(),
                launch_target: target.to_string(),
                system: Some(system.id.into()),
                ..Default::default()
            },
            None => match local::game_for_file(path, confirmations)? {
                local::LocalFile::Game(g) => g
                    .routes
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("This game's file can't be started."))?,
                local::LocalFile::NeedsConfirmation(_) => bail!(
                    "This shortcut runs a program with arguments you haven't confirmed (or it changed since you did). Rescan the launcher's games folder, or add the game again, and confirm it under Review."
                ),
            },
        },
    };
    if non_empty(&route.launch_args).is_some() && direct.launch_target != route.launch_target {
        bail!("Confirm this route's launch options again on its game page.");
    }
    direct.launch_args = route.launch_args.clone();
    direct.installed = route.installed;
    direct.launcher_id = None;
    direct.assigned = None;
    Ok(direct)
}

/// A game's own local or ROM route, checked and ready to start (nothing started yet).
enum DirectLaunch {
    /// A ROM: its emulator, argv and working folder (`prepare_emulator_launch`).
    Emulator(PathBuf, Vec<String>, PathBuf),
    /// A local route: how it starts (`local_launch`) and its confirmed launch options.
    Local(LocalLaunch, Vec<String>),
}

/// Every check of a local or ROM route's launch, before anything starts: the emulator profile
/// and the ROM, or the target (`local_launch`: shortcuts must be plain or confirmed) and the
/// launch options (confirmed for exactly this `.exe`).
async fn prepare_direct(db: &db::Db, route: &GameRoute) -> Result<DirectLaunch> {
    if is_rom_route(route) {
        let raw = db::emulator_profiles(&db.lock())?;
        // Availability is checked after the database lock is released.
        let profiles = emulators::refresh_available(raw);
        let profile = emulator_for_route(&profiles, route)?;
        let (exe, argv, cwd) = prepare_emulator_launch(&profile, route)?;
        return Ok(DirectLaunch::Emulator(exe, argv, cwd));
    }
    if route.source_id != local::LOCAL_SOURCE {
        bail!("This game can't be started directly.");
    }
    // Launch options (set on the game page) are checked again at every launch, wherever the
    // route came from; they are only accepted on a route that starts its .exe, and only when the
    // user confirmed exactly these options for exactly this .exe.
    let args = crate::links::checked_stored_args(&db.lock(), route)?;
    let confirmations = db::shortcut_confirmations(&db.lock())?;
    let owned = route.launch_target.clone();
    let how = on_thread(move || local_launch(&owned, &confirmations)).await??;
    Ok(DirectLaunch::Local(how, args))
}

/// Starts a checked [`DirectLaunch`] for `game_id` and tracks it.
async fn start_direct(
    state: &AppState,
    game_id: &str,
    route: &GameRoute,
    launch: DirectLaunch,
) -> Result<()> {
    match launch {
        DirectLaunch::Emulator(exe, argv, cwd) => {
            let pid = spawn_detached_in(&exe, &argv, Some(&cwd))
                .map_err(|e| anyhow!("Could not start {}: {e}", exe.display()))?;
            track_started_process(state, game_id, pid, &exe)
        }
        DirectLaunch::Local(how, args) => {
            match how {
                LocalLaunch::Exe(exe) => launch_local_exe(&exe, &args).await?,
                LocalLaunch::Shortcut(lnk) => {
                    shell_open(lnk.to_string_lossy().into_owned()).await?
                }
                LocalLaunch::Uri(uri) => shell_open(uri).await?,
            }
            watch_game(state, game_id, route)
        }
    }
}

/// Starts session tracking for a game started without a process handle (by its folders).
fn watch_game(state: &AppState, game_id: &str, route: &GameRoute) -> Result<()> {
    if route.installed != Some(false) {
        let settings = db::get_settings(&state.db.lock())?;
        let watching = state.tracker.watch(&state.db, game_id, &settings);
        if !watching {
            db::mark_game_played(&state.db.lock(), game_id, now_ms())?;
        }
    }
    Ok(())
}

/// Why an "open first" game can't start directly, naming its file when no emulator fits it.
fn direct_error(e: anyhow::Error, direct: &GameRoute) -> anyhow::Error {
    if !is_rom_route(direct) || !e.to_string().starts_with("No emulator is set up") {
        return e;
    }
    let file = Path::new(&direct.launch_target)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| direct.launch_target.clone());
    match direct.system.as_deref().and_then(crate::catalog::system) {
        Some(system) => anyhow!(
            "No emulator is set up for {file} ({}). Add one under Settings → Emulators, then press Play again.",
            system.name
        ),
        None => anyhow!(
            "No emulator fits {file}: its system can't be told from its folders or its extension. Put it in a folder named after its system (like \"PS2\"), or add the game from Add local game and choose an emulator for it."
        ),
    }
}

/// Play on an "open first" launcher's route: every check first (the launcher's program, and the
/// game's direct start like its own local or ROM route); then, when the launcher isn't running,
/// only the launcher's program, on its own with no arguments, and a pending launch for Continue;
/// otherwise (or on Continue) the game, directly. The launcher never gets the game.
async fn launch_open_first(
    state: &AppState,
    game: &Game,
    route: &GameRoute,
    launcher: &launchers::CustomLauncher,
    wait: LauncherWait,
) -> Result<LaunchOutcome> {
    let open = launchers::prepare_open_first(launcher, route)?;
    let confirmations = db::shortcut_confirmations(&state.db.lock())?;
    let (owned_game, owned_route) = (game.clone(), route.clone());
    let direct =
        on_thread(move || direct_route(&owned_game, &owned_route, &confirmations)).await??;
    // Checked before the launcher is opened too, so a launch that would be refused doesn't open
    // it first.
    let launch = prepare_direct(&state.db, &direct)
        .await
        .map_err(|e| direct_error(e, &direct))?;
    if wait == LauncherWait::IfNotRunning && !launcher_program_running(&open.program).await {
        // The launcher alone: its checked program, no arguments, nothing tracked. Continue only
        // names this pending id and runs `launch_game` again, with every check again.
        spawn_detached_in(&open.program, &[], Some(&open.cwd))
            .map_err(|e| anyhow!("Could not start {}: {e}", open.program.display()))?;
        let pending_id = state
            .pending_launches
            .begin(crate::pending_launch::Pending {
                game_id: game.id.clone(),
                launcher_id: launcher.id.clone(),
                launcher_name: launcher.name.clone(),
                program: launcher.executable.clone(),
                start_mode: launcher.start_mode,
                route: route.clone(),
            });
        return Ok(LaunchOutcome::Waiting {
            pending_id,
            launcher_id: launcher.id.clone(),
            launcher_name: launcher.name.clone(),
        });
    }
    start_direct(state, &game.id, &direct, launch).await?;
    Ok(LaunchOutcome::Started)
}

pub async fn launch_game(
    app: &AppHandle,
    game: &Game,
    route_index: usize,
    wait: LauncherWait,
) -> Result<LaunchOutcome> {
    let route = game
        .routes
        .get(route_index)
        .ok_or_else(|| anyhow!("No launch route is configured."))?;
    let state = app.state::<AppState>();
    // Custom launchers: only the launcher's own .exe, started directly (never through a shell
    // or a link), with the checked argv of `launchers::prepare_launch`.
    if let Some(launcher_id) = launchers::launcher_id_of(route) {
        let launcher = launchers::get(&state.db.lock(), launcher_id)?.ok_or_else(|| {
            anyhow!("This game's launcher was deleted. Add the game again to another launcher.")
        })?;
        if launcher.start_mode == launchers::StartMode::OpenFirst {
            return launch_open_first(&state, game, route, &launcher, wait).await;
        }
        // Only a Continue launches with `Never`, and only "open first" launchers wait: one that
        // was switched to "through" meanwhile doesn't get the game from a Continue.
        if wait == LauncherWait::Never {
            bail!(
                "{} changed since you pressed Play. Press Play again.",
                launcher.name
            );
        }
        // "Through": the launcher's program, once, with the game as its argument (also when it
        // isn't running yet: it gets the game in the same start, so it is never opened twice).
        // A shortcut handed to the launcher must still be one the user may run (confirmed,
        // or a plain `.exe` / store link), however the route got there (scan, Add game, a
        // health relocation). Read through COM, off the async threads.
        let confirmations = db::shortcut_confirmations(&state.db.lock())?;
        let target = route.launch_target.clone();
        on_thread(move || check_launcher_shortcut(&target, &confirmations)).await??;
        // Touches the filesystem: the database lock is already released.
        let launch = launchers::prepare_launch(&launcher, route)?;
        // The route's own launch options (set on the game page) run only when the user confirmed
        // exactly this program, command line and working folder.
        let argv = crate::links::launcher_launch_argv(&state.db.lock(), &launcher, route, &launch)?;
        let pid = spawn_detached_in(&launch.program, &argv, Some(&launch.cwd))
            .map_err(|e| anyhow!("Could not start {}: {e}", launch.program.display()))?;
        track_started_process(&state, &game.id, pid, &launch.program)?;
        return Ok(LaunchOutcome::Started);
    }
    if is_rom_route(route) || route.source_id == local::LOCAL_SOURCE {
        let launch = prepare_direct(&state.db, route).await?;
        start_direct(&state, &game.id, route, launch).await?;
        return Ok(LaunchOutcome::Started);
    }
    let target = route.launch_target.as_str();
    match store_launch(target)? {
        StoreLaunch::Link(uri) => shell_open(uri).await?,
        StoreLaunch::AppsFolder => {
            let windows_dir = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            spawn_detached(&windows_dir.join("explorer.exe"), &[target.to_string()])?;
        }
        StoreLaunch::Path(path) => match non_empty(&route.launch_args) {
            Some(args) => {
                if route.source_id == "epic" {
                    // Epic routes with arguments came from the removed Legendary import.
                    // They are never run; the cleanup in `db::migrate` turns them into
                    // launcher links on every start.
                    bail!("This Epic Games entry still uses Legendary, which is no longer supported. Restart Game Lobby to switch it to the Epic Games Launcher.");
                }
                spawn_detached(&path, &parse_launch_arguments(Some(args)))?;
            }
            None => shell_open(path.to_string_lossy().into_owned()).await?,
        },
    }
    watch_game(&state, &game.id, route)?;
    Ok(LaunchOutcome::Started)
}

#[cfg(test)]
pub(crate) mod tests {
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
        // Opaque links (no `//`) read as relative file paths on Windows; backslashes and
        // control characters never belong in a link.
        for bad in OPAQUE_LINKS.iter().copied().chain([
            "steam://run/570\\..\\x",
            "steam://run/5\n70",
            "steam:///run/570",
            "steam://",
        ]) {
            assert!(validate_launch_uri(bad).is_err(), "{bad}");
        }
        assert_eq!(
            validate_launch_uri("Steam://run/570").unwrap(),
            "steam://run/570"
        );
    }

    /// Opaque store "links" that once passed the scheme check.
    pub(crate) const OPAQUE_LINKS: [&str; 5] = [
        r"xbox:x\..\..\Windows\System32\calc.exe",
        r"xbox:x\..\..\..\..\..\..\Windows\System32\calc.exe",
        r"xbox:\..\x.exe",
        "uplay:launch/1",
        "steam:run/1",
    ];

    #[test]
    fn store_routes_launch_only_links_apps_or_absolute_paths() {
        let dir = TempDir::new("launch-store");
        let exe = dir.file("Game/game.exe");
        assert_eq!(
            store_launch("steam://run/570").unwrap(),
            StoreLaunch::Link("steam://run/570".into())
        );
        assert_eq!(
            store_launch(r"shell:AppsFolder\Pkg_123!App").unwrap(),
            StoreLaunch::AppsFolder
        );
        assert_eq!(
            store_launch(&exe.to_string_lossy()).unwrap(),
            StoreLaunch::Path(exe.clone())
        );
        let missing = dir.path().join("Game/missing.exe");
        for bad in OPAQUE_LINKS.iter().map(|s| s.to_string()).chain([
            missing.to_string_lossy().into_owned(),
            r"Windows\System32\calc.exe".into(),
            r"\Windows\System32\calc.exe".into(),
            r"..\..\Windows\System32\calc.exe".into(),
            r"\\server\share\calc.exe".into(),
            "C:calc.exe".into(),
            "https://evil.example/x.exe".into(),
        ]) {
            assert!(store_launch(&bad).is_err(), "{bad}");
        }
        // Local routes take the same decision.
        let none = HashMap::new();
        for bad in OPAQUE_LINKS {
            assert!(local_launch(bad, &none).is_err(), "{bad}");
        }
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

    /// Options that make Chromium/Electron/Node programs run other code; refused in every
    /// template (emulators and custom launchers alike).
    pub(crate) const DEBUGGING_TEMPLATES: [&str; 31] = [
        "--gpu-launcher=calc.exe {rom}",
        "--gpu-launcher calc.exe",
        "\"--utility-cmd-prefix=cmd /c calc\" {rom}",
        "--renderer-cmd-prefix=calc",
        "--plugin-cmd-prefix=x",
        "--inspect {rom}",
        "--inspect=9229",
        "--inspect-brk=0.0.0.0:9229",
        "--INSPECT",
        "-inspect",
        "/inspect",
        "--remote-debugging-port=9222",
        "--remote-debugging-pipe",
        "--Remote-Debugging-Address=0.0.0.0",
        "--no-sandbox {rom}",
        "--load-extension=ext",
        "--disable-web-security",
        "--js-flags=--allow-natives-syntax",
        "--browser-subprocess-path=calc.exe",
        "--x=--inspect",
        "'--gpu-launcher=calc'",
        "--NO-SANDBOX",
        "--js-flags",
        "-remote-debugging-port=1",
        "--proxy-server=evil.example",
        "--proxy-pac-url=http://evil.example/p.pac",
        "--host-rules=MAP * evil.example",
        "--host-resolver-rules=MAP * evil.example",
        "--ignore-certificate-errors",
        "--disable-gpu-sandbox",
        "--require=x.js",
    ];

    #[test]
    fn options_that_run_other_programs_are_refused() {
        let exe = r"D:\Emulators\RetroArch\retroarch.exe";
        for bad in DEBUGGING_TEMPLATES {
            let e = check_template(bad, exe).unwrap_err().to_string();
            assert!(e.contains("not allowed"), "{bad}: {e}");
        }
        for known in crate::catalog::EMULATORS {
            assert!(
                debugging_option(known.default_args).is_none(),
                "{}",
                known.id
            );
            for token in tokenize_template(known.default_args).unwrap() {
                assert!(
                    token.split('=').find_map(debugging_option).is_none(),
                    "{}",
                    known.id
                );
            }
        }
        for good in [
            "--fullscreen",
            "--no-gui",
            "--sandboxed",
            "/f",
            "-batch",
            "{rom}",
        ] {
            assert!(debugging_option(good).is_none(), "{good}");
            assert!(check_template(good, exe).is_ok(), "{good}");
        }
    }

    #[test]
    fn tokenizer_messages_name_the_program() {
        let e = tokenize_arguments("\"x", "launcher")
            .unwrap_err()
            .to_string();
        assert!(e.contains("launcher") && !e.contains("emulator"), "{e}");
        let e = tokenize_template("\"x").unwrap_err().to_string();
        assert!(e.contains("emulator"), "{e}");
        let many = "a ".repeat(MAX_TEMPLATE_TOKENS + 1);
        assert!(tokenize_arguments(&many, "launcher")
            .unwrap_err()
            .to_string()
            .contains("launcher"));
    }

    #[test]
    fn renderer_templates_keep_paths_inside_the_emulator_folder() {
        let exe = r"D:\Emulators\RetroArch\retroarch.exe";
        // Every catalog default passes, as do usual hand-written templates.
        for known in crate::catalog::EMULATORS {
            assert!(
                check_template(known.default_args, exe).is_ok(),
                "{}",
                known.id
            );
        }
        for good in [
            "",
            "{rom}",
            "--fullscreen",
            "/fullscreen {rom}",
            r"-L cores\snes9x_libretro.dll {rom}",
            r#"-L "cores\snes 9x_libretro.dll" "{rom}""#,
            r"-L .\cores\mgba_libretro.dll {rom}",
            "-L cores/mgba_libretro.dll {rom}",
            r"-L D:\Emulators\RetroArch\cores\x.dll {rom}",
            r"-L d:/emulators/retroarch/cores/x.dll {rom}",
            r"--config=config\retroarch.cfg {rom}",
            r"--path={rom} --dir={romdir} --name={romname}",
            r"--cfg={romdir}\game.cfg",
            "-C Dolphin.Display.Fullscreen=True -b -e {rom}",
            "--set-volume=50",
            "/f-1 /no_gui {rom}",
            // Letters and digits of any script are fine.
            "--profile=Pokémon_ポケモン {rom}",
        ] {
            assert!(check_template(good, exe).is_ok(), "{good} should pass");
        }
        for bad in [
            // Code from outside the emulator's folder.
            r"-L C:\Users\Me\Downloads\evil.dll {rom}",
            r"-L D:\Emulators\Other\x.dll {rom}",
            r"-L D:\Emulators\RetroArch\..\Other\x.dll {rom}",
            r"-L D:\Emulators\RetroArchX\x.dll {rom}",
            r"-L ..\evil.dll {rom}",
            r"-L cores\..\..\evil.dll {rom}",
            r"-L \evil.dll {rom}",
            "-L /Users/me/evil.dll {rom}",
            r"-L \\attacker\share\evil.dll {rom}",
            "-L //attacker/share/evil.dll {rom}",
            "-L C:evil.dll {rom}",
            r"-LC:\evil.dll {rom}",
            r"-L\evil.dll {rom}",
            r"--core=C:\evil.dll {rom}",
            r"--core=..\evil.dll {rom}",
            r"-L D:\Emulators\RetroArch\cores\x.dll:stream {rom}",
            "--script=https://evil.example/x.lua {rom}",
            r"--cfg={romdir}\..\..\evil.cfg",
            // Shell tricks and control characters.
            "{rom} & calc.exe",
            "{rom} | calc",
            "{rom} > out.txt",
            "%COMSPEC% /c calc",
            "^& calc",
            "`calc`",
            "{rom}\ncalc.exe",
            "{rom}\tx",
            // A root-relative file is not an option.
            "-L /evil.dll {rom}",
            "/evil.dll",
            "/1x",
            // The game file glued to an option or a response-file prefix.
            "-L{rom}",
            "-d{romdir}",
            "/x{rom}",
            "@{rom}",
            "--core=-L{rom}",
            // Look-alikes that best-fit to \ / : " . on an ANSI command line.
            "-L C\u{FF1A}\u{FF3C}Users\u{FF3C}Me\u{FF3C}evil.dll {rom}",
            "-L \u{FF3C}\u{FF3C}attacker\u{FF3C}share\u{FF3C}evil.dll {rom}",
            "-L \u{FF0F}Users\u{FF0F}evil.dll {rom}",
            "-L \u{2215}Users\u{2215}evil.dll {rom}",
            "-L \u{FF0E}\u{FF0E}\u{FF3C}evil.dll {rom}",
            "\u{FF02}-L C:\\x.dll\u{FF02} {rom}",
            // Paths hidden behind the separators some programs split option values at.
            r"a;\Windows\x.dll {rom}",
            r"--x=a,\Windows\x.dll {rom}",
            r"--x=ok;C:\Elsewhere\x.dll {rom}",
            // Broken templates.
            "\"{rom}",
        ] {
            assert!(check_template(bad, exe).is_err(), "{bad} should be refused");
        }
        assert!(check_template("{rom}", "retroarch.exe").is_err());
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

    // -----------------------------------------------------------------------------------------
    // "Open first" launchers: the game starts directly, like its own local or ROM route.

    fn launcher_route(target: &Path) -> GameRoute {
        GameRoute {
            source_id: "launcher:retro".into(),
            store: "Custom".into(),
            launch_target: target.to_string_lossy().into_owned(),
            installed: Some(true),
            ..Default::default()
        }
    }

    fn game_with(routes: Vec<GameRoute>) -> Game {
        Game {
            id: "g".into(),
            title: "Cairn".into(),
            routes,
            ..Default::default()
        }
    }

    #[test]
    fn open_first_games_start_like_their_own_local_or_rom_route() {
        let dir = TempDir::new("direct-route");
        let none = HashMap::new();
        // A PC game's .exe: a local route of exactly that file.
        let exe = dir.file(r"PC Game\00356\Cairn.exe");
        let route = launcher_route(&exe);
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap();
        assert_eq!(direct.source_id, local::LOCAL_SOURCE);
        assert_eq!(direct.launch_target, route.launch_target);
        assert_eq!(direct.launcher_id, None);
        assert_eq!(direct.installed, Some(true));
        assert_eq!(
            local_launch(&direct.launch_target, &none).unwrap(),
            LocalLaunch::Exe(exe.clone())
        );

        // A ROM: a ROM route with the system its folder names (no emulator chosen: the default).
        let chd = dir.file(r"Sony Playstation 2\Game (USA).chd");
        let route = launcher_route(&chd);
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap();
        assert_eq!(direct.source_id, local::ROM_SOURCE);
        assert_eq!(direct.system.as_deref(), Some("ps2"));
        assert_eq!(direct.emulator_id, None);
        assert!(is_rom_route(&direct));

        // A PS3 folder game's EBOOT.BIN: the folder format's system (.bin alone is ambiguous).
        dir.file(r"Sony Playstation 3\Sly\PS3_GAME\PARAM.SFO");
        let eboot = dir.file(r"Sony Playstation 3\Sly\PS3_GAME\USRDIR\EBOOT.BIN");
        let route = launcher_route(&eboot);
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap();
        assert_eq!(direct.source_id, local::ROM_SOURCE);
        assert_eq!(direct.system.as_deref(), Some("ps3"));
        assert_eq!(direct.launch_target, route.launch_target);

        // The game's own ROM route for the same file wins: it keeps its emulator choice.
        let own = GameRoute {
            emulator_id: Some("pcsx2".into()),
            system: Some("ps2".into()),
            ..rom_route(&chd, Some("pcsx2"))
        };
        let route = launcher_route(&chd);
        let game = game_with(vec![route.clone(), own]);
        let direct = direct_route(&game, &route, &none).unwrap();
        assert_eq!(direct.emulator_id.as_deref(), Some("pcsx2"));
        assert_eq!(direct.source_id, local::ROM_SOURCE);

        // Folders and identifiers aren't started by Game Lobby itself.
        let folder = dir.dir(r"PC Game\00357");
        let route = launcher_route(&folder);
        let e = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap_err();
        assert!(e.to_string().contains("is a folder"), "{e}");
        let id = GameRoute {
            launch_target: "Hollow Knight".into(),
            ..launcher_route(&exe)
        };
        let e = direct_route(&game_with(vec![id.clone()]), &id, &none).unwrap_err();
        assert!(e.to_string().contains("not a file"), "{e}");
        // Files Game Lobby wouldn't start as a game aren't started through a launcher either.
        for bad in [dir.file(r"PC Game\run.bat"), dir.file(r"PC Game\notes.txt")] {
            let route = launcher_route(&bad);
            assert!(
                direct_route(&game_with(vec![route.clone()]), &route, &none).is_err(),
                "{}",
                bad.display()
            );
        }
        let missing = launcher_route(&dir.path().join("gone.exe"));
        assert!(direct_route(&game_with(vec![missing.clone()]), &missing, &none).is_err());
    }

    #[test]
    fn open_first_launch_options_belong_to_the_games_own_exe() {
        let dir = TempDir::new("direct-options");
        let none = HashMap::new();
        let exe = dir.file(r"Cairn\Cairn.exe");
        let route = GameRoute {
            launch_args: Some(r#"["-windowed"]"#.into()),
            ..launcher_route(&exe)
        };
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap();
        assert_eq!(direct.launch_args, route.launch_args);
        // They are checked exactly like the options of a local route of that .exe: refused until
        // confirmed for that .exe, and then the same confirmation covers both.
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        assert!(crate::links::checked_stored_args(&conn, &direct).is_err());
        let local = GameRoute {
            source_id: local::LOCAL_SOURCE.into(),
            launch_target: exe.to_string_lossy().into_owned(),
            launch_args: route.launch_args.clone(),
            ..Default::default()
        };
        crate::links::confirm_args(&conn, &local, &["-windowed".to_string()]).unwrap();
        assert_eq!(
            crate::links::checked_stored_args(&conn, &direct).unwrap(),
            ["-windowed"]
        );
        // A ROM takes no launch options (its emulator's arguments apply).
        let rom = dir.file(r"GBA\x.gba");
        let route = GameRoute {
            launch_args: Some(r#"["-x"]"#.into()),
            ..launcher_route(&rom)
        };
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap();
        assert!(crate::links::checked_stored_args(&conn, &direct).is_err());
    }

    #[tokio::test]
    async fn open_first_games_get_every_check_of_a_direct_start() {
        let dir = TempDir::new("direct-prepare");
        let none = HashMap::new();
        let db = db::Db::memory().unwrap();
        // A ROM without an emulator: a clear error naming the file.
        let rom = dir.file(r"Roms\GBA\Cairn Quest.gba");
        let route = launcher_route(&rom);
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap();
        let e = prepare_direct(&db, &direct)
            .await
            .err()
            .map(|e| direct_error(e, &direct))
            .unwrap()
            .to_string();
        assert!(e.contains("Cairn Quest.gba"), "{e}");
        // With an emulator: exactly what the game's own ROM route would run.
        let emulator = dir.file(r"mGBA\mGBA.exe");
        db::save_emulator(&db.lock(), &profile(&emulator)).unwrap();
        match prepare_direct(&db, &direct).await.unwrap() {
            DirectLaunch::Emulator(exe, argv, cwd) => {
                let own = rom_route(&rom, None);
                let (want_exe, want_argv, want_cwd) =
                    prepare_emulator_launch(&profile(&emulator), &own).unwrap();
                assert_eq!((exe, argv, cwd), (want_exe, want_argv, want_cwd));
            }
            DirectLaunch::Local(..) => panic!("a ROM starts with its emulator"),
        }
        // An .exe: started directly, no options.
        let exe = dir.file(r"Cairn\Cairn.exe");
        let route = launcher_route(&exe);
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap();
        match prepare_direct(&db, &direct).await.unwrap() {
            DirectLaunch::Local(how, args) => {
                assert_eq!(how, LocalLaunch::Exe(exe.clone()));
                assert!(args.is_empty());
            }
            DirectLaunch::Emulator(..) => panic!("an .exe starts directly"),
        }
        // Unconfirmed options: refused before anything (also the launcher) would start.
        let with = GameRoute {
            launch_args: Some(r#"["-x"]"#.into()),
            ..route
        };
        let direct = direct_route(&game_with(vec![with.clone()]), &with, &none).unwrap();
        assert!(prepare_direct(&db, &direct).await.is_err());
        // Only local and ROM routes start this way.
        let store = GameRoute {
            source_id: "steam".into(),
            launch_target: "steam://rungameid/1".into(),
            ..Default::default()
        };
        assert!(prepare_direct(&db, &store).await.is_err());
    }

    #[cfg(windows)]
    #[test]
    fn open_first_shortcuts_need_the_same_confirmation_as_local_ones() {
        use crate::platform::create_shortcut;
        let dir = TempDir::new("direct-lnk");
        let none = HashMap::new();
        let exe = dir.file(r"Game\game.exe");
        let plain = dir.path().join("Plain.lnk");
        create_shortcut(&plain, &exe.to_string_lossy(), "").unwrap();
        let route = launcher_route(&plain);
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap();
        assert_eq!(direct.source_id, local::LOCAL_SOURCE);
        assert_eq!(
            local_launch(&direct.launch_target, &none).unwrap(),
            LocalLaunch::Exe(exe.clone())
        );
        let args = dir.path().join("Args.lnk");
        create_shortcut(&args, &exe.to_string_lossy(), "-x").unwrap();
        let route = launcher_route(&args);
        let e = direct_route(&game_with(vec![route.clone()]), &route, &none).unwrap_err();
        assert!(e.to_string().contains("haven't confirmed"), "{e}");
        let (target, arguments) = crate::platform::resolve_shortcut(&args).unwrap();
        let confirmed: HashMap<String, (String, String)> =
            [(crate::localscan::key_of(&args), (target, arguments))].into();
        let direct = direct_route(&game_with(vec![route.clone()]), &route, &confirmed).unwrap();
        assert_eq!(
            local_launch(&direct.launch_target, &confirmed).unwrap(),
            LocalLaunch::Shortcut(args.clone())
        );
    }
}
