//! Custom launchers: programs the user sets up to start some of their games (Heroic, Playnite,
//! a mod manager, an older store client). A launcher works like a store: its games have a
//! route with `sourceId` `launcher:<id>` (store `Custom`), the renderer shows the launcher's
//! name, icon and colour for them, and launching runs the launcher's program with the game's
//! target as one argument.
//!
//! Trust model (the renderer is untrusted), modelled on emulator profiles:
//! - Executable: an absolute local `.exe` ([`emulators::executable_path_ok`]: no UNC, URI,
//!   relative path, `.bat`/`.cmd`/`.ps1`). It is set only from a native-picker result
//!   ([`crate::picks::PickKind::LauncherExecutable`]) or kept as stored; the renderer never
//!   sends a path, only "use the file I just picked" ([`accept_renderer_launcher`]). Backups
//!   (a file the user chose in a native dialog) may carry one, with the same shape checks.
//! - Arguments: [`check_launcher_template`], the emulator rules
//!   ([`crate::launch::check_arguments`]: no shell or control characters, no look-alike
//!   Unicode, file paths only inside the launcher's folder) plus: the only placeholder is
//!   `{target}`, and only as a whole argument. Launches never use a shell: every template token
//!   is one argv entry and the target is exactly one entry ([`build_argv`]).
//! - Target, per launcher ([`TargetKind`]): a file or a folder chosen in a native picker by Rust
//!   (`launchers_add_game`), or a short identifier typed by the user ([`check_identifier`]).
//!   Checked again at every launch ([`check_target`]).
//! - Working folder: the executable's folder, or a folder chosen in a native picker
//!   ([`crate::picks::PickKind::LauncherFolder`]).
//! - Icon: one of [`BUILTIN_ICONS`] (stored by name) or an image the user chose, copied into the
//!   covers folder by [`import_icon`] (magic-byte check, size cap, PNG/JPEG/WebP/GIF, never SVG)
//!   and referenced by its bare file name only ([`icon_file_ok`]).
//! - Colour: `#rrggbb` ([`check_color`]).

use crate::db;
use crate::emulators::{drive_path_ok, executable_path_ok};
use crate::launch::{check_arguments, tokenize_arguments};
use crate::localscan::key_of;
use crate::types::{now_ms, Game, GamePatch, GameRoute};
use anyhow::{anyhow, bail, Result};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// `GameRoute.sourceId` prefix of a launcher's games: `launcher:<launcher id>`.
pub const SOURCE_PREFIX: &str = "launcher:";
/// The only placeholder in a launcher's argument template.
pub const TARGET: &str = "{target}";
/// Longest typed identifier.
pub const MAX_IDENTIFIER_CHARS: usize = 200;
pub const MAX_NAME_CHARS: usize = 64;
pub const MAX_TEMPLATE_CHARS: usize = 2_048;
/// Largest icon image that can be chosen.
pub const MAX_ICON_BYTES: u64 = 5 * 1024 * 1024;
/// At most this many launchers (backups included).
pub const MAX_LAUNCHERS: usize = 200;

/// The built-in icons (lucide icon names, kebab-case). Keep in sync with
/// `src/shared/launchers.ts`.
pub const BUILTIN_ICONS: [&str; 24] = [
    "gamepad-2",
    "joystick",
    "rocket",
    "swords",
    "sword",
    "shield",
    "crown",
    "castle",
    "ghost",
    "skull",
    "bot",
    "puzzle",
    "dices",
    "trophy",
    "star",
    "heart",
    "flame",
    "zap",
    "car",
    "terminal",
    "code",
    "wrench",
    "package",
    "folder",
];

const ICON_RULES: crate::art::ImageRules = crate::art::ImageRules {
    extensions: &["png", "jpg", "jpeg", "webp", "gif"],
    kinds: "PNG, JPEG, WebP or GIF",
    max_bytes: MAX_ICON_BYTES,
};

static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{1,64}$").unwrap());
static COLOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#[0-9A-Fa-f]{6}$").unwrap());
/// Icon files as [`import_icon`] names them.
static ICON_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^launcher-icon-[0-9a-f]{32}\.(png|jpg|webp|gif)$").unwrap());

/// What a launcher's games carry as their target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A file chosen in a native picker.
    File,
    /// A folder chosen in a native picker.
    Folder,
    /// A short identifier the user typed ([`check_identifier`]).
    Identifier,
}

impl TargetKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "file" => Some(Self::File),
            "folder" => Some(Self::Folder),
            "identifier" => Some(Self::Identifier),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
            Self::Identifier => "identifier",
        }
    }
}

/// A stored launcher (table `custom_launchers`), as listed to the renderer and kept in backups.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomLauncher {
    pub id: String,
    pub name: String,
    /// `builtin` (`icon_value` is one of [`BUILTIN_ICONS`]) or `image` (`icon_value` is a
    /// covers-folder file name, see [`icon_file_ok`]).
    pub icon_kind: String,
    pub icon_value: String,
    /// `#rrggbb`.
    pub color: String,
    pub executable: String,
    pub args_template: String,
    /// `file`, `folder` or `identifier` ([`TargetKind`]).
    pub target_kind: String,
    /// `None` = the executable's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    /// Output only: the executable exists right now (filled after the database lock is gone).
    #[serde(default)]
    pub available: bool,
}

impl CustomLauncher {
    pub fn kind(&self) -> Result<TargetKind> {
        TargetKind::parse(&self.target_kind)
            .ok_or_else(|| anyhow!("The launcher's target kind is invalid."))
    }
}

/// The launcher id of a launcher route.
pub fn launcher_id_of(route: &GameRoute) -> Option<&str> {
    route
        .source_id
        .strip_prefix(SOURCE_PREFIX)
        .filter(|id| !id.is_empty())
}

pub fn source_id(launcher_id: &str) -> String {
    format!("{SOURCE_PREFIX}{launcher_id}")
}

pub fn is_launcher_route(route: &GameRoute) -> bool {
    route.source_id.starts_with(SOURCE_PREFIX)
}

// ---------------------------------------------------------------------------
// Field checks

pub fn check_id(raw: &str) -> Result<String> {
    let id = raw.trim();
    if !ID.is_match(id) {
        bail!("The launcher id is invalid.");
    }
    Ok(id.to_string())
}

pub fn check_name(raw: &str) -> Result<String> {
    if raw.chars().any(char::is_control) {
        bail!("The launcher name can't contain control characters.");
    }
    let name = raw.trim();
    let len = name.chars().count();
    if len == 0 || len > MAX_NAME_CHARS {
        bail!("The launcher name must be between 1 and {MAX_NAME_CHARS} characters.");
    }
    Ok(name.to_string())
}

/// `#rrggbb`, stored lower-case.
pub fn check_color(raw: &str) -> Result<String> {
    if !COLOR.is_match(raw) {
        bail!("The launcher colour must be a hex colour like #4f8cff.");
    }
    Ok(raw.to_ascii_lowercase())
}

/// A covers-folder file name as [`import_icon`] creates it (syntax only).
pub fn icon_file_ok(name: &str) -> bool {
    ICON_FILE.is_match(name)
}

/// `builtin` + a [`BUILTIN_ICONS`] name, or `image` + an [`icon_file_ok`] name.
pub fn check_icon(kind: &str, value: &str) -> Result<()> {
    match kind {
        "builtin" if BUILTIN_ICONS.contains(&value) => Ok(()),
        "builtin" => bail!("Choose one of the built-in icons."),
        "image" if icon_file_ok(value) => Ok(()),
        "image" => {
            bail!("The launcher icon image is not valid. Choose it again with \"Use an image…\".")
        }
        _ => bail!("The launcher icon is invalid."),
    }
}

