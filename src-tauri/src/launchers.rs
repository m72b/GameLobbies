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
//! - Targets, per launcher: a set of allowed [`TargetKind`]s (at least one). Each game's target
//!   is a file or a folder chosen in a native picker by Rust (`launchers_add_game`) or found by a
//!   scan of one of the launcher's games folders (`local::scan_roots`), or a short identifier
//!   typed by the user ([`check_identifier`]). Checked again at every launch against the
//!   launcher's allowed kinds ([`check_target_any`]).
//! - Program (optional): a launcher without one is a label-only "app folder" ([`is_label_only`]).
//!   Its games are found in its games folders and run directly: they get the scan's ordinary
//!   local and ROM routes, which carry `launcherId` only as a label. A `launcher:<id>` route of a
//!   label-only launcher never runs ([`prepare_launch`]). The program can't be added or removed
//!   while the launcher has games or games folders, so existing routes never change meaning.
//! - Working folder: the executable's folder, or a folder chosen in a native picker
//!   ([`crate::picks::PickKind::LauncherFolder`]).
//! - Icon: one of [`BUILTIN_ICONS`] (stored by name) or an image the user chose, copied into the
//!   covers folder by [`import_icon`] (magic-byte check, size cap, PNG/JPEG/WebP/GIF, never SVG)
//!   and referenced by its bare file name only ([`icon_file_ok`]).
//! - Colour: `#rrggbb` ([`check_color`]).
//! - Assignments ([`assign`]): a game already in the library can be given to any launcher from
//!   its page. The renderer names only the game, the launcher and which of the game's routes and
//!   kinds to use; the target is taken from that route ([`assign_candidates`]). File and folder
//!   targets come from stored routes. An identifier target is a route's `appId`, which the
//!   renderer can set on link routes, but it is constrained by `check_identifier` and passed
//!   as one argv element without a shell. A launcher with a program gets a new
//!   `launcher:<id>` route (shown in a native confirmation first, checked like any launch); a
//!   label-only launcher only labels a route. Both are marked [`GameRoute::assigned`], kept by
//!   scans, and removed exactly by [`unassign`].
//! - Launch options: a launcher route can have its own options (`links::launcher_host`), added
//!   after this argv, with the template's path rules for the launcher's folder and a native
//!   confirmation of the whole command line (on the game page, or in the same dialog as an
//!   assignment, [`assign_prompt`]).
//! - Launching: when the launcher's program isn't running ([`program_running`], by exact
//!   path), it is started alone first (no arguments, no launch options) and the launch waits for
//!   the user (see `pending_launch`).

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

    /// Canonical order of a kind set.
    const ORDER: [Self; 3] = [Self::File, Self::Folder, Self::Identifier];

    fn noun(self) -> &'static str {
        match self {
            Self::File => "a file",
            Self::Folder => "a folder",
            Self::Identifier => "an identifier",
        }
    }
}

/// The target kinds as the confirmation dialogs name them, joined naturally: "file",
/// "file or folder", "file, folder or store id".
fn kinds_text(kinds: &[TargetKind]) -> String {
    let nouns: Vec<&str> = kinds
        .iter()
        .map(|k| match k {
            TargetKind::File => "file",
            TargetKind::Folder => "folder",
            TargetKind::Identifier => "store id",
        })
        .collect();
    match nouns.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_string(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    }
}

/// A kind set from its names: known names only, at least one, de-duplicated, in canonical order
/// (file, folder, identifier).
pub fn parse_kinds(raw: &[String]) -> Result<Vec<TargetKind>> {
    let mut parsed = Vec::new();
    for name in raw {
        let kind = TargetKind::parse(name).ok_or_else(|| {
            anyhow!("Choose what the launcher's games use: files, folders or identifiers.")
        })?;
        parsed.push(kind);
    }
    let kinds: Vec<TargetKind> = TargetKind::ORDER
        .into_iter()
        .filter(|k| parsed.contains(k))
        .collect();
    if kinds.is_empty() {
        bail!("Choose at least one thing the launcher's games use: files, folders or identifiers.");
    }
    Ok(kinds)
}

pub fn kind_names(kinds: &[TargetKind]) -> Vec<String> {
    kinds.iter().map(|k| k.as_str().to_string()).collect()
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
    /// Empty for a label-only launcher ([`is_label_only`]).
    pub executable: String,
    pub args_template: String,
    /// The first of `target_kinds` (kept for older backups and builds).
    pub target_kind: String,
    /// The allowed [`TargetKind`]s (`file`, `folder`, `identifier`), canonical order, at least
    /// one. Older backups have only `target_kind`: [`validate`] fills this from it.
    #[serde(default)]
    pub target_kinds: Vec<String>,
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
    /// The primary (first) target kind.
    #[cfg(test)]
    pub fn kind(&self) -> Result<TargetKind> {
        Ok(self.kinds()?[0])
    }

    /// The allowed target kinds (`target_kinds`, or `target_kind` for launchers stored before
    /// several kinds existed).
    pub fn kinds(&self) -> Result<Vec<TargetKind>> {
        let names = if self.target_kinds.is_empty() {
            std::slice::from_ref(&self.target_kind)
        } else {
            self.target_kinds.as_slice()
        };
        parse_kinds(names).map_err(|_| anyhow!("The launcher's target kinds are invalid."))
    }

    pub fn allows(&self, kind: TargetKind) -> bool {
        self.kinds().is_ok_and(|k| k.contains(&kind))
    }
}

/// A launcher without a program: only a label for the games found in its games folders, which
/// run directly.
pub fn is_label_only(l: &CustomLauncher) -> bool {
    l.executable.is_empty()
}