/// A typed per-game identifier, passed to the launcher as exactly one argument: at most
/// [`MAX_IDENTIFIER_CHARS`] characters of `A-Z a-z 0-9 space . _ : @ # ( ) ' + , ! -`, not
/// starting with `-` or `/` (it would be read as an option) or `@` (a response file to many
/// programs), not starting like a drive (`C:`, a drive-relative path), without `\`, `..` or control
/// characters. Leading and trailing spaces are removed. Only ASCII is accepted, so no
/// look-alike of `\ / : " .` can be "best-fit" converted into a real one.
pub fn check_identifier(raw: &str) -> Result<String> {
    if raw.chars().any(char::is_control) {
        bail!("The identifier can't contain tabs, line breaks or other control characters.");
    }
    let id = raw.trim_matches(' ');
    if id.is_empty() {
        bail!("Enter the game's identifier.");
    }
    if id.chars().count() > MAX_IDENTIFIER_CHARS {
        bail!("The identifier must be at most {MAX_IDENTIFIER_CHARS} characters.");
    }
    if id.contains('\\') || id.contains('/') {
        bail!("The identifier can't contain \"\\\" or \"/\". To use a file or folder, set the launcher's target to File or Folder.");
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || " ._:@#()'+,!-".contains(c);
    if let Some(c) = id.chars().find(|c| !allowed(*c)) {
        bail!(
            "The identifier can't contain \"{c}\". Use letters, digits, spaces and . _ : @ # ( ) ' + , ! -"
        );
    }
    if id.starts_with('-') {
        bail!("The identifier can't start with \"-\": the launcher would read it as an option.");
    }
    if id.starts_with('@') {
        bail!("The identifier can't start with \"@\": the launcher could read it as a file of more arguments.");
    }
    let bytes = id.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        bail!("The identifier can't start like a drive (\"C:\"): the launcher could read it as a file path.");
    }
    if id.contains("..") {
        bail!("The identifier can't contain \"..\".");
    }
    Ok(id.to_string())
}

/// A stored target of the given kind, shape only: an absolute drive path for files and
/// folders, a valid identifier (exactly as stored) otherwise.
pub fn target_shape_ok(kind: TargetKind, target: &str) -> bool {
    match kind {
        TargetKind::File | TargetKind::Folder => {
            !target.chars().any(char::is_control) && drive_path_ok(target)
        }
        TargetKind::Identifier => check_identifier(target).is_ok_and(|id| id == target),
    }
}

/// A launcher route's target in a saved or imported game, when the launcher isn't known:
/// either shape of [`target_shape_ok`].
pub fn route_target_ok(target: &str) -> bool {
    target_shape_ok(TargetKind::File, target) || target_shape_ok(TargetKind::Identifier, target)
}

/// The target right before a launch: the right shape for the launcher's kind, and an existing
/// file or folder for path kinds.
pub fn check_target(kind: TargetKind, target: &str) -> Result<()> {
    if !target_shape_ok(kind, target) {
        bail!(
            "This game's target doesn't fit the launcher (it expects {}). Add the game again.",
            match kind {
                TargetKind::File => "a file",
                TargetKind::Folder => "a folder",
                TargetKind::Identifier => "an identifier",
            }
        );
    }
    let path = Path::new(target);
    match kind {
        TargetKind::File if !path.is_file() => bail!("The game's file was not found: {target}"),
        TargetKind::Folder if !path.is_dir() => bail!("The game's folder was not found: {target}"),
        _ => Ok(()),
    }
}

/// A launcher's argument template: the emulator template rules for the launcher's executable
/// (see [`crate::launch::check_template`]), and `{target}` is the only placeholder, always as a
/// whole argument (`--start {target}` or `"{target}"`, never `--id={target}`, `-x{target}` or
/// `@{target}`). Braces are refused everywhere else, so no other (or misspelled) placeholder can
/// slip through as literal text. Without `{target}` the target is appended as the last argument.
pub fn check_launcher_template(template: &str, executable: &str) -> Result<()> {
    if template.chars().count() > MAX_TEMPLATE_CHARS {
        bail!("The launcher arguments must be at most {MAX_TEMPLATE_CHARS} characters.");
    }
    check_arguments(
        template,
        executable,
        &[TARGET],
        "launcher",
        r"plugins\settings.ini",
    )?;
    for token in tokenize_arguments(template, "launcher")? {
        if token != TARGET && (token.contains('{') || token.contains('}')) {
            bail!("The launcher argument \"{token}\" is not allowed: {{target}} must be an argument on its own (like --start {{target}}), and no other placeholders exist.");
        }
    }
    Ok(())
}

/// The argv of a launch (program excluded). The template is tokenized before anything is
/// substituted; each token is one argument and each `{target}` token becomes exactly the target,
/// as one argument, whatever it contains. Without a `{target}` token the target is appended.
pub fn build_argv(template: &str, target: &str) -> Result<Vec<String>> {
    let mut argv = Vec::new();
    let mut used = false;
    for token in tokenize_arguments(template, "launcher")? {
        if token == TARGET {
            argv.push(target.to_string());
            used = true;
        } else if token.contains('{') || token.contains('}') {
            bail!("The launcher argument \"{token}\" is not allowed.");
        } else {
            argv.push(token);
        }
    }
    if !used {
        argv.push(target.to_string());
    }
    Ok(argv)
}

/// Every stored field in shape: used for renderer saves (after [`accept_renderer_launcher`]) and
/// backup imports. Returns the normalized launcher (`available` cleared).
pub fn validate(mut l: CustomLauncher) -> Result<CustomLauncher> {
    l.id = check_id(&l.id)?;
    l.name = check_name(&l.name)?;
    check_icon(&l.icon_kind, &l.icon_value)?;
    l.color = check_color(&l.color)?;
    l.executable = l.executable.trim().to_string();
    if l.executable.chars().any(char::is_control) || !executable_path_ok(&l.executable) {
        bail!(
            r"The launcher executable must be a full path to an .exe file on a local drive, like C:\Games\Heroic\Heroic.exe."
        );
    }
    check_launcher_template(&l.args_template, &l.executable)?;
    l.kind()?;
    if let Some(dir) = &l.working_dir {
        if dir.chars().any(char::is_control) || !drive_path_ok(dir) {
            bail!("The launcher's working folder must be a full path on a local drive.");
        }
    }
    if !(0..=1_000_000).contains(&l.sort_order) || l.created_at < 0 {
        bail!("The launcher's order or date is invalid.");
    }
    l.available = false;
    Ok(l)
}

// ---------------------------------------------------------------------------
// Renderer saves

/// What the renderer sends to `launchers_save`: ids, names, colours, icon names and validated
/// text, never a path. Paths come from the latest native picks.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LauncherInput {
    /// Absent for a new launcher.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub icon_kind: String,
    pub icon_value: String,
    pub color: String,
    #[serde(default)]
    pub args_template: String,
    pub target_kind: String,
    /// `executable` (the executable's folder) or `folder` (a picked folder).
    pub working_dir: String,
    /// Use the executable just chosen with `launchers_browse_executable`.
    #[serde(default)]
    pub use_executable_pick: bool,
    /// Use the folder just chosen with `launchers_browse_folder`.
    #[serde(default)]
    pub use_folder_pick: bool,
}

/// What a renderer save may draw on besides its own input.
pub struct SaveContext<'a> {
    pub stored: Option<&'a CustomLauncher>,
    /// The latest executable pick (`PickKind::LauncherExecutable`), if recent.
    pub executable_pick: Option<String>,
    /// The latest folder pick (`PickKind::LauncherFolder`), if recent.
    pub folder_pick: Option<String>,
    /// Whether the stored launcher has games (its target kind is then fixed).
    pub has_games: bool,
    /// Whether an icon file of that name exists in the covers folder.
    pub icon_exists: &'a dyn Fn(&str) -> bool,
}

/// The result of [`accept_renderer_launcher`]: the launcher to store, and which picks it used
/// (to forget them afterwards).
pub struct Accepted {
    pub launcher: CustomLauncher,
    pub used_executable_pick: bool,
    pub used_folder_pick: bool,
}

/// Trust boundary for `launchers_save`. The executable is the latest pick (when asked for and
/// recent) or the stored one; a new launcher needs a pick. The working folder is the
/// executable's folder, the latest folder pick, or the stored folder. A new icon image must be a
/// file [`import_icon`] created. The target kind can't change once the launcher has games.
/// Everything is then checked by [`validate`] (the template against the resulting executable).
pub fn accept_renderer_launcher(input: LauncherInput, ctx: SaveContext<'_>) -> Result<Accepted> {
    let stored = ctx.stored;
    if input.id.is_some() && stored.is_none() {
        bail!("That launcher no longer exists.");
    }
    let (executable, used_executable_pick) = if input.use_executable_pick {
        let pick = ctx.executable_pick.ok_or_else(|| {
            anyhow!("Choose the launcher's executable again with Browse… (the choice expired).")
        })?;
        (pick, true)
    } else {
        match stored {
            Some(s) => (s.executable.clone(), false),
            None => {
                bail!("Choose the launcher's executable with Browse… (it can't be typed in).")
            }
        }
    };
    let (working_dir, used_folder_pick) = match input.working_dir.as_str() {
        "executable" => (None, false),
        "folder" if input.use_folder_pick => {
            let pick = ctx.folder_pick.ok_or_else(|| {
                anyhow!("Choose the working folder again with Browse… (the choice expired).")
            })?;
            (Some(pick), true)
        }
        "folder" => match stored.and_then(|s| s.working_dir.clone()) {
            Some(dir) => (Some(dir), false),
            None => bail!("Choose the working folder with Browse…."),
        },
        _ => bail!("The working folder setting is invalid."),
    };
    let kind = TargetKind::parse(&input.target_kind).ok_or_else(|| {
        anyhow!("Choose what the launcher's games use: a file, a folder or an identifier.")
    })?;
    if let Some(s) = stored {
        if ctx.has_games && s.target_kind != kind.as_str() {
            bail!("This launcher already has games, so what they use (file, folder or identifier) can't change. Create another launcher instead.");
        }
    }
    check_icon(&input.icon_kind, &input.icon_value)?;
    let unchanged_icon =
        stored.is_some_and(|s| s.icon_kind == input.icon_kind && s.icon_value == input.icon_value);
    if input.icon_kind == "image" && !unchanged_icon && !(ctx.icon_exists)(&input.icon_value) {
        bail!("The icon image was not found. Choose it again with \"Use an image…\".");
    }
    let launcher = validate(CustomLauncher {
        id: match stored {
            Some(s) => s.id.clone(),
            None => uuid::Uuid::new_v4().simple().to_string(),
        },
        name: input.name,
        icon_kind: input.icon_kind,
        icon_value: input.icon_value,
        color: input.color,
        executable,
        args_template: input.args_template,
        target_kind: kind.as_str().into(),
        working_dir,
        sort_order: stored.map_or(0, |s| s.sort_order),
        created_at: stored.map_or_else(now_ms, |s| s.created_at),
        available: false,
    })?;
    Ok(Accepted {
        launcher,
        used_executable_pick,
        used_folder_pick,
    })
}

/// Whether a renderer save creates a launcher or changes what its launches run (program,
/// arguments, target kind or working folder). Such a save is confirmed by the user in a native
/// dialog showing [`confirmation_prompt`], so a compromised renderer can't change it alone.
/// Name, icon and colour changes save without asking.
pub fn needs_confirmation(stored: Option<&CustomLauncher>, next: &CustomLauncher) -> bool {
    stored.is_none_or(|s| {
        s.executable != next.executable
            || s.args_template != next.args_template
            || s.target_kind != next.target_kind
            || s.working_dir != next.working_dir
    })
}

/// The native confirmation text: the launcher's name, its program, the exact argument list
/// (one argument per line, `{target}` standing for each game) and the working folder.
pub fn confirmation_prompt(l: &CustomLauncher) -> Result<String> {
    let argv = build_argv(&l.args_template, TARGET)?;
    let lines: Vec<String> = argv
        .iter()
        .enumerate()
        .map(|(i, arg)| format!("  {}. {arg}", i + 1))
        .collect();
    let what = match l.kind()? {
        TargetKind::File => "the game's file",
        TargetKind::Folder => "the game's folder",
        TargetKind::Identifier => "the game's identifier",
    };
    let folder = match &l.working_dir {
        Some(dir) => dir.clone(),
        None => Path::new(&l.executable)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    Ok(format!(
        "Save the launcher \"{}\"?\n\nProgram:\n  {}\n\nArguments (one per line; {TARGET} is {what}):\n{}\n\nWorking folder:\n  {folder}\n\nGame Lobby runs exactly this whenever you play one of this launcher's games. Save it only if you set it up yourself.",
        l.name,
        l.executable,
        lines.join("\n")
    ))
}

/// Copies a user-chosen icon image into `covers_dir` and returns its new bare file name.
pub fn import_icon(covers_dir: &Path, source: &Path) -> Result<String> {
    crate::art::import_image_as(covers_dir, source, &ICON_RULES, |ext| {
        format!("launcher-icon-{}.{ext}", uuid::Uuid::new_v4().simple())
    })
}

/// Whether `name` is an icon file that exists (as a regular file) in `covers_dir`.
pub fn icon_exists(covers_dir: &Path, name: &str) -> bool {
    icon_file_ok(name)
        && std::fs::symlink_metadata(covers_dir.join(name)).is_ok_and(|m| m.is_file())
}

// ---------------------------------------------------------------------------
// Launching

/// Everything a launch needs, checked. There is deliberately no link or shell variant: a
/// launcher is always its own `.exe`, started directly.
#[derive(Debug, PartialEq)]
pub struct LauncherLaunch {
    pub program: PathBuf,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

/// Checks the launcher and the route right before a launch (touches the filesystem): an
/// existing absolute `.exe`, a valid template, a target of the launcher's kind (an existing file
/// or folder for path kinds) and an existing working folder.
pub fn prepare_launch(l: &CustomLauncher, route: &GameRoute) -> Result<LauncherLaunch> {
    if launcher_id_of(route) != Some(l.id.as_str()) {
        bail!("This game belongs to another launcher.");
    }
    if !executable_path_ok(&l.executable) {
        bail!(
            "{}'s executable must be a full path to an .exe file.",
            l.name
        );
    }
    let program = PathBuf::from(&l.executable);
    if !program.is_file() {
        bail!("{}'s executable was not found: {}", l.name, l.executable);
    }
    check_launcher_template(&l.args_template, &l.executable)?;
    let target = route.launch_target.as_str();
    check_target(l.kind()?, target)?;
    let argv = build_argv(&l.args_template, target)?;
    let cwd = match &l.working_dir {
        Some(dir) => {
            if !drive_path_ok(dir) || !Path::new(dir).is_dir() {
                bail!("{}'s working folder was not found: {dir}", l.name);
            }
            PathBuf::from(dir)
        }
        None => program
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("{}'s folder is invalid.", l.name))?,
    };
    Ok(LauncherLaunch { program, argv, cwd })
}

// ---------------------------------------------------------------------------
// Games

/// A new game for a launcher. `target` is a picked path (path kinds) or a typed identifier;
/// `title` may be empty for path targets (the file or folder name is used then).
pub fn new_game(l: &CustomLauncher, title: &str, target: &str) -> Result<Game> {
    let kind = l.kind()?;
    let target = match kind {
        TargetKind::Identifier => check_identifier(target)?,
        _ => {
            if !target_shape_ok(kind, target) {
                bail!("Choose a file or folder on a local drive.");
            }
            target.to_string()
        }
    };
    let path = Path::new(&target);
    let mut title = title.trim().to_string();
    if title.chars().any(char::is_control) {
        bail!("The title can't contain control characters.");
    }
    if title.is_empty() && kind != TargetKind::Identifier {
        let stem = match kind {
            TargetKind::File => path.file_stem(),
            _ => path.file_name(),
        };
        title = stem
            .map(|s| crate::catalog::clean_title(&s.to_string_lossy()))
            .unwrap_or_default();
    }
    if title.is_empty() {
        bail!("Enter a title.");
    }
    if title.chars().count() > 300 {
        bail!("The title must be at most 300 characters.");
    }
    let (app_id, install_path) = match kind {
        TargetKind::Identifier => (None, None),
        TargetKind::File => (
            Some(key_of(path)),
            path.parent().map(|p| p.to_string_lossy().into_owned()),
        ),
        TargetKind::Folder => (Some(key_of(path)), Some(target.clone())),
    };
    Ok(Game {
        id: format!("custom:{}", uuid::Uuid::new_v4()),
        title,
        store: "Custom".into(),
        routes: vec![GameRoute {
            source_id: source_id(&l.id),
            store: "Custom".into(),
            app_id,
            launch_target: target,
            install_path,
            installed: Some(true),
            ..Default::default()
        }],
        ..Default::default()
    })
}

/// What happens to a deleted launcher's games.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveMode {
    /// The games stay (store Custom) without the launcher's route.
    KeepGames,
    /// Games that only had the launcher's route are removed; others just lose that route.
    RemoveGames,
}