/// The launcher a route is labelled with or run by: `launcherId` (a label on a scan's local or
/// ROM route) or the `launcher:<id>` source.
pub fn route_launcher(route: &GameRoute) -> Option<&str> {
    launcher_id_of(route).or(route.launcher_id.as_deref().filter(|id| !id.is_empty()))
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

/// The target right before a launch: the right shape for one of the launcher's kinds, and an
/// existing file or folder for path kinds. Identifiers and drive paths never look alike
/// ([`check_identifier`] refuses `\`, `/` and a leading `C:`), so only path kinds can both fit.
pub fn check_target_any(kinds: &[TargetKind], target: &str) -> Result<()> {
    let fitting: Vec<TargetKind> = kinds
        .iter()
        .copied()
        .filter(|k| target_shape_ok(*k, target))
        .collect();
    if fitting.is_empty() {
        let nouns: Vec<&str> = kinds.iter().map(|k| k.noun()).collect();
        bail!(
            "This game's target doesn't fit the launcher (it expects {}). Add the game again.",
            nouns.join(" or ")
        );
    }
    let mut first = None;
    for kind in fitting {
        match check_target(kind, target) {
            Ok(()) => return Ok(()),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    Err(first.unwrap_or_else(|| anyhow!("This game's target doesn't fit the launcher.")))
}

/// The target right before a launch: the right shape for the launcher's kind, and an existing
/// file or folder for path kinds.
pub fn check_target(kind: TargetKind, target: &str) -> Result<()> {
    if !target_shape_ok(kind, target) {
        bail!(
            "This game's target doesn't fit the launcher (it expects {}). Add the game again.",
            kind.noun()
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
    let kinds = l.kinds()?;
    // Both fields given (newer backups): they must agree.
    if !l.target_kinds.is_empty()
        && TargetKind::parse(&l.target_kind).is_none_or(|k| !kinds.contains(&k))
    {
        bail!("The launcher's target kinds are invalid.");
    }
    l.target_kinds = kind_names(&kinds);
    l.target_kind = kinds[0].as_str().into();
    if is_label_only(&l) {
        // Nothing runs through a label-only launcher: no arguments and no working folder.
        if !l.args_template.trim().is_empty() || l.working_dir.is_some() {
            bail!("A launcher without a program can't have arguments or a working folder.");
        }
        l.args_template = String::new();
    } else {
        if l.executable.chars().any(char::is_control) || !executable_path_ok(&l.executable) {
            bail!(
                r"The launcher executable must be a full path to an .exe file on a local drive, like C:\Games\Heroic\Heroic.exe."
            );
        }
        check_launcher_template(&l.args_template, &l.executable)?;
    }
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
    /// One kind (older renderers); `target_kinds` wins when given.
    #[serde(default)]
    pub target_kind: String,
    /// The allowed kinds (`file`, `folder`, `identifier`), at least one.
    #[serde(default)]
    pub target_kinds: Vec<String>,
    /// `executable` (the executable's folder) or `folder` (a picked folder).
    pub working_dir: String,
    /// Use the executable just chosen with `launchers_browse_executable`.
    #[serde(default)]
    pub use_executable_pick: bool,
    /// A label-only launcher: no program (its games run directly). Overrides the pick.
    #[serde(default)]
    pub no_program: bool,
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
    /// Whether the stored launcher has games or games folders (its program can then neither be
    /// added nor removed).
    pub has_games: bool,
    /// The target kinds its existing games use (they can't be taken away).
    pub used_kinds: Vec<TargetKind>,
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
/// executable's folder, the latest folder pick, or the stored folder. With `noProgram` there is no
/// executable, arguments or working folder (a label-only launcher). A new icon image must be a
/// file [`import_icon`] created. Target kinds used by existing games can't be removed, and the
/// program can't be added or removed while the launcher has games or games folders. Everything is
/// then checked by [`validate`] (the template against the resulting executable).
pub fn accept_renderer_launcher(input: LauncherInput, ctx: SaveContext<'_>) -> Result<Accepted> {
    let stored = ctx.stored;
    if input.id.is_some() && stored.is_none() {
        bail!("That launcher no longer exists.");
    }
    let kind_input = if input.target_kinds.is_empty() {
        vec![input.target_kind.clone()]
    } else {
        input.target_kinds.clone()
    };
    let kinds = parse_kinds(&kind_input)?;
    if let Some(s) = stored {
        if ctx.has_games && is_label_only(s) != input.no_program {
            bail!("This launcher already has games or games folders, so its program can't be added or removed. Create another launcher instead.");
        }
        if let Some(missing) = ctx.used_kinds.iter().find(|k| !kinds.contains(k)) {
            bail!(
                "Some of this launcher's games use {}, so that can't be turned off. Remove those games first or create another launcher.",
                match missing {
                    TargetKind::File => "files",
                    TargetKind::Folder => "folders",
                    TargetKind::Identifier => "identifiers",
                }
            );
        }
    }
    if input.no_program {
        if !input.args_template.trim().is_empty() || input.working_dir != "executable" {
            bail!("A launcher without a program can't have arguments or a working folder.");
        }
        check_icon(&input.icon_kind, &input.icon_value)?;
        let unchanged_icon = stored
            .is_some_and(|s| s.icon_kind == input.icon_kind && s.icon_value == input.icon_value);
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
            executable: String::new(),
            args_template: String::new(),
            target_kind: kinds[0].as_str().into(),
            target_kinds: kind_names(&kinds),
            working_dir: None,
            sort_order: stored.map_or(0, |s| s.sort_order),
            created_at: stored.map_or_else(now_ms, |s| s.created_at),
            available: false,
        })?;
        return Ok(Accepted {
            launcher,
            used_executable_pick: false,
            used_folder_pick: false,
        });
    }
    let (executable, used_executable_pick) = if input.use_executable_pick {
        let pick = ctx.executable_pick.ok_or_else(|| {
            anyhow!("Choose the launcher's executable again with Browse… (the choice expired).")
        })?;
        (pick, true)
    } else {
        match stored.filter(|s| !is_label_only(s)) {
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
        target_kind: kinds[0].as_str().into(),
        target_kinds: kind_names(&kinds),
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
/// arguments, target kinds or working folder). Such a save is confirmed by the user in a native
/// dialog showing [`confirmation_prompt`], so a compromised renderer can't change it alone.
/// Name, icon and colour changes save without asking.
pub fn needs_confirmation(stored: Option<&CustomLauncher>, next: &CustomLauncher) -> bool {
    stored.is_none_or(|s| {
        s.executable != next.executable
            || s.args_template != next.args_template
            || s.kinds().ok() != next.kinds().ok()
            || s.working_dir != next.working_dir
    })
}

/// The native confirmation text: the launcher's name, its program, the exact argument list
/// (one argument per line, `{target}` standing for each game) and the working folder.
pub fn confirmation_prompt(l: &CustomLauncher) -> Result<String> {
    if is_label_only(l) {
        return Ok(format!(
            "Save the launcher \"{}\"?\n\nIt has no program. Games found in its games folders run directly, like games found in your library folders, and show \"{}\" as where they are played from. Emulators found there are added to your emulator list with their standard settings.\n\nShortcuts with arguments still need your confirmation under Review before they are added.",
            crate::validation::display_text(&l.name, 80),
            crate::validation::display_text(&l.name, 80)
        ));
    }
    let argv = build_argv(&l.args_template, TARGET)?;
    let nouns: Vec<&str> = l
        .kinds()?
        .into_iter()
        .map(|k| match k {
            TargetKind::File => "file",
            TargetKind::Folder => "folder",
            TargetKind::Identifier => "identifier",
        })
        .collect();
    let what = format!("the game's {}", nouns.join(" or "));
    let folder = match &l.working_dir {
        Some(dir) => dir.clone(),
        None => Path::new(&l.executable)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    // Right after the program, so a long argument list can't push it out of the dialog.
    let kinds = kinds_text(&l.kinds()?);
    Ok(format!(
        "Save this launcher?\n\nProgram:\n  {}\n\nAny game in your library can then be linked to this launcher without asking again; it runs this command with that game's own {kinds}.\n\nArguments (one per line; {TARGET} is {what}):\n{}\n\nWorking folder:\n  {}\n\nName: {}\n\nGame Lobby runs exactly this whenever you play one of this launcher's games. If you give the launcher games folders, they are also searched for emulators, which are added to your emulator list with their standard settings. Save it only if you set it up yourself.",
        crate::validation::escape_for_prompt(&l.executable),
        crate::links::numbered(&argv),
        crate::validation::escape_for_prompt(&folder),
        crate::validation::display_text(&l.name, 80)
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
    if is_label_only(l) {
        bail!(
            "{} has no program, so it can't start this game. Add the game from {}'s games folder instead.",
            l.name,
            l.name
        );
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
    check_target_any(&l.kinds()?, target)?;
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

/// Whether the launcher program `program` is running now: one of `processes` has exactly that
/// executable (compared as normalized paths, so case and `/` vs `\` don't matter; a program of
/// the same name elsewhere doesn't count).
pub fn program_running(processes: &[crate::platform::ProcessInfo], program: &Path) -> bool {
    let Some(want) = crate::tracker::normalize_path(&program.to_string_lossy()) else {
        return false;
    };
    processes
        .iter()
        .any(|p| crate::tracker::normalize_path(&p.path).as_deref() == Some(want.as_str()))
}

// ---------------------------------------------------------------------------
// Assigning games that are already in the library (from the game page)
//
// The renderer names only a game, a launcher and (optionally) which of the game's routes and
// which target kind to use. The target always comes from that stored route: its file, its
// install folder or its store id. Nothing typed or picked is involved, so a launcher route made
// here can't run anything the game didn't already point at; the native confirmation shows the
// exact program and arguments before it is saved.

/// One way a game can be passed to a launcher: route `route_index`'s `target`, of `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignCandidate {
    pub route_index: usize,
    pub kind: TargetKind,
    pub target: String,
}

/// Whether a route (or its launcher label) was added on the game page.
pub fn is_assigned(route: &GameRoute) -> bool {
    route.assigned == Some(true)
}

/// What a route offers as a target of `kind`, by shape only (no disk access): its target as a
/// file, its install folder (or a folder target) as a folder, its store id as an identifier.
fn route_target_of(route: &GameRoute, kind: TargetKind) -> Vec<String> {
    let text = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
    let mut found = Vec::new();
    match kind {
        TargetKind::File => found.push(route.launch_target.clone()),
        TargetKind::Folder => {
            found.extend(text(&route.install_path));
            found.push(route.launch_target.clone());
        }
        TargetKind::Identifier => {
            // A store's own id (Steam app id, Epic app name, GOG id ...). Local, ROM and
            // launcher file routes use a path key as app id, which is never an identifier.
            found.extend(text(&route.app_id));
            if is_launcher_route(route) {
                found.push(route.launch_target.clone());
            }
        }
    }
    found.retain(|t| target_shape_ok(kind, t));
    found
}

/// Every target the game's routes offer for the launcher's kinds, in route order and the
/// launcher's kind order, without repeats. Routes added by an assignment are never a source.
/// Shape only: call [`candidate_exists`] (disk access) before using one.
pub fn assign_candidates(l: &CustomLauncher, game: &Game) -> Vec<AssignCandidate> {
    let kinds = l.kinds().unwrap_or_default();
    let mut out: Vec<AssignCandidate> = Vec::new();
    for (route_index, route) in game.routes.iter().enumerate() {
        if is_assigned(route) {
            continue;
        }
        for kind in &kinds {
            for target in route_target_of(route, *kind) {
                if !out.iter().any(|c| c.kind == *kind && c.target == target) {
                    out.push(AssignCandidate {
                        route_index,
                        kind: *kind,
                        target,
                    });
                }
            }
        }
    }
    out
}

/// Whether a candidate's file or folder exists (identifiers always do). Touches the filesystem.
pub fn candidate_exists(c: &AssignCandidate) -> bool {
    check_target(c.kind, &c.target).is_ok()
}

/// Why the game can't be assigned to the launcher at all, or `None`.
pub fn assign_blocker(l: &CustomLauncher, game: &Game) -> Option<String> {
    if game
        .routes
        .iter()
        .any(|r| route_launcher(r) == Some(l.id.as_str()))
    {
        return Some(format!(
            "{} is already listed under {}.",
            game.title, l.name
        ));
    }
    if game.routes.is_empty() {
        return Some(format!(
            "{} has no way to start yet, so there is nothing to give {}.",
            game.title, l.name
        ));
    }
    None
}

/// Why none of the game's targets fit the launcher (after [`assign_candidates`] and
/// [`candidate_exists`] found none).
pub fn no_candidate_reason(l: &CustomLauncher, game: &Game) -> String {
    let nouns: Vec<&str> = l
        .kinds()
        .unwrap_or_default()
        .into_iter()
        .map(|k| match k {
            TargetKind::File => "a file",
            TargetKind::Folder => "a folder",
            TargetKind::Identifier => "a store id",
        })
        .collect();
    format!(
        "{} starts games from {}, and none of the ways {} starts now gives one (that exists on this PC). Let {} use other kinds of targets in Settings, or add the game to it with Add game.",
        l.name,
        nouns.join(" or "),
        game.title,
        l.name
    )
}

/// The launcher route an assignment adds: like one from [`new_game_of_kind`], with the base
/// route's install state, marked [`is_assigned`].
pub fn assigned_route(l: &CustomLauncher, game: &Game, c: &AssignCandidate) -> Result<GameRoute> {
    if is_label_only(l) {
        bail!(
            "{} has no program, so it doesn't get a route of its own.",
            l.name
        );
    }
    if !l.allows(c.kind) {
        bail!("{}'s games don't use {}.", l.name, c.kind.noun());
    }
    let base = game
        .routes
        .get(c.route_index)
        .filter(|r| !is_assigned(r))
        .ok_or_else(|| anyhow!("That way to start the game no longer exists."))?;
    if !route_target_of(base, c.kind).contains(&c.target) {
        bail!("That target doesn't belong to this game.");
    }
    let path = Path::new(&c.target);
    let (app_id, install_path) = match c.kind {
        TargetKind::Identifier => (None, None),
        TargetKind::File => (
            Some(key_of(path)),
            path.parent().map(|p| p.to_string_lossy().into_owned()),
        ),
        TargetKind::Folder => (Some(key_of(path)), Some(c.target.clone())),
    };
    Ok(GameRoute {
        source_id: source_id(&l.id),
        store: "Custom".into(),
        app_id,
        launch_target: c.target.clone(),
        install_path,
        installed: base.installed,
        assigned: Some(true),
        ..Default::default()
    })
}

/// The game's routes after assigning it to launcher `l`. A launcher with a program gets a new
/// route first in the list (so Play uses it), and the game's other routes stay as they are. A
/// launcher without a program only labels one existing route (`route_index`, else the first
/// one without a launcher), which keeps running as before.
pub fn assign(
    l: &CustomLauncher,
    game: &Game,
    candidate: Option<&AssignCandidate>,
    route_index: Option<usize>,
) -> Result<Vec<GameRoute>> {
    if let Some(reason) = assign_blocker(l, game) {
        bail!("{reason}");
    }
    let mut routes = game.routes.clone();
    if is_label_only(l) {
        let free = |r: &GameRoute| route_launcher(r).is_none() && !is_assigned(r);
        let index = match route_index {
            Some(i) => Some(i).filter(|i| routes.get(*i).is_some_and(free)),
            None => routes.iter().position(free),
        }
        .ok_or_else(|| {
            anyhow!(
                "Every way to start {} already shows another launcher.",
                game.title
            )
        })?;
        routes[index].launcher_id = Some(l.id.clone());
        routes[index].assigned = Some(true);
        return Ok(routes);
    }
    let candidate = candidate.ok_or_else(|| anyhow!("{}", no_candidate_reason(l, game)))?;
    routes.insert(0, assigned_route(l, game, candidate)?);
    if routes.len() > 128 {
        bail!("A game can have at most 128 launch routes.");
    }
    Ok(routes)
}

/// The game's routes without what assigning it to launcher `launcher_id` added: the assigned
/// launcher route goes, an assigned label is taken off (the route stays). Routes the launcher's
/// own Add game or games folders made are kept. `None` when there is nothing to remove.
pub fn unassign(game: &Game, launcher_id: &str) -> Option<Vec<GameRoute>> {
    let mut changed = false;
    let mut routes = Vec::with_capacity(game.routes.len());
    for route in &game.routes {
        if !is_assigned(route) {
            routes.push(route.clone());
            continue;
        }
        if launcher_id_of(route) == Some(launcher_id) {
            changed = true;
            continue;
        }
        if !is_launcher_route(route) && route.launcher_id.as_deref() == Some(launcher_id) {
            let mut next = route.clone();
            next.launcher_id = None;
            next.assigned = None;
            routes.push(next);
            changed = true;
            continue;
        }
        routes.push(route.clone());
    }
    changed.then_some(routes)
}

/// The native confirmation text for an assignment that adds a launcher route: the launcher's
/// program, the exact argument list with this game's target and the route's launch options
/// (`options`, already checked by `links::prepare_options`; may be empty), and the working
/// folder. One dialog covers both the route and its options. Confirming it also confirms the
/// launcher's command for later links ([`record_confirmed`]), which the text says.
pub fn assign_prompt(
    l: &CustomLauncher,
    game_title: &str,
    c: &AssignCandidate,
    options: &[String],
) -> Result<String> {
    let mut argv = build_argv(&l.args_template, &c.target)?;
    let base = argv.len();
    argv.extend(options.iter().cloned());
    let folder = match &l.working_dir {
        Some(dir) => dir.clone(),
        None => Path::new(&l.executable)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let what = kinds_text(&[c.kind]);
    // Confirming also confirms the launcher's command for later links of every kind it takes
    // (`record_confirmed` stores them all), so the dialog names them all, right after the
    // program where a long argument list can't push it out of the dialog.
    let kinds = kinds_text(&l.kinds()?);
    // Facts first: names and titles are user-editable text, so they come last, sanitised.
    let name = crate::validation::display_text(&l.name, 80);
    let title = crate::validation::display_text(game_title, 80);
    let note = if options.is_empty() {
        String::new()
    } else {
        format!(
            " {}",
            crate::links::options_note(base, options.len(), &name)
        )
    };
    // The target is shown once, in the arguments: with launch options the whole command line
    // is kept short enough for the dialog (`links::MAX_CONFIRM_CHARS`), and a second copy of a
    // long target would push the options off it.
    Ok(format!(
        "Game Lobby will run:\n  {}\n\nGames you link to this launcher later run the same way, each with its own {kinds}, without asking again until the launcher's command changes.\n\nArguments (one per line, with the game's {what}):\n{}\n\nWorking folder:\n  {}\n\nFor the game: {title}\nLauncher: {name}\n\nPlay will use {name} from now on.{note} The game's current way to start stays under Play from, and you can remove {name} from the game page at any time.",
        crate::validation::escape_for_prompt(&l.executable),
        crate::links::numbered(&argv),
        crate::validation::escape_for_prompt(&folder),
    ))
}

// ---------------------------------------------------------------------------
// Games

/// A new game for a launcher with its primary target kind (see [`new_game_of_kind`]).
#[cfg(test)]
pub fn new_game(l: &CustomLauncher, title: &str, target: &str) -> Result<Game> {
    new_game_of_kind(l, title, target, l.kind()?)
}

/// A new game for a launcher. `target` is a picked path (path kinds) or a typed identifier;
/// `title` may be empty for path targets (the file or folder name is used then). `kind` must be
/// one of the launcher's kinds, and the launcher must have a program.
pub fn new_game_of_kind(
    l: &CustomLauncher,
    title: &str,
    target: &str,
    kind: TargetKind,
) -> Result<Game> {
    if !l.allows(kind) {
        bail!("{}'s games don't use {}.", l.name, kind.noun());
    }
    if is_label_only(l) {
        bail!(
            "{} has no program: add games from its games folders.",
            l.name
        );
    }
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

/// The ids of games with a route run by or labelled with launcher `id`.
pub fn games_of(conn: &Connection, id: &str) -> Result<Vec<String>> {
    Ok(db::all_games(conn)?
        .into_iter()
        .filter(|g| g.routes.iter().any(|r| route_launcher(r) == Some(id)))
        .map(|g| g.id)
        .collect())
}

/// The targets of launcher `id`'s own (`launcher:<id>`) routes, to tell which target kinds its
/// games use ([`used_kinds`]).
pub fn route_targets(conn: &Connection, id: &str) -> Result<Vec<String>> {
    Ok(db::all_games(conn)?
        .into_iter()
        .flat_map(|g| g.routes)
        .filter(|r| launcher_id_of(r) == Some(id))
        .map(|r| r.launch_target)
        .collect())
}

/// The target kinds a launcher's route targets use. Touches the filesystem (a path is a folder
/// when it is one now, else a file): call it without the database lock.
pub fn used_kinds(targets: &[String]) -> Vec<TargetKind> {
    let mut kinds = Vec::new();
    for target in targets {
        let kind = if target_shape_ok(TargetKind::Identifier, target) {
            TargetKind::Identifier
        } else if Path::new(target).is_dir() {
            TargetKind::Folder
        } else {
            TargetKind::File
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// Deletes a launcher and handles its games per `mode`: its own routes and the routes labelled
/// with it (found in its games folders). `KeepGames` keeps labelled routes as ordinary local or
/// ROM routes (label and games-folder install state removed); launcher routes go either way.
/// Its games folders and their open review items go too. `None` when it doesn't exist;
/// otherwise the ids of removed games. Icon files are left on disk (like replaced page art).
pub fn remove(conn: &Connection, id: &str, mode: RemoveMode) -> Result<Option<Vec<String>>> {
    if get(conn, id)?.is_none() {
        return Ok(None);
    }
    let tx = conn.unchecked_transaction()?;
    let root_keys: Vec<String> = crate::roots::list(&tx)?
        .into_iter()
        .filter(|r| r.launcher_id.as_deref() == Some(id))
        .map(|r| {
            crate::review::forget_root(&tx, &r.id)?;
            crate::roots::delete(&tx, &r.id)?;
            Ok(db::root_state_key(&r.id))
        })
        .collect::<Result<_>>()?;
    let mut removed = Vec::new();
    for game in db::all_games(&tx)? {
        if !game.routes.iter().any(|r| route_launcher(r) == Some(id)) {
            continue;
        }
        let rest: Vec<GameRoute> = game
            .routes
            .iter()
            .filter(|r| {
                // A label assigned on the game page sits on a route of another store or folder:
                // only the label goes, whatever the mode.
                launcher_id_of(r) != Some(id)
                    && (mode == RemoveMode::KeepGames
                        || route_launcher(r) != Some(id)
                        || is_assigned(r))
            })
            .cloned()
            .map(|mut r| {
                if route_launcher(&r) == Some(id) {
                    r.launcher_id = None;
                    r.assigned = None;
                    if let Some(states) = r.install_states.as_mut() {
                        states.retain(|k, _| !root_keys.contains(k));
                    }
                }
                r
            })
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
    forget_confirmed(&tx, id)?;
    tx.commit()?;
    Ok(Some(removed))
}

// ---------------------------------------------------------------------------
// Storage

const COLUMNS: &str =
    "id,name,iconKind,iconValue,color,executable,argsTemplate,targetKind,workingDir,sortOrder,createdAt,targetKinds";

fn read_row(row: &Row) -> rusqlite::Result<CustomLauncher> {
    let target_kind: String = row.get(7)?;
    // Stored before several kinds existed: the one kind.
    let target_kinds = row
        .get::<_, Option<String>>(11)?
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| vec![target_kind.clone()]);
    Ok(CustomLauncher {
        id: row.get(0)?,
        name: row.get(1)?,
        icon_kind: row.get(2)?,
        icon_value: row.get(3)?,
        color: row.get(4)?,
        executable: row.get(5)?,
        args_template: row.get(6)?,
        target_kind,
        target_kinds,
        working_dir: row.get::<_, Option<String>>(8)?.filter(|d| !d.is_empty()),
        sort_order: row.get(9)?,
        created_at: row.get(10)?,
        available: false,
    })
}

/// Adds `targetKinds` to `custom_launchers` and fills it from `targetKind` (idempotent; no
/// `user_version` change: older builds keep reading `targetKind`, which stays the first kind).
pub fn migrate(conn: &Connection) -> Result<()> {
    let columns: std::collections::HashSet<String> = conn
        .prepare("SELECT name FROM pragma_table_info('custom_launchers')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !columns.contains("targetKinds") {
        conn.execute_batch("ALTER TABLE custom_launchers ADD COLUMN targetKinds TEXT;")?;
    }
    let pending: Vec<(String, String)> = conn
        .prepare("SELECT id,targetKind FROM custom_launchers WHERE targetKinds IS NULL OR targetKinds=''")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, kind) in pending {
        conn.execute(
            "UPDATE custom_launchers SET targetKinds=? WHERE id=?",
            params![serde_json::to_string(&[kind])?, id],
        )?;
    }
    // What the user confirmed for each launcher on this PC ([`record_confirmed`]): an added
    // table, no `user_version` change (older builds ignore it). Never part of a backup.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS launcher_confirmed(launcherId TEXT PRIMARY KEY,exeKey TEXT NOT NULL,template TEXT NOT NULL,cwdKey TEXT NOT NULL,targetKinds TEXT NOT NULL,confirmedAt INTEGER NOT NULL);",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Confirmed launch commands
//
// When the user confirms a launcher in its native save dialog (`launchers_save`), or confirms
// linking a game to it ([`assign_prompt`]), Game Lobby remembers exactly what was shown: the
// program, the argument template, the working folder and the target kinds (table
// `launcher_confirmed`, one row per launcher). Linking another game of the library to that
// launcher then needs no dialog, because it runs nothing new: the same command, with the game's
// own stored file, folder or store id as `{target}` (the case the save dialog describes). A
// launcher whose command differs from its row in any way (restored from a backup, changed
// without a dialog, saved before this table existed) has to be confirmed again. Launch options
// are never covered by this (they keep their own confirmations, `links`).

/// What a confirmation covers for launcher `l`: its normalised program, its template (JSON), its
/// normalised working folder and its target kinds (JSON). `None` for a launcher without a
/// program, or one whose fields can't be read.
fn confirmed_key(l: &CustomLauncher) -> Option<(String, String, String, String)> {
    if is_label_only(l) {
        return None;
    }
    let exe = crate::tracker::normalize_path(&l.executable)?;
    let cwd = match &l.working_dir {
        Some(dir) => dir.clone(),
        None => Path::new(&l.executable)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let cwd = crate::tracker::normalize_path(&cwd)?;
    let template = serde_json::to_string(&l.args_template).ok()?;
    let kinds = serde_json::to_string(&kind_names(&l.kinds().ok()?)).ok()?;
    Some((exe, template, cwd, kinds))
}

/// Remembers that the user confirmed launcher `l`'s command on this PC. Only called right after
/// a native dialog that showed it, inside that dialog's compare-and-save (`launchers_save`,
/// `launchers_assign`). A launcher without a program forgets any earlier confirmation.
pub fn record_confirmed(conn: &Connection, l: &CustomLauncher) -> Result<()> {
    match confirmed_key(l) {
        Some((exe, template, cwd, kinds)) => {
            conn.execute(
                "INSERT OR REPLACE INTO launcher_confirmed(launcherId,exeKey,template,cwdKey,targetKinds,confirmedAt) VALUES(?,?,?,?,?,?)",
                params![l.id, exe, template, cwd, kinds, now_ms()],
            )?;
        }
        None => forget_confirmed(conn, &l.id)?,
    }
    Ok(())
}

/// Forgets launcher `id`'s confirmation (it was deleted, or lost its program).
pub fn forget_confirmed(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM launcher_confirmed WHERE launcherId=?", [id])?;
    Ok(())
}

/// Whether the user confirmed exactly launcher `l`'s current command on this PC
/// ([`record_confirmed`]). Always false for a launcher without a program.
pub fn is_confirmed(conn: &Connection, l: &CustomLauncher) -> Result<bool> {
    let Some((exe, template, cwd, kinds)) = confirmed_key(l) else {
        return Ok(false);
    };
    Ok(conn
        .query_row(
            "SELECT 1 FROM launcher_confirmed WHERE launcherId=? AND exeKey=? AND template=? AND cwdKey=? AND targetKinds=?",
            params![l.id, exe, template, cwd, kinds],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
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
            "INSERT INTO custom_launchers({COLUMNS}) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name,iconKind=excluded.iconKind,iconValue=excluded.iconValue,color=excluded.color,executable=excluded.executable,argsTemplate=excluded.argsTemplate,targetKind=excluded.targetKind,workingDir=excluded.workingDir,sortOrder=excluded.sortOrder,targetKinds=excluded.targetKinds"
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
            serde_json::to_string(&match l.kinds() {
                Ok(kinds) => kind_names(&kinds),
                Err(_) => vec![l.target_kind.clone()],
            })?,
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
            // A label-only launcher has nothing to find.
            l.available = is_label_only(&l) || crate::emulators::executable_ok(&l.executable);
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
            target_kinds: vec![],
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
            target_kinds: vec![],
            working_dir: "executable".into(),
            use_executable_pick: true,
            no_program: false,
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
            used_kinds: vec![],
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

        // A target kind its games use can't be taken away; others can be added and removed.
        let stored = validate(launcher(EXE, "identifier")).unwrap();
        let mut change = input("file");
        change.id = Some(stored.id.clone());
        change.use_executable_pick = false;
        let mut c = ctx(Some(&stored), None, &none);
        c.has_games = true;
        c.used_kinds = vec![TargetKind::Identifier];
        assert!(accept_renderer_launcher(change.clone(), c).is_err());
        let mut c = ctx(Some(&stored), None, &none);
        c.has_games = true;
        c.used_kinds = vec![TargetKind::Identifier];
        let mut both = change.clone();
        both.target_kinds = vec!["identifier".into(), "file".into(), "file".into()];
        let saved = accept_renderer_launcher(both, c).unwrap().launcher;
        assert_eq!(
            saved.target_kinds,
            ["file", "identifier"],
            "canonical order, no repeats"
        );
        assert_eq!(saved.target_kind, "file");
        assert!(accept_renderer_launcher(change, ctx(Some(&stored), None, &none)).is_ok());
        let mut none_kind = input("file");
        none_kind.target_kind = String::new();
        assert!(accept_renderer_launcher(none_kind, ctx(None, Some(EXE), &none)).is_err());
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
            |l: &mut CustomLauncher| l.target_kinds = vec!["file".into()],
            |l: &mut CustomLauncher| l.working_dir = Some(r"D:\Mods".into()),
        ];
        // Allowing another kind of target changes what the launcher can be given too.
        let mut more = stored.clone();
        more.target_kinds = vec!["file".into(), "identifier".into()];
        assert!(needs_confirmation(Some(&stored), &more));
        for change in changes {
            let mut next = stored.clone();
            change(&mut next);
            assert!(needs_confirmation(Some(&stored), &next));
        }

        let mut shown = stored.clone();
        shown.args_template = r#"-p "My Profile" {target} --fullscreen"#.into();
        let text = confirmation_prompt(&shown).unwrap();
        assert!(text.contains("Name: Heroic"), "{text}");
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
        // The program, arguments and folder are shown exactly, with control and bidi characters
        // written out, like every other confirmation (`links::numbered`).
        let raw = CustomLauncher {
            executable: "D:\\Launchers\\\u{202e}exe.gnp\\Heroic.exe".into(),
            args_template: "--a\u{7}b {target}".into(),
            working_dir: Some("D:\\Mods\n\u{2066}x".into()),
            ..shown.clone()
        };
        let text = confirmation_prompt(&raw).unwrap();
        assert!(
            text.starts_with(
                "Save this launcher?\n\nProgram:\n  D:\\Launchers\\<U+202E>exe.gnp\\Heroic.exe\n\nAny game in your library can then be linked to this launcher without asking again; it runs this command with that game's own store id.\n\nArguments (one per line; {target} is the game's identifier):\n  1. --a<U+0007>b\n  2. {target}\n\nWorking folder:\n  D:\\Mods<U+000A><U+2066>x\n\nName: Heroic\n"
            ),
            "{text}"
        );
        assert!(!text.contains(['\u{202e}', '\u{2066}', '\u{7}']));
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
    fn several_target_kinds_are_validated_per_kind() {
        let dir = TempDir::new("launcher-kinds");
        let file = dir.file("Games/a.rom");
        let folder = dir.dir("Games/B");
        let mut l = launcher(EXE, "file");
        l.target_kinds = vec!["identifier".into(), "file".into()];
        let l = validate(l).unwrap();
        assert_eq!(l.target_kinds, ["file", "identifier"]);
        assert_eq!(l.target_kind, "file");
        let kinds = l.kinds().unwrap();
        assert!(check_target_any(&kinds, &file.to_string_lossy()).is_ok());
        assert!(check_target_any(&kinds, "Hollow Knight").is_ok());
        // A folder is not a file, and folders aren't allowed.
        assert!(check_target_any(&kinds, &folder.to_string_lossy()).is_err());
        assert!(check_target_any(&kinds, "-x").is_err());
        let both = [TargetKind::File, TargetKind::Folder];
        assert!(check_target_any(&both, &folder.to_string_lossy()).is_ok());
        assert!(check_target_any(&both, "Hollow Knight").is_err());
        // Each game uses one allowed kind.
        assert!(new_game_of_kind(&l, "", &file.to_string_lossy(), TargetKind::File).is_ok());
        assert!(new_game_of_kind(&l, "HK", "hk", TargetKind::Identifier).is_ok());
        assert!(new_game_of_kind(&l, "", &folder.to_string_lossy(), TargetKind::Folder).is_err());
        assert!(new_game_of_kind(&l, "HK", r"C:\x", TargetKind::Identifier).is_err());
        // Unknown, empty or contradictory sets are refused.
        for kinds in [
            vec![],
            vec!["url".to_string()],
            vec!["file".into(), "url".into()],
        ] {
            let mut bad = launcher(EXE, "file");
            bad.target_kinds = kinds.clone();
            assert!(validate(bad).is_err() || kinds.is_empty(), "{kinds:?}");
        }
        let mut bad = launcher(EXE, "folder");
        bad.target_kinds = vec!["file".into()];
        assert!(validate(bad).is_err(), "targetKind outside targetKinds");
        assert!(parse_kinds(&[]).is_err());
        // The prompt names every kind.
        let text = confirmation_prompt(&l).unwrap();
        assert!(
            text.contains("{target} is the game's file or identifier"),
            "{text}"
        );
        assert!(text.contains("searched for emulators"), "{text}");
        // Saving confirms the command for later one-click links of every kind, which the dialog
        // says right after the program (above the arguments, so it can't be clipped).
        let sentence = "Any game in your library can then be linked to this launcher without asking again; it runs this command with that game's own file or store id.";
        let at = text.find(sentence).expect(&text);
        assert!(at > text.find("Program:").unwrap(), "{text}");
        assert!(at < text.find("Arguments").unwrap(), "{text}");
        let mut all = l.clone();
        all.target_kinds = vec!["file".into(), "folder".into(), "identifier".into()];
        assert!(confirmation_prompt(&all)
            .unwrap()
            .contains("that game's own file, folder or store id."));
        all.target_kinds = vec!["folder".into()];
        all.target_kind = "folder".into();
        assert!(confirmation_prompt(&all)
            .unwrap()
            .contains("that game's own folder."));
        // A launcher without a program confirms nothing for later links.
        assert!(!confirmation_prompt(&CustomLauncher {
            executable: String::new(),
            args_template: String::new(),
            ..l.clone()
        })
        .unwrap()
        .contains("without asking again"));
    }

    #[test]
    fn target_kind_sets_migrate_and_old_backups_still_import() {
        let db = Db::memory().unwrap();
        let conn = db.lock();
        // A launcher stored before several kinds existed: only `targetKind`.
        conn.execute(
            "INSERT INTO custom_launchers(id,name,iconKind,iconValue,color,executable,argsTemplate,targetKind,sortOrder,createdAt) VALUES('old','Old','builtin','rocket','#4f8cff',?,'{target}','folder',0,1)",
            [EXE],
        )
        .unwrap();
        let old = get(&conn, "old").unwrap().unwrap();
        assert_eq!(old.target_kinds, ["folder"], "read as its one kind");
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let stored: String = conn
            .query_row(
                "SELECT targetKinds FROM custom_launchers WHERE id='old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, r#"["folder"]"#);
        // Saving keeps both columns in step (older builds read `targetKind`).
        let mut next = old.clone();
        next.target_kinds = vec!["identifier".into(), "folder".into()];
        save(&conn, &validate(next).unwrap()).unwrap();
        let (kind, kinds): (String, String) = conn
            .query_row(
                "SELECT targetKind,targetKinds FROM custom_launchers WHERE id='old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (kind.as_str(), kinds.as_str()),
            ("folder", r#"["folder","identifier"]"#)
        );

        // An old backup entry (only `targetKind`) imports with that kind; a new one round-trips.
        let old_entry = serde_json::json!({
            "id": "a", "name": "A", "iconKind": "builtin", "iconValue": "rocket", "color": "#4f8cff",
            "executable": EXE, "argsTemplate": "{target}", "targetKind": "identifier"
        });
        let imported = crate::validation::backup_launcher(&old_entry).unwrap();
        assert_eq!(imported.target_kinds, ["identifier"]);
        let json = serde_json::to_value(get(&conn, "old").unwrap().unwrap()).unwrap();
        assert_eq!(
            json["targetKinds"],
            serde_json::json!(["folder", "identifier"])
        );
        let back = crate::validation::backup_launcher(&json).unwrap();
        assert_eq!(back.target_kinds, ["folder", "identifier"]);
        // A label-only launcher backs up and imports too, with no program.
        let mut label = old_entry.clone();
        label["executable"] = "".into();
        label["argsTemplate"] = "".into();
        assert!(is_label_only(
            &crate::validation::backup_launcher(&label).unwrap()
        ));
        label["argsTemplate"] = "--run {target}".into();
        assert!(crate::validation::backup_launcher(&label).is_err());
    }

    #[test]
    fn a_launcher_without_a_program_is_only_a_label() {
        let none = |_: &str| false;
        let mut input = input("file");
        input.no_program = true;
        input.args_template = String::new();
        input.use_executable_pick = false;
        // A pick is never used for a label-only launcher, even when one is waiting.
        let accepted =
            accept_renderer_launcher(input.clone(), ctx(None, Some(EXE), &none)).unwrap();
        assert!(!accepted.used_executable_pick);
        let l = accepted.launcher;
        assert!(is_label_only(&l));
        assert_eq!(
            (
                l.executable.as_str(),
                l.args_template.as_str(),
                l.working_dir.as_ref()
            ),
            ("", "", None)
        );
        assert!(needs_confirmation(None, &l));
        let text = confirmation_prompt(&l).unwrap();
        assert!(
            text.contains("no program") && text.contains("run directly"),
            "{text}"
        );
        // No arguments or working folder without a program.
        let mut args = input.clone();
        args.args_template = "--x {target}".into();
        assert!(accept_renderer_launcher(args, ctx(None, None, &none)).is_err());
        let mut dir = input.clone();
        dir.working_dir = "folder".into();
        assert!(accept_renderer_launcher(dir, ctx(None, None, &none)).is_err());
        let mut raw = l.clone();
        raw.args_template = "{target}".into();
        assert!(validate(raw).is_err());
        let mut raw = l.clone();
        raw.working_dir = Some(r"D:\x".into());
        assert!(validate(raw).is_err());
        // A launcher route of a label-only launcher never runs, and it adds no games by hand.
        let route = GameRoute {
            source_id: source_id(&l.id),
            store: "Custom".into(),
            launch_target: "hk".into(),
            ..Default::default()
        };
        assert!(prepare_launch(&l, &route)
            .unwrap_err()
            .to_string()
            .contains("no program"));
        assert!(new_game_of_kind(&l, "HK", "hk", TargetKind::File).is_err());
        assert!(with_status(vec![l.clone()])[0].available, "nothing to find");

        // The program can't be added or removed while the launcher has games or folders.
        let stored = validate(launcher(EXE, "file")).unwrap();
        let mut drop_program = input.clone();
        drop_program.id = Some(stored.id.clone());
        let mut c = ctx(Some(&stored), None, &none);
        c.has_games = true;
        assert!(accept_renderer_launcher(drop_program.clone(), c).is_err());
        assert!(accept_renderer_launcher(drop_program, ctx(Some(&stored), None, &none)).is_ok());
        let mut add_program = super::tests::input("file");
        add_program.id = Some(l.id.clone());
        let mut c = ctx(Some(&l), Some(EXE), &none);
        c.has_games = true;
        assert!(accept_renderer_launcher(add_program.clone(), c).is_err());
        // Without a pick, a label-only launcher has no stored program to keep.
        add_program.use_executable_pick = false;
        assert!(accept_renderer_launcher(add_program.clone(), ctx(Some(&l), None, &none)).is_err());
        add_program.use_executable_pick = true;
        let added = accept_renderer_launcher(add_program, ctx(Some(&l), Some(EXE), &none)).unwrap();
        assert_eq!(added.launcher.executable, EXE);
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
        // A shortcut given to the launcher is checked again before every launch, and before
        // `prepare_launch` builds the argv.
        let check = branch
            .find("check_launcher_shortcut(&target, &confirmations)")
            .expect("shortcut check");
        assert!(check < branch.find("launchers::prepare_launch").unwrap());
        // The game runs with the launcher's argv plus the route's confirmed launch options.
        let options = branch
            .find("crate::links::launcher_launch_argv(")
            .expect("launch options checked");
        assert!(options > branch.find("launchers::prepare_launch").unwrap());
        let game = branch
            .find("spawn_detached_in(&launch.program, &argv, Some(&launch.cwd))")
            .expect("game started");
        assert!(branch.contains("track_started_process(&state, &game.id, pid, &launch.program)"));
        // A launcher that isn't open yet is started alone: its checked program, no arguments
        // and no launch options, only after every check (the options' too), and before the
        // game would start.
        let alone = branch
            .find("spawn_detached_in(&launch.program, &[], Some(&launch.cwd))")
            .expect("launcher started alone");
        assert!(alone > options);
        assert!(alone < game);
        assert_eq!(branch.matches("spawn_detached_in(").count(), 2);
        assert!(branch.contains(".pending_launches") && branch.contains(".begin("));
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
                game_links: None,
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

    // -----------------------------------------------------------------------
    // Assignments from the game page

    fn named(id: &str, exe: &str, kinds: &[&str]) -> CustomLauncher {
        let mut l = launcher(exe, kinds[0]);
        l.id = id.into();
        l.name = id.to_uppercase();
        l.target_kinds = kinds.iter().map(|k| k.to_string()).collect();
        validate(l).unwrap()
    }

    fn label_only(id: &str) -> CustomLauncher {
        let mut l = launcher("", "file");
        l.id = id.into();
        l.name = id.to_uppercase();
        l.args_template = String::new();
        validate(l).unwrap()
    }

    fn local_route(exe: &Path) -> GameRoute {
        GameRoute {
            source_id: "local".into(),
            store: "Custom".into(),
            app_id: Some(key_of(exe)),
            launch_target: exe.to_string_lossy().into_owned(),
            install_path: exe.parent().map(|p| p.to_string_lossy().into_owned()),
            installed: Some(true),
            ..Default::default()
        }
    }

    fn game_with(id: &str, routes: Vec<GameRoute>) -> Game {
        Game {
            id: id.into(),
            title: "Hollow Knight".into(),
            store: routes[0].store.clone(),
            routes,
            ..Default::default()
        }
    }

    #[test]
    fn assignments_take_the_target_from_the_game_and_never_from_the_renderer() {
        let dir = TempDir::new("launcher-assign");
        let exe = dir.file("Heroic/Heroic.exe");
        let game_exe = dir.file("Games/Hollow Knight/hk.exe");
        let l = named("heroic", &exe.to_string_lossy(), &["file"]);
        let game = game_with("g", vec![steam_route(), local_route(&game_exe)]);

        // Only the stored routes' targets are offered: the local route's file. A Steam link is
        // no file.
        let candidates = assign_candidates(&l, &game);
        assert_eq!(
            candidates,
            [AssignCandidate {
                route_index: 1,
                kind: TargetKind::File,
                target: game_exe.to_string_lossy().into_owned(),
            }]
        );
        assert!(candidates.iter().all(candidate_exists));

        // A target that isn't the route's own (what a forged request would carry) is refused,
        // as is another route's index or a kind the launcher doesn't use.
        for forged in [
            AssignCandidate {
                target: r"C:\Windows\System32\calc.exe".into(),
                ..candidates[0].clone()
            },
            AssignCandidate {
                route_index: 0,
                ..candidates[0].clone()
            },
            AssignCandidate {
                route_index: 9,
                ..candidates[0].clone()
            },
            AssignCandidate {
                kind: TargetKind::Identifier,
                target: "400".into(),
                route_index: 0,
            },
        ] {
            assert!(assigned_route(&l, &game, &forged).is_err(), "{forged:?}");
            assert!(
                assign(&l, &game, Some(&forged), None).is_err(),
                "{forged:?}"
            );
        }

        // The added route comes first (Play uses it); the game's own routes stay unchanged.
        let routes = assign(&l, &game, Some(&candidates[0]), None).unwrap();
        assert_eq!(routes.len(), 3);
        assert_eq!(routes[1..], game.routes[..]);
        let added = &routes[0];
        assert_eq!(added.source_id, "launcher:heroic");
        assert_eq!(added.launch_target, game_exe.to_string_lossy());
        assert!(is_assigned(added));
        assert_eq!(added.installed, Some(true));
        // It launches exactly like a route from Add game: the launcher's program, its template,
        // this target as one argument.
        let launch = prepare_launch(&l, added).unwrap();
        assert_eq!(launch.program, exe);
        assert_eq!(
            launch.argv,
            ["--start".to_string(), added.launch_target.clone()]
        );
        let prompt = assign_prompt(&l, &game.title, &candidates[0], &[]).unwrap();
        assert!(prompt.contains(&*exe.to_string_lossy()));
        assert!(prompt.contains(&format!("  2. {}", game_exe.to_string_lossy())));

        // A title can't fake the dialog: the facts come first, and the title is one clean line.
        let spoof = "Fine\n\nGame Lobby will run:\n  safe.exe\u{202e}\t\u{7}";
        let prompt = assign_prompt(&l, spoof, &candidates[0], &[]).unwrap();
        let first: Vec<&str> = prompt.lines().take(7).collect();
        assert_eq!(first[0], "Game Lobby will run:");
        assert_eq!(first[1], format!("  {}", exe.to_string_lossy()));
        assert!(
            first[3].starts_with("Games you link to this launcher later"),
            "{first:?}"
        );
        assert!(first[6].ends_with("  1. --start"), "{first:?}");
        assert!(!prompt
            .chars()
            .any(|c| (c.is_control() && c != '\n') || ('\u{202a}'..='\u{202e}').contains(&c)));
        let long = assign_prompt(&l, &"x".repeat(500), &candidates[0], &[]).unwrap();
        assert!(long.contains(&format!("For the game: {}\u{2026}", "x".repeat(79))));

        // Confirming covers later links of every kind the launcher takes (`record_confirmed`
        // stores them all), so the dialog names them all, not only the kind used now, and says
        // so right after the program, above the arguments, where it can't be clipped.
        let both = named("hub", &exe.to_string_lossy(), &["file", "identifier"]);
        let prompt = assign_prompt(&both, &game.title, &candidates[0], &[]).unwrap();
        let sentence = "Games you link to this launcher later run the same way, each with its own file or store id, without asking again until the launcher's command changes.";
        let at = prompt.find(sentence).expect(&prompt);
        assert!(
            at > prompt.find(&*exe.to_string_lossy()).unwrap(),
            "{prompt}"
        );
        assert!(at < prompt.find("Arguments").unwrap(), "{prompt}");
        assert!(
            prompt.contains("Arguments (one per line, with the game's file):"),
            "{prompt}"
        );
        let id = AssignCandidate {
            route_index: 0,
            kind: TargetKind::Identifier,
            target: "400".into(),
        };
        let prompt = assign_prompt(&both, &game.title, &id, &[]).unwrap();
        assert!(prompt.contains(sentence), "{prompt}");
        assert!(prompt.contains("with the game's store id):"), "{prompt}");
        let all = named(
            "all",
            &exe.to_string_lossy(),
            &["file", "folder", "identifier"],
        );
        assert!(assign_prompt(&all, &game.title, &candidates[0], &[])
            .unwrap()
            .contains("each with its own file, folder or store id, without"));
        assert!(assign_prompt(&l, &game.title, &candidates[0], &[])
            .unwrap()
            .contains("each with its own file, without"));

        // The renderer can't add that route itself, nor mark or unmark a route as assigned.
        let check = crate::validation::check_renderer_routes;
        assert!(check(&game.routes, &routes).is_err(), "adding it");
        assert!(check(&routes, &routes).is_ok(), "sending it back unchanged");
        let mut unmarked = routes.clone();
        unmarked[0].assigned = None;
        assert!(check(&routes, &unmarked).is_err(), "unmarking it");
        let mut marked = game.routes.clone();
        marked[1].assigned = Some(true);
        assert!(
            check(&game.routes, &marked).is_err(),
            "marking another route"
        );
        let mut link = game.routes.clone();
        link[0].assigned = Some(true);
        assert!(check(&game.routes, &link).is_err(), "even on a store link");

        // Once listed under the launcher, it can't be assigned again.
        let assigned = Game {
            routes: routes.clone(),
            ..game.clone()
        };
        assert!(assign_blocker(&l, &assigned).is_some());
        assert!(assign(&l, &assigned, Some(&candidates[0]), None).is_err());
        // A route made by an assignment is never the source of another one.
        let other = named("playnite", &exe.to_string_lossy(), &["file"]);
        assert!(assign_candidates(&other, &assigned)
            .iter()
            .all(|c| c.route_index != 0));
    }

    #[test]
    fn a_target_kind_the_launcher_does_not_use_is_refused() {
        let dir = TempDir::new("launcher-assign-kinds");
        let exe = dir.file("Heroic/Heroic.exe");
        let exe = exe.to_string_lossy();
        // A Steam game: a link, an install folder that no longer exists, and a store id.
        let mut steam = steam_route();
        steam.install_path = Some(dir.path().join("gone").to_string_lossy().into_owned());
        let game = game_with("g", vec![steam]);

        let files = named("files", &exe, &["file"]);
        assert!(assign_candidates(&files, &game).is_empty());
        assert!(assign(&files, &game, None, None).is_err());
        assert!(no_candidate_reason(&files, &game).contains("a file"));

        // Folders: the install folder fits by shape, but it must exist.
        let folders = named("folders", &exe, &["folder"]);
        let found = assign_candidates(&folders, &game);
        assert_eq!(found.len(), 1);
        assert!(!candidate_exists(&found[0]));

        // Identifiers: the Steam app id.
        let ids = named("ids", &exe, &["identifier"]);
        let found = assign_candidates(&ids, &game);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target, "400");
        assert_eq!(found[0].kind, TargetKind::Identifier);
        let routes = assign(&ids, &game, Some(&found[0]), None).unwrap();
        assert_eq!(routes[0].launch_target, "400");
        assert_eq!(routes[0].app_id, None);

        // Path keys used as app ids by local and ROM routes are never identifiers.
        let game_exe = dir.file("Games/x/x.exe");
        let local = game_with("l", vec![local_route(&game_exe)]);
        assert!(assign_candidates(&ids, &local).is_empty());
        let both = named("both", &exe, &["file", "identifier"]);
        let found = assign_candidates(&both, &local);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, TargetKind::File);
    }

    #[test]
    fn unassigning_removes_only_what_the_game_page_added() {
        let dir = TempDir::new("launcher-unassign");
        let exe = dir.file("Heroic/Heroic.exe");
        let exe = exe.to_string_lossy();
        let own = named("heroic", &exe, &["identifier"]);
        let ids = named("ids", &exe, &["identifier"]);
        let app = label_only("app");
        // A game Heroic's Add game made, which also has a Steam route.
        let mut game = new_game(&own, "Hollow Knight", "hk").unwrap();
        game.routes.push(steam_route());
        let original = game.routes.clone();

        // Assigned to another launcher with a program, and to a launcher without one.
        let candidate = assign_candidates(&ids, &game)
            .into_iter()
            .find(|c| c.target == "400")
            .unwrap();
        game.routes = assign(&ids, &game, Some(&candidate), None).unwrap();
        assert_eq!(game.routes.len(), 3);
        game.routes = assign(&app, &game, None, None).unwrap();
        // The label goes on the first route without a launcher (the Steam route), which keeps
        // running as before.
        let labelled = game.routes.iter().find(|r| r.source_id == "steam").unwrap();
        assert_eq!(labelled.launcher_id.as_deref(), Some("app"));
        assert!(is_assigned(labelled));
        assert_eq!(game.routes.len(), 3);
        // A launcher without a program never gets a route of its own.
        assert!(game.routes.iter().all(|r| launcher_id_of(r) != Some("app")));

        // Heroic's own route wasn't added on the game page: nothing to unassign.
        assert_eq!(unassign(&game, "heroic"), None);
        assert_eq!(unassign(&game, "nobody"), None);
        let without_ids = unassign(&game, "ids").unwrap();
        assert_eq!(without_ids.len(), 2);
        assert!(without_ids.iter().all(|r| launcher_id_of(r) != Some("ids")));
        let without_app = unassign(
            &Game {
                routes: without_ids,
                ..game.clone()
            },
            "app",
        )
        .unwrap();
        assert_eq!(without_app, original, "back to exactly what it was");

        // A label from a games folder scan (not assigned) is never taken off.
        let mut scanned = steam_route();
        scanned.launcher_id = Some("app".into());
        let found = game_with("s", vec![scanned]);
        assert_eq!(unassign(&found, "app"), None);
        assert!(assign(&app, &found, None, None).is_err(), "already listed");

        // Deleting a launcher with "remove its games" never removes a store route it only
        // labelled from the game page; the label goes.
        let db = library_with(&[]);
        {
            let conn = db.lock();
            save(&conn, &app).unwrap();
            save(&conn, &ids).unwrap();
            db::save_game(&conn, &GamePatch::from(&game)).unwrap();
            assert_eq!(
                remove(&conn, "app", RemoveMode::RemoveGames).unwrap(),
                Some(vec![])
            );
            assert_eq!(
                remove(&conn, "ids", RemoveMode::RemoveGames).unwrap(),
                Some(vec![])
            );
            let left = db::get_game(&conn, &game.id).unwrap().unwrap();
            assert_eq!(left.routes, original);
        }
    }

    #[test]
    fn rescans_and_backups_keep_assignments() {
        let dir = TempDir::new("launcher-assign-rescan");
        let exe = dir.file("Heroic/Heroic.exe");
        let game_exe = dir.file("Games/Hollow Knight/hk.exe");
        let files = named("files", &exe.to_string_lossy(), &["file"]);
        let ids = named("ids", &exe.to_string_lossy(), &["identifier"]);
        let app = label_only("app");
        let mut game = game_with("g", vec![steam_route(), local_route(&game_exe)]);
        let file = assign_candidates(&files, &game)[0].clone();
        game.routes = assign(&files, &game, Some(&file), None).unwrap();
        let id = assign_candidates(&ids, &game)
            .into_iter()
            .find(|c| c.kind == TargetKind::Identifier)
            .unwrap();
        game.routes = assign(&ids, &game, Some(&id), None).unwrap();
        game.routes = assign(&app, &game, None, None).unwrap();
        let assigned = game.routes.clone();
        assert_eq!(assigned.iter().filter(|r| is_assigned(r)).count(), 3);

        let db = library_with(&[]);
        {
            let mut conn = db.lock();
            for l in [&files, &ids, &app] {
                save(&conn, l).unwrap();
            }
            db::save_game(&conn, &GamePatch::from(&game)).unwrap();
            // A Steam scan and a library folder scan find the game's own routes again (without
            // labels or assignments: scans never set them).
            let steam = steam_route();
            let mut local = local_route(&game_exe);
            local.install_states = Some([("root:r1".to_string(), true)].into());
            let found = Game {
                id: "scan".into(),
                routes: vec![steam, local],
                ..game_with("scan", vec![steam_route()])
            };
            db::upsert_imported_games(&mut conn, &[found]).unwrap();
            let now = db::get_game(&conn, "g").unwrap().unwrap();
            assert!(db::get_game(&conn, "scan").unwrap().is_none(), "merged");
            assert_eq!(now.routes.len(), assigned.len());
            for route in &assigned {
                let kept = now
                    .routes
                    .iter()
                    .find(|r| db::route_identity(r) == db::route_identity(route))
                    .unwrap();
                assert_eq!(kept.assigned, route.assigned, "{}", route.source_id);
                assert_eq!(kept.launcher_id, route.launcher_id, "{}", route.source_id);
                assert_eq!(kept.launch_target, route.launch_target);
            }
            // The store no longer lists the game as installed: its route stays (not
            // installed) because the game has an assignment, and so do the assignments.
            let other = Game {
                id: "other".into(),
                routes: vec![GameRoute {
                    app_id: Some("1".into()),
                    launch_target: "steam://rungameid/1".into(),
                    install_states: Some([("steam-installed".to_string(), true)].into()),
                    ..steam_route()
                }],
                ..game_with("other", vec![steam_route()])
            };
            db::upsert_imported_games(&mut conn, std::slice::from_ref(&other)).unwrap();
            let mut steam_state = db::get_game(&conn, "g").unwrap().unwrap();
            for r in steam_state
                .routes
                .iter_mut()
                .filter(|r| r.source_id == "steam")
            {
                r.install_states = Some([("steam-installed".to_string(), true)].into());
            }
            db::set_routes(&conn, "g", steam_state.routes.clone()).unwrap();
            db::reconcile_installed_source(&mut conn, "steam-installed", &[other]).unwrap();
            let after = db::get_game(&conn, "g").unwrap().unwrap();
            assert_eq!(after.routes.len(), assigned.len());
            assert_eq!(after.routes.iter().filter(|r| is_assigned(r)).count(), 3);
            let steam_now = after
                .routes
                .iter()
                .find(|r| r.source_id == "steam")
                .unwrap();
            assert_eq!(steam_now.installed, Some(false));
            // The library folder scan no longer finds the file: same.
            let (marked, removed) =
                db::mark_local_routes_missing(&mut conn, Some("root:r1"), &|_| true).unwrap();
            assert_eq!((marked, removed), (1, 0));
            let after = db::get_game(&conn, "g").unwrap().unwrap();
            assert_eq!(after.routes.iter().filter(|r| is_assigned(r)).count(), 3);
        }

        // Backups carry assignments and restore them.
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
                game_links: None,
                launchers: Some(list(&conn).unwrap()),
            }
        };
        let json = serde_json::to_value(&backup).unwrap();
        let games = json["games"].as_array().unwrap();
        let g = games.iter().find(|g| g["id"] == "g").unwrap();
        assert_eq!(
            g["routes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["assigned"] == true)
                .count(),
            3
        );
        let parsed = crate::validation::backup(&json).unwrap();
        let restored = Db::memory().unwrap();
        db::restore_backup(&mut restored.lock(), &parsed).unwrap();
        let conn = restored.lock();
        let back = db::get_game(&conn, "g").unwrap().unwrap();
        assert_eq!(back.routes.iter().filter(|r| is_assigned(r)).count(), 3);
        assert_eq!(games_of(&conn, "app").unwrap(), ["g"]);
    }

    #[test]
    fn the_running_launcher_is_matched_by_its_exact_path() {
        use crate::platform::ProcessInfo;
        let exe = Path::new(r"D:\Launchers\Heroic\Heroic.exe");
        let p = |path: &str| ProcessInfo {
            path: path.into(),
            pid: 1,
        };
        assert!(program_running(
            &[p(r"d:\launchers\heroic\heroic.exe")],
            exe
        ));
        assert!(program_running(&[p("D:/Launchers/Heroic/Heroic.exe")], exe));
        assert!(program_running(
            &[
                p(r"C:\Windows\explorer.exe"),
                p(r"D:\Launchers\Heroic\Heroic.exe")
            ],
            exe
        ));
        for other in [
            r"C:\Other\Heroic.exe",
            r"D:\Launchers\Heroic\Heroic.exe.old",
            r"D:\Launchers\Heroic\HeroicHelper.exe",
            "Heroic.exe",
            "",
        ] {
            assert!(!program_running(&[p(other)], exe), "{other}");
        }
        assert!(!program_running(&[], exe));
        assert!(!program_running(
            &[p("Heroic.exe")],
            Path::new("Heroic.exe")
        ));
    }
}