impl RemoveMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "keep" => Some(Self::KeepGames),
            "remove" => Some(Self::RemoveGames),
            _ => None,
        }
    }
}

/// The ids of games with a route of launcher `id`.
pub fn games_of(conn: &Connection, id: &str) -> Result<Vec<String>> {
    Ok(db::all_games(conn)?
        .into_iter()
        .filter(|g| g.routes.iter().any(|r| launcher_id_of(r) == Some(id)))
        .map(|g| g.id)
        .collect())
}

/// Deletes a launcher and handles its games per `mode`. `None` when it doesn't exist;
/// otherwise the ids of removed games. Icon files are left on disk (like replaced page art).
pub fn remove(conn: &Connection, id: &str, mode: RemoveMode) -> Result<Option<Vec<String>>> {
    if get(conn, id)?.is_none() {
        return Ok(None);
    }
    let tx = conn.unchecked_transaction()?;
    let mut removed = Vec::new();
    for game in db::all_games(&tx)? {
        if !game.routes.iter().any(|r| launcher_id_of(r) == Some(id)) {
            continue;
        }
        let rest: Vec<GameRoute> = game
            .routes
            .iter()
            .filter(|r| launcher_id_of(r) != Some(id))
            .cloned()
            .collect();
        if mode == RemoveMode::RemoveGames && rest.is_empty() {
            db::delete_game_rows(&tx, &game.id)?;
            removed.push(game.id);
            continue;
        }
        let store = if rest.is_empty() {
            "Custom".to_string()
        } else {
            game.store.clone()
        };
        db::save_patch(
            &tx,
            &GamePatch {
                store: Some(store),
                routes: Some(rest),
                ..GamePatch::new(game.id.clone())
            },
        )?;
    }
    tx.execute("DELETE FROM custom_launchers WHERE id=?", [id])?;
    tx.commit()?;
    Ok(Some(removed))
}

// ---------------------------------------------------------------------------
// Storage

const COLUMNS: &str =
    "id,name,iconKind,iconValue,color,executable,argsTemplate,targetKind,workingDir,sortOrder,createdAt";

fn read_row(row: &Row) -> rusqlite::Result<CustomLauncher> {
    Ok(CustomLauncher {
        id: row.get(0)?,
        name: row.get(1)?,
        icon_kind: row.get(2)?,
        icon_value: row.get(3)?,
        color: row.get(4)?,
        executable: row.get(5)?,
        args_template: row.get(6)?,
        target_kind: row.get(7)?,
        working_dir: row.get::<_, Option<String>>(8)?.filter(|d| !d.is_empty()),
        sort_order: row.get(9)?,
        created_at: row.get(10)?,
        available: false,
    })
}

/// Every launcher in display order. `available` stays false (no filesystem access here):
/// fill it with [`with_status`] after the database lock is released.
pub fn list(conn: &Connection) -> Result<Vec<CustomLauncher>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM custom_launchers ORDER BY sortOrder,name COLLATE NOCASE,createdAt,id"
    ))?;
    let rows = stmt
        .query_map([], read_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<CustomLauncher>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM custom_launchers WHERE id=?"),
            [id],
            read_row,
        )
        .optional()?)
}

/// Inserts or replaces a launcher (already validated).
pub fn save(conn: &Connection, l: &CustomLauncher) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO custom_launchers({COLUMNS}) VALUES(?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name,iconKind=excluded.iconKind,iconValue=excluded.iconValue,color=excluded.color,executable=excluded.executable,argsTemplate=excluded.argsTemplate,targetKind=excluded.targetKind,workingDir=excluded.workingDir,sortOrder=excluded.sortOrder"
        ),
        params![
            l.id,
            l.name,
            l.icon_kind,
            l.icon_value,
            l.color,
            l.executable,
            l.args_template,
            l.target_kind,
            l.working_dir,
            l.sort_order,
            if l.created_at > 0 { l.created_at } else { now_ms() },
        ],
    )?;
    Ok(())
}

/// The icon files stored launchers use (for the covers folder's start-up clean-up, which
/// deletes unreferenced [`icon_file_ok`] files once they are old enough).
pub fn referenced_icons(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("SELECT iconValue FROM custom_launchers WHERE iconKind='image'")?;
    let names = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names)
}

/// The next sort position (after every stored launcher).
pub fn next_sort_order(conn: &Connection) -> Result<i64> {
    let max: Option<i64> =
        conn.query_row("SELECT MAX(sortOrder) FROM custom_launchers", [], |r| {
            r.get(0)
        })?;
    Ok(max.map_or(0, |m| (m + 1).min(1_000_000)))
}

/// Fills `available`. Touches the filesystem: never call it while holding the database lock.
pub fn with_status(list: Vec<CustomLauncher>) -> Vec<CustomLauncher> {
    list.into_iter()
        .map(|mut l| {
            l.available = crate::emulators::executable_ok(&l.executable);
            l
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::localscan::tests::TempDir;

    fn launcher(exe: &str, kind: &str) -> CustomLauncher {
        CustomLauncher {
            id: "heroic".into(),
            name: "Heroic".into(),
            icon_kind: "builtin".into(),
            icon_value: "rocket".into(),
            color: "#4F8CFF".into(),
            executable: exe.into(),
            args_template: "--start {target}".into(),
            target_kind: kind.into(),
            working_dir: None,
            sort_order: 0,
            created_at: 1,
            available: false,
        }
    }

    fn input(kind: &str) -> LauncherInput {
        LauncherInput {
            id: None,
            name: "Heroic".into(),
            icon_kind: "builtin".into(),
            icon_value: "rocket".into(),
            color: "#4f8cff".into(),
            args_template: "--start {target}".into(),
            target_kind: kind.into(),
            working_dir: "executable".into(),
            use_executable_pick: true,
            use_folder_pick: false,
        }
    }

    fn ctx<'a>(
        stored: Option<&'a CustomLauncher>,
        exe: Option<&str>,
        icons: &'a dyn Fn(&str) -> bool,
    ) -> SaveContext<'a> {
        SaveContext {
            stored,
            executable_pick: exe.map(Into::into),
            folder_pick: None,
            has_games: false,
            icon_exists: icons,
        }
    }

    const EXE: &str = r"D:\Launchers\Heroic\Heroic.exe";

    #[test]
    fn the_executable_comes_only_from_a_pick_or_the_stored_value() {
        let none = |_: &str| false;
        // A new launcher needs a pick; without one (or with an expired one) it is refused.
        let mut no_pick = input("identifier");
        no_pick.use_executable_pick = false;
        assert!(accept_renderer_launcher(no_pick.clone(), ctx(None, Some(EXE), &none)).is_err());
        assert!(accept_renderer_launcher(input("identifier"), ctx(None, None, &none)).is_err());
        let accepted =
            accept_renderer_launcher(input("identifier"), ctx(None, Some(EXE), &none)).unwrap();
        assert!(accepted.used_executable_pick);
        assert_eq!(accepted.launcher.executable, EXE);
        assert!(check_id(&accepted.launcher.id).is_ok());
        // An existing launcher keeps its executable unless a pick is used.
        let stored = accepted.launcher;
        let mut edit = input("identifier");
        edit.id = Some(stored.id.clone());
        edit.use_executable_pick = false;
        edit.name = "Heroic Games".into();
        let kept = accept_renderer_launcher(
            edit.clone(),
            ctx(Some(&stored), Some(r"C:\Other\x.exe"), &none),
        )
        .unwrap();
        assert_eq!(kept.launcher.executable, EXE);
        assert_eq!(kept.launcher.id, stored.id);
        assert!(!kept.used_executable_pick);
        // Picks that aren't local .exe files are refused even from the picker.
        for bad in [
            r"\\server\share\x.exe",
            r"D:\Launchers\run.bat",
            r"D:\Launchers\run.cmd",
            r"D:\Launchers\run.ps1",
            "x.exe",
            "steam://run/1",
            r"C:x.exe",
        ] {
            assert!(
                accept_renderer_launcher(input("identifier"), ctx(None, Some(bad), &none)).is_err(),
                "{bad}"
            );
        }
        // An id that doesn't exist can't be "edited" into existence.
        assert!(accept_renderer_launcher(edit, ctx(None, Some(EXE), &none)).is_err());
        // The renderer can't send a path: unknown fields are refused.
        let raw = serde_json::json!({
            "name": "x", "iconKind": "builtin", "iconValue": "rocket", "color": "#000000",
            "targetKind": "file", "workingDir": "executable", "executable": EXE
        });
        assert!(serde_json::from_value::<LauncherInput>(raw).is_err());
    }

    #[test]
    fn working_folders_icons_and_target_kinds_follow_the_rules() {
        let none = |_: &str| false;
        let mut folder = input("file");
        folder.working_dir = "folder".into();
        assert!(accept_renderer_launcher(folder.clone(), ctx(None, Some(EXE), &none)).is_err());
        folder.use_folder_pick = true;
        let mut c = ctx(None, Some(EXE), &none);
        c.folder_pick = Some(r"D:\Mods".into());
        let saved = accept_renderer_launcher(folder, c).unwrap().launcher;
        assert_eq!(saved.working_dir.as_deref(), Some(r"D:\Mods"));
        let mut bad_dir = input("file");
        bad_dir.working_dir = "C:\\Windows".into();
        assert!(accept_renderer_launcher(bad_dir, ctx(None, Some(EXE), &none)).is_err());

        // Icons: built-in names, or an imported file that exists.
        let mut icon = input("file");
        icon.icon_value = "not-an-icon".into();
        assert!(accept_renderer_launcher(icon.clone(), ctx(None, Some(EXE), &none)).is_err());
        icon.icon_kind = "image".into();
        icon.icon_value = format!("launcher-icon-{}.png", "a".repeat(32));
        assert!(accept_renderer_launcher(icon.clone(), ctx(None, Some(EXE), &none)).is_err());
        let all = |_: &str| true;
        assert!(accept_renderer_launcher(icon.clone(), ctx(None, Some(EXE), &all)).is_ok());
        for bad in [
            "../library.sqlite".to_string(),
            "hero-g-1.png".to_string(),
            "launcher-icon-x.svg".to_string(),
            format!("launcher-icon-{}.svg", "a".repeat(32)),
            format!("launcher-icon-{}.png\\..\\x", "a".repeat(32)),
            format!("LAUNCHER-ICON-{}.png", "a".repeat(32)),
        ] {
            icon.icon_value = bad.clone();
            assert!(
                accept_renderer_launcher(icon.clone(), ctx(None, Some(EXE), &all)).is_err(),
                "{bad}"
            );
        }

        // The target kind is fixed once the launcher has games.
        let stored = validate(launcher(EXE, "identifier")).unwrap();
        let mut change = input("file");
        change.id = Some(stored.id.clone());
        change.use_executable_pick = false;
        let mut c = ctx(Some(&stored), None, &none);
        c.has_games = true;
        assert!(accept_renderer_launcher(change.clone(), c).is_err());
        assert!(accept_renderer_launcher(change, ctx(Some(&stored), None, &none)).is_ok());
        let mut bad_kind = input("url");
        bad_kind.use_executable_pick = true;
        assert!(accept_renderer_launcher(bad_kind, ctx(None, Some(EXE), &none)).is_err());
    }

    #[test]
    fn colours_must_be_six_digit_hex() {
        assert_eq!(check_color("#4F8CFF").unwrap(), "#4f8cff");
        assert_eq!(check_color("#000000").unwrap(), "#000000");
        for bad in [
            "",
            "#fff",
            "#ffff",
            "#4f8cff00",
            "4f8cff",
            "#4f8cfg",
            "red",
            "#4f8cff;background:url(x)",
            " #4f8cff",
            "#４f8cff",
        ] {
            assert!(check_color(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_ids_and_icons_are_checked() {
        assert_eq!(check_name("  Heroic  ").unwrap(), "Heroic");
        assert!(check_name("").is_err());
        assert!(check_name("a\nb").is_err());
        assert!(check_name(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(check_id("abc-123_X").is_ok());
        let long = "x".repeat(65);
        for bad in ["", "a b", "a/b", "a:b", long.as_str()] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
        for name in BUILTIN_ICONS {
            assert!(check_icon("builtin", name).is_ok());
        }
        assert!(check_icon("builtin", "skull-and-bones").is_err());
        assert!(check_icon("svg", "rocket").is_err());
        assert!(check_icon("image", "rocket").is_err());
    }

    #[test]
    fn identifiers_are_short_plain_and_never_options_or_paths() {
        for (raw, want) in [
            (
                "1a2b3c4d-0000-1111-2222-333344445555",
                "1a2b3c4d-0000-1111-2222-333344445555",
            ),
            ("  Hollow Knight  ", "Hollow Knight"),
            ("legendary:Fortnite@main#1", "legendary:Fortnite@main#1"),
            (
                "Tom and Jerry (1999), 'Gold'+!",
                "Tom and Jerry (1999), 'Gold'+!",
            ),
            ("a.b_c", "a.b_c"),
            ("x-", "x-"),
            ("user@host", "user@host"),
            ("Game: Subtitle", "Game: Subtitle"),
            ("1:2", "1:2"),
        ] {
            assert_eq!(check_identifier(raw).unwrap(), want, "{raw}");
        }
        for bad in [
            "",
            "   ",
            "-fullscreen",
            "--exec=calc",
            " -x",
            "/c",
            "/S",
            "a/b",
            r"a\b",
            r"\\server\share",
            r"C:\Windows\System32\calc.exe",
            "..",
            "a..b",
            "game\n--x",
            "game\t",
            "game\r",
            "\u{0}",
            "a\"b",
            "a;b",
            "a|b",
            "a<b",
            "a>b",
            "a%PATH%",
            "a^b",
            "a`b",
            "a$b",
            "a*b",
            "a?b",
            "a=b",
            "a{target}",
            // `&` is not part of the set; `@` can't start one (a response file to many
            // programs); a drive letter and colon would be a drive-relative path.
            "Tom & Jerry",
            "&",
            "@args.txt",
            " @x",
            "@",
            "C:evil.exe",
            "c:",
            "Z:x y",
            // Look-alikes of \ / : " . that best-fit to the real ones.
            "a\u{FF3C}b",
            "a\u{2215}b",
            "a\u{FF0F}b",
            "C\u{FF1A}x",
            "a\u{FF0E}\u{FF0E}b",
            "\u{FF0D}x",
            "Pokémon",
        ] {
            assert!(check_identifier(bad).is_err(), "{bad:?}");
        }
        assert!(check_identifier(&"a".repeat(MAX_IDENTIFIER_CHARS)).is_ok());
        assert!(check_identifier(&"a".repeat(MAX_IDENTIFIER_CHARS + 1)).is_err());
    }

    #[test]
    fn options_that_run_other_programs_are_refused_in_launcher_templates() {
        for bad in crate::launch::tests::DEBUGGING_TEMPLATES {
            let template = bad.replace("{rom}", TARGET);
            let e = check_launcher_template(&template, EXE)
                .unwrap_err()
                .to_string();
            assert!(e.contains("not allowed"), "{template}: {e}");
            assert!(!e.contains("emulator"), "{e}");
        }
        assert!(check_launcher_template("--start {target}", EXE).is_ok());
        let e = check_launcher_template("\"{target}", EXE)
            .unwrap_err()
            .to_string();
        assert!(e.contains("launcher") && !e.contains("emulator"), "{e}");
    }

    #[test]
    fn launch_changes_are_confirmed_and_cosmetic_ones_are_not() {
        let stored = validate(launcher(EXE, "identifier")).unwrap();
        assert!(needs_confirmation(None, &stored), "a new launcher");
        let mut same = stored.clone();
        assert!(!needs_confirmation(Some(&stored), &same), "unchanged");
        same.name = "Heroic Games".into();
        same.color = "#000000".into();
        same.icon_value = "star".into();
        same.sort_order = 3;
        assert!(
            !needs_confirmation(Some(&stored), &same),
            "name, icon, colour"
        );
        let changes: [fn(&mut CustomLauncher); 4] = [
            |l: &mut CustomLauncher| l.executable = r"D:\Other\x.exe".into(),
            |l: &mut CustomLauncher| l.args_template = "--run {target}".into(),
            |l: &mut CustomLauncher| l.target_kind = "file".into(),
            |l: &mut CustomLauncher| l.working_dir = Some(r"D:\Mods".into()),
        ];
        for change in changes {
            let mut next = stored.clone();
            change(&mut next);
            assert!(needs_confirmation(Some(&stored), &next));
        }

        let mut shown = stored.clone();
        shown.args_template = r#"-p "My Profile" {target} --fullscreen"#.into();
        let text = confirmation_prompt(&shown).unwrap();
        assert!(text.contains("\"Heroic\""), "{text}");
        assert!(text.contains(EXE), "{text}");
        assert!(
            text.contains("  1. -p\n  2. My Profile\n  3. {target}\n  4. --fullscreen"),
            "{text}"
        );
        assert!(text.contains("{target} is the game's identifier"), "{text}");
        assert!(
            text.contains(r"D:\Launchers\Heroic"),
            "working folder: {text}"
        );
        // Without {target}, the appended target is shown last.
        shown.args_template = "--silent".into();
        shown.working_dir = Some(r"D:\Mods".into());
        let text = confirmation_prompt(&shown).unwrap();
        assert!(text.contains("  1. --silent\n  2. {target}"), "{text}");
        assert!(text.contains(r"  D:\Mods"), "{text}");
    }

    #[test]
    fn bad_templates_are_refused_with_the_emulator_rules() {
        let exe = r"D:\Launchers\Heroic\Heroic.exe";
        for good in [
            "",
            "{target}",
            "--start {target}",
            "\"{target}\" --fullscreen",
            "/silent {target}",
            r"--config config\heroic.json {target}",
            r"--config D:\Launchers\Heroic\config.json {target}",
            "-p Default {target} {target}",
            "--set-volume=50 {target}",
        ] {
            assert!(check_launcher_template(good, exe).is_ok(), "{good}");
        }
        for bad in [
            // The emulator cases, with {target}.
            r"-L C:\Users\Me\Downloads\evil.dll {target}",
            r"-L D:\Launchers\Other\x.dll {target}",
            r"-L D:\Launchers\Heroic\..\Other\x.dll {target}",
            r"-L ..\evil.dll {target}",
            r"-L cores\..\..\evil.dll {target}",
            r"-L \evil.dll {target}",
            "-L /Users/me/evil.dll {target}",
            r"-L \\attacker\share\evil.dll {target}",
            "-L //attacker/share/evil.dll {target}",
            "-L C:evil.dll {target}",
            r"-LC:\evil.dll {target}",
            r"--core=C:\evil.dll {target}",
            "--script=https://evil.example/x.lua {target}",
            "{target} & calc.exe",
            "{target} | calc",
            "{target} > out.txt",
            "%COMSPEC% /c calc",
            "^& calc",
            "`calc`",
            "{target}\ncalc.exe",
            "{target}\tx",
            "-L /evil.dll {target}",
            "-L C\u{FF1A}\u{FF3C}Users\u{FF3C}Me\u{FF3C}evil.dll {target}",
            "-L \u{2215}Users\u{2215}evil.dll {target}",
            "\u{FF02}-L C:\\x.dll\u{FF02} {target}",
            "\"{target}",
            // Glued, misspelled or foreign placeholders.
            "-x{target}",
            "/x{target}",
            "@{target}",
            "--id={target}",
            "moshortcut://{target}",
            "{target}.json",
            "{rom}",
            "{romdir}",
            "{Target}",
            "{target",
            "target}",
            "{{target}}",
            "{}",
        ] {
            assert!(
                check_launcher_template(bad, exe).is_err(),
                "{bad} should be refused"
            );
        }
        assert!(check_launcher_template(&"a ".repeat(1_500), exe).is_err());
        assert!(check_launcher_template("{target}", "heroic.exe").is_err());
    }

    #[test]
    fn the_target_is_exactly_one_argument() {
        for target in [
            r"D:\Games\Simple",
            r"D:\My Games\Game (USA) [!]",
            r#"D:\Games\Quote"d & more"#,
            r"D:\Games\100% & ^done %PATH%",
            r#"D:\Games\" --evil "x"#,
            r"D:\Games\{target}",
            "Tom & Jerry (1999)",
            "a b c",
        ] {
            assert_eq!(
                build_argv("--start {target} -x", target).unwrap(),
                ["--start", target, "-x"],
                "{target}"
            );
            assert_eq!(
                build_argv("--fullscreen", target).unwrap(),
                ["--fullscreen", target]
            );
            assert_eq!(build_argv("", target).unwrap(), [target]);
            assert_eq!(build_argv("\"{target}\"", target).unwrap(), [target]);
            assert_eq!(
                build_argv("{target} {target}", target).unwrap(),
                [target, target]
            );
        }
        assert_eq!(
            build_argv(r#"-p "My Profile" {target}"#, "x").unwrap(),
            ["-p", "My Profile", "x"]
        );
        // A template that slipped past the checks still can't glue the target.
        assert!(build_argv("--id={target}", "x").is_err());
        assert!(build_argv("\"{target}", "x").is_err());
    }

    #[test]
    fn launches_run_the_exe_directly_with_a_checked_target() {
        let dir = TempDir::new("launcher-launch");
        let exe = dir.file("Heroic/Heroic.exe");
        let game_file = dir.file("Games/Hollow Knight/hk & x.json");
        let game_folder = game_file.parent().unwrap().to_path_buf();
        let exe_text = exe.to_string_lossy().into_owned();
        let route = |target: &str| GameRoute {
            source_id: source_id("heroic"),
            store: "Custom".into(),
            launch_target: target.into(),
            installed: Some(true),
            ..Default::default()
        };
        let file_target = game_file.to_string_lossy().into_owned();
        let l = launcher(&exe_text, "file");
        let launch = prepare_launch(&l, &route(&file_target)).unwrap();
        assert_eq!(launch.program, exe);
        assert_eq!(launch.argv, ["--start".to_string(), file_target.clone()]);
        assert_eq!(launch.cwd, exe.parent().unwrap());

        let folder = launcher(&exe_text, "folder");
        let folder_target = game_folder.to_string_lossy().into_owned();
        assert!(prepare_launch(&folder, &route(&folder_target)).is_ok());
        assert!(
            prepare_launch(&folder, &route(&file_target)).is_err(),
            "a file is not a folder"
        );
        assert!(
            prepare_launch(&l, &route(&folder_target)).is_err(),
            "a folder is not a file"
        );

        let ident = launcher(&exe_text, "identifier");
        assert_eq!(
            prepare_launch(&ident, &route("Hollow Knight"))
                .unwrap()
                .argv,
            ["--start", "Hollow Knight"]
        );
        for bad in ["-x", "/c", r"..\x", file_target.as_str(), "steam://run/1"] {
            assert!(prepare_launch(&ident, &route(bad)).is_err(), "{bad}");
        }
        // Missing files, other launchers' routes, scripts and stale templates are refused.
        let missing = dir
            .path()
            .join("Games/gone.json")
            .to_string_lossy()
            .into_owned();
        assert!(prepare_launch(&l, &route(&missing)).is_err());
        let mut other = route(&file_target);
        other.source_id = source_id("playnite");
        assert!(prepare_launch(&l, &other).is_err());
        for script in ["Heroic/run.bat", "Heroic/run.cmd", "Heroic/run.ps1"] {
            let path = dir.file(script).to_string_lossy().into_owned();
            assert!(prepare_launch(&launcher(&path, "file"), &route(&file_target)).is_err());
        }
        let gone = dir.path().join("Gone/x.exe").to_string_lossy().into_owned();
        assert!(prepare_launch(&launcher(&gone, "file"), &route(&file_target)).is_err());
        let mut stale = l.clone();
        stale.args_template = "{target} & calc".into();
        assert!(prepare_launch(&stale, &route(&file_target)).is_err());
        // The working folder: the exe's folder, or an existing chosen folder.
        let mut custom_dir = l.clone();
        custom_dir.working_dir = Some(game_folder.to_string_lossy().into_owned());
        assert_eq!(
            prepare_launch(&custom_dir, &route(&file_target))
                .unwrap()
                .cwd,
            game_folder
        );
        custom_dir.working_dir = Some(dir.path().join("nope").to_string_lossy().into_owned());
        assert!(prepare_launch(&custom_dir, &route(&file_target)).is_err());
    }

    #[test]
    fn launch_dispatch_never_uses_a_shell() {
        // The launcher branch of `launch_game` starts the program directly and returns before
        // any link, shortcut or shell path is reached.
        let source = include_str!("launch.rs");
        let start = source
            .find("if let Some(launcher_id) = launchers::launcher_id_of(route)")
            .expect("launcher branch");
        let branch = &source[start..];
        let branch = &branch[..branch.find("if is_rom_route(route)").expect("next branch")];
        assert!(branch.contains("launchers::prepare_launch"));
        assert!(branch.contains("spawn_detached_in(&launch.program, &launch.argv"));
        assert!(branch.contains("return track_started_process"));
        for forbidden in [
            "shell_open",
            "spawn_detached(",
            "cmd.exe",
            "explorer",
            "Command::new",
        ] {
            assert!(!branch.contains(forbidden), "{forbidden}");
        }
        // The launcher branch comes first, so a launcher route is never treated as a ROM, local
        // or store route.
        assert!(start < source.find("if is_rom_route(route) {").unwrap());
    }

    fn library_with(games: &[Game]) -> Db {
        let db = Db::memory().unwrap();
        {
            let conn = db.lock();
            save(&conn, &validate(launcher(EXE, "identifier")).unwrap()).unwrap();
            for g in games {
                db::save_game(&conn, &GamePatch::from(g)).unwrap();
            }
        }
        db
    }

    fn steam_route() -> GameRoute {
        GameRoute {
            source_id: "steam".into(),
            store: "Steam".into(),
            app_id: Some("400".into()),
            launch_target: "steam://rungameid/400".into(),
            installed: Some(true),
            ..Default::default()
        }
    }

    fn launcher_games() -> Vec<Game> {
        let l = validate(launcher(EXE, "identifier")).unwrap();
        let only = Game {
            id: "only".into(),
            ..new_game(&l, "Only Heroic", "hk").unwrap()
        };
        let mut both = Game {
            id: "both".into(),
            store: "Steam".into(),
            ..new_game(&l, "Portal", "portal").unwrap()
        };
        both.routes.push(steam_route());
        let other = Game {
            id: "other".into(),
            title: "Other".into(),
            store: "Steam".into(),
            routes: vec![steam_route()],
            ..Default::default()
        };
        vec![only, both, other]
    }

    #[test]
    fn deleting_a_launcher_keeps_or_removes_its_games() {
        // Keep: the games stay, without the launcher's route.
        let db = library_with(&launcher_games());
        {
            let conn = db.lock();
            assert_eq!(games_of(&conn, "heroic").unwrap().len(), 2);
            assert_eq!(
                remove(&conn, "heroic", RemoveMode::KeepGames).unwrap(),
                Some(vec![])
            );
            assert!(get(&conn, "heroic").unwrap().is_none());
            let only = db::get_game(&conn, "only").unwrap().unwrap();
            assert!(only.routes.is_empty());
            assert_eq!(only.store, "Custom");
            let both = db::get_game(&conn, "both").unwrap().unwrap();
            assert_eq!(both.routes, [steam_route()]);
            assert_eq!(both.store, "Steam");
            assert_eq!(
                db::get_game(&conn, "other").unwrap().unwrap().routes.len(),
                1
            );
            assert_eq!(
                remove(&conn, "heroic", RemoveMode::KeepGames).unwrap(),
                None
            );
        }
        // Remove: games that only had the launcher go; others lose its route.
        let db = library_with(&launcher_games());
        let conn = db.lock();
        assert_eq!(
            remove(&conn, "heroic", RemoveMode::RemoveGames).unwrap(),
            Some(vec!["only".to_string()])
        );
        assert!(db::get_game(&conn, "only").unwrap().is_none());
        assert_eq!(
            db::get_game(&conn, "both").unwrap().unwrap().routes,
            [steam_route()]
        );
        assert!(db::get_game(&conn, "other").unwrap().is_some());
        assert!(list(&conn).unwrap().is_empty());
        assert_eq!(RemoveMode::parse("keep"), Some(RemoveMode::KeepGames));
        assert_eq!(RemoveMode::parse("remove"), Some(RemoveMode::RemoveGames));
        assert_eq!(RemoveMode::parse("delete"), None);
    }

    #[test]
    fn backups_round_trip_launchers_and_check_them_again() {
        let db = library_with(&launcher_games());
        let backup = {
            let conn = db.lock();
            crate::types::Backup {
                games: db::all_games(&conn).unwrap(),
                settings: None,
                emulators: None,
                game_page_custom: None,
                game_notes: None,
                game_added: None,
                play_sessions: None,
                launchers: Some(list(&conn).unwrap()),
            }
        };
        let json = serde_json::to_value(&backup).unwrap();
        assert!(json["customLaunchers"].is_array());
        let parsed = crate::validation::backup(&json).unwrap();
        let restored = Db::memory().unwrap();
        db::restore_backup(&mut restored.lock(), &parsed).unwrap();
        let conn = restored.lock();
        assert_eq!(list(&conn).unwrap(), backup.launchers.clone().unwrap());
        assert_eq!(games_of(&conn, "heroic").unwrap().len(), 2);
        drop(conn);

        // Older backups have no launchers.
        let mut old = json.clone();
        old.as_object_mut().unwrap().remove("customLaunchers");
        assert!(crate::validation::backup(&old).unwrap().launchers.is_none());

        let with = |field: &str, value: serde_json::Value| {
            let mut v = json.clone();
            v["customLaunchers"][0][field] = value;
            v
        };
        let icon_name = format!("launcher-icon-{}.png", "b".repeat(32));
        assert!(crate::validation::backup(&with("iconKind", "image".into())).is_err());
        let mut image = with("iconKind", "image".into());
        image["customLaunchers"][0]["iconValue"] = icon_name.clone().into();
        assert!(
            crate::validation::backup(&image).is_ok(),
            "icons by name only"
        );
        for (field, value) in [
            ("executable", r"\\server\share\Heroic.exe"),
            ("executable", r"D:\Launchers\run.bat"),
            ("executable", r"D:\Launchers\run.cmd"),
            ("executable", "Heroic.exe"),
            ("executable", "steam://run/1"),
            ("argsTemplate", "{target} & calc"),
            ("argsTemplate", r"-L C:\evil.dll {target}"),
            ("argsTemplate", "--id={target}"),
            ("iconValue", "../library.sqlite"),
            ("color", "red"),
            ("targetKind", "url"),
            ("workingDir", r"\\server\share"),
            ("id", "../x"),
            ("name", ""),
        ] {
            assert!(
                crate::validation::backup(&with(field, value.into())).is_err(),
                "{field} = {value}"
            );
        }
        assert!(crate::validation::backup(&with("script", "x".into())).is_err());
        let mut twice = json.clone();
        let first = twice["customLaunchers"][0].clone();
        twice["customLaunchers"].as_array_mut().unwrap().push(first);
        assert!(crate::validation::backup(&twice).is_err());
        // Games' launcher routes are checked too.
        for bad_target in ["-x", "/c", r"..\..\x", "a\nb"] {
            let mut v = json.clone();
            let games = v["games"].as_array_mut().unwrap();
            let game = games.iter_mut().find(|g| g["id"] == "only").unwrap();
            game["routes"][0]["launchTarget"] = bad_target.into();
            assert!(crate::validation::backup(&v).is_err(), "{bad_target}");
        }
        let mut v = json.clone();
        let games = v["games"].as_array_mut().unwrap();
        let game = games.iter_mut().find(|g| g["id"] == "only").unwrap();
        game["routes"][0]["sourceId"] = "launcher:../x".into();
        assert!(crate::validation::backup(&v).is_err());
    }

    #[test]
    fn the_renderer_cannot_create_or_change_launcher_routes() {
        let l = validate(launcher(EXE, "identifier")).unwrap();
        let stored = new_game(&l, "Hollow Knight", "hk").unwrap().routes;
        assert!(crate::validation::check_renderer_routes(&stored, &stored).is_ok());
        let mut changed = stored.clone();
        changed[0].launch_target = "other".into();
        assert!(crate::validation::check_renderer_routes(&stored, &changed).is_err());
        // Even a valid store link can't be given a launcher source.
        let mut link = stored.clone();
        link[0].launch_target = "steam://run/400".into();
        link[0].app_id = None;
        assert!(crate::validation::check_renderer_routes(&[], &link).is_err());
        assert!(crate::validation::check_renderer_routes(&[], &stored).is_err());

        // The prefix is exact: other spellings are refused, not treated as another source.
        let patch = |source: &str| {
            crate::validation::game_patch(&serde_json::json!({
                "id": "g",
                "routes": [{ "sourceId": source, "store": "Custom", "launchTarget": "hk" }]
            }))
        };
        assert!(patch("launcher:heroic").is_ok());
        for bad in [
            "LAUNCHER:heroic",
            "Launcher:heroic",
            "launcheR:x",
            "LAUNCHER:",
        ] {
            assert!(patch(bad).is_err(), "{bad}");
        }
        assert!(patch("launch").is_ok(), "an unrelated short source");
    }

    #[test]
    fn merge_health_and_storage_handle_launcher_routes() {
        let dir = TempDir::new("launcher-routes");
        let file = dir.file("Games/HK/hk.json");
        let folder = file.parent().unwrap().to_path_buf();
        let file_text = file.to_string_lossy().into_owned();
        let folder_text = folder.to_string_lossy().into_owned();
        let mut files = validate(launcher(EXE, "file")).unwrap();
        files.id = "files".into();
        let mut folders = validate(launcher(EXE, "folder")).unwrap();
        folders.id = "folders".into();
        let ident = validate(launcher(EXE, "identifier")).unwrap();

        // Path targets record their folder (sizes, install folder), identifiers don't.
        let by_file = new_game(&files, "", &file_text).unwrap();
        assert_eq!(by_file.title, "hk");
        assert_eq!(
            by_file.routes[0].install_path.as_deref(),
            Some(folder_text.as_str())
        );
        assert_eq!(by_file.routes[0].app_id, Some(key_of(&file)));
        let by_folder = new_game(&folders, "", &folder_text).unwrap();
        assert_eq!(by_folder.title, "HK");
        assert_eq!(
            by_folder.routes[0].install_path.as_deref(),
            Some(folder_text.as_str())
        );
        let by_id = new_game(&ident, "Hollow Knight", "hk").unwrap();
        assert_eq!(by_id.routes[0].install_path, None);
        assert!(
            new_game(&ident, "", "hk").is_err(),
            "identifiers need a title"
        );
        assert!(new_game(&ident, "x", "-x").is_err());
        assert!(new_game(&files, "x", r"\\server\share\x.json").is_err());

        // Health: a missing file or folder target is reported; identifiers never are.
        use crate::health::{find_missing, subject_of};
        assert!(find_missing(&[by_file.clone(), by_folder.clone(), by_id.clone()]).is_empty());
        assert!(!subject_of(&by_file.routes[0]).unwrap().folder);
        assert!(subject_of(&by_folder.routes[0]).unwrap().folder);
        assert!(subject_of(&by_id.routes[0]).is_none());
        std::fs::remove_file(&file).unwrap();
        let missing = find_missing(&[by_file.clone(), by_folder.clone(), by_id.clone()]);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].subject.path, file_text);
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(find_missing(&[by_file.clone(), by_folder.clone()]).len(), 2);

        // Storage: path targets are measured under their folder.
        let db = Db::memory().unwrap();
        let conn = db.lock();
        for game in [&by_file, &by_folder, &by_id] {
            db::save_game(&conn, &GamePatch::from(game)).unwrap();
        }
        let report = crate::storage::report(&conn).unwrap();
        let path_of = |id: &str| {
            report
                .entries
                .iter()
                .find(|e| e.game_id == id)
                .and_then(|e| e.install_path.clone())
        };
        assert_eq!(path_of(&by_file.id).as_deref(), Some(folder_text.as_str()));
        assert_eq!(
            path_of(&by_folder.id).as_deref(),
            Some(folder_text.as_str())
        );
        assert_eq!(path_of(&by_id.id), None);
        drop(conn);

        // Merge: launcher routes move over and keep their launcher.
        let mut conn = db.lock();
        let games = db::merge_games(&mut conn, &by_id.id, &by_file.id).unwrap();
        let merged = games.iter().find(|g| g.id == by_id.id).unwrap();
        let sources: Vec<&str> = merged.routes.iter().map(|r| r.source_id.as_str()).collect();
        assert_eq!(sources, ["launcher:heroic", "launcher:files"]);
        assert!(games.iter().all(|g| g.id != by_file.id));
        // The same target added twice is one route (imports reuse the game).
        let again = new_game(&ident, "Hollow Knight", "hk").unwrap();
        db::upsert_imported_games(&mut conn, std::slice::from_ref(&again)).unwrap();
        let all = db::all_games(&conn).unwrap();
        assert!(
            all.iter().all(|g| g.id != again.id),
            "the existing game is reused"
        );
    }

    #[test]
    fn unused_icon_files_are_cleaned_up_at_start_up_once_old() {
        let covers = TempDir::new("launcher-icon-sweep");
        let db = Db::memory().unwrap();
        let name = |c: char| format!("launcher-icon-{}.png", c.to_string().repeat(32));
        let (used, orphan, fresh) = (name('a'), name('b'), name('c'));
        let mut l = validate(launcher(EXE, "identifier")).unwrap();
        l.icon_kind = "image".into();
        l.icon_value = used.clone();
        save(&db.lock(), &l).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
        for file in [&used, &orphan, &fresh] {
            covers.write(file, b"\x89PNG\r\n\x1a\n");
        }
        for file in [used.as_str(), orphan.as_str(), "hero-g-1.png"] {
            let path = covers.path().join(file);
            if !path.exists() {
                covers.write(file, b"x");
            }
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        // The cover cache's own pattern never claims launcher icons.
        for file in [&used, &orphan, &fresh] {
            assert!(!crate::covercache::is_cache_file_name(file), "{file}");
        }
        assert_eq!(referenced_icons(&db.lock()).unwrap().len(), 1);
        crate::covercache::sweep_files(&db.lock(), covers.path()).unwrap();
        assert!(covers.path().join(&used).exists(), "still used");
        assert!(!covers.path().join(&orphan).exists(), "unused and old");
        assert!(
            covers.path().join(&fresh).exists(),
            "just chosen, not saved yet"
        );
        assert!(
            covers.path().join("hero-g-1.png").exists(),
            "other covers are left alone"
        );
    }

    #[test]
    fn icon_images_are_checked_by_content_and_never_svg() {
        let src = TempDir::new("launcher-icon-src");
        let covers = TempDir::new("launcher-icon-covers");
        let png = src.write("Icon.PNG", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        let name = import_icon(covers.path(), &png).unwrap();
        assert!(icon_file_ok(&name), "{name}");
        assert!(name.ends_with(".png"));
        assert!(icon_exists(covers.path(), &name));
        assert!(!icon_exists(
            covers.path(),
            &format!("launcher-icon-{}.png", "0".repeat(32))
        ));
        let gif = src.write("a.gif", b"GIF89a   ;");
        assert!(import_icon(covers.path(), &gif).unwrap().ends_with(".gif"));
        let svg = src.write("icon.svg", br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#);
        let svg_as_png = src.write("icon2.png", br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#);
        let text = src.write("notes.txt", b"\x89PNG\r\n\x1a\n");
        let html = src.write("x.webp", b"<html></html>");
        // AVIF is fine for page art, but not one of the icon formats.
        let avif = src.write("x.png", b"\0\0\0\x1cftypavif\0\0\0\0");
        let big = src.path().join("big.png");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_ICON_BYTES + 1)
            .unwrap();
        for bad in [&svg, &svg_as_png, &text, &html, &avif, &big] {
            assert!(
                import_icon(covers.path(), bad).is_err(),
                "{}",
                bad.display()
            );
        }
        assert_eq!(std::fs::read_dir(covers.path()).unwrap().count(), 2);
    }
}
