//! Built-in data for local game and emulator support (pure, no I/O).
//!
//! - [`SYSTEMS`]: every supported system with its folder-name aliases and file formats.
//!   Aliases are matched case-, space- and punctuation-insensitively ([`alias_key`]), so
//!   `Sony 3`, `PlayStation 3`, `ps-3` and `PS3` all mean the PS3.
//! - [`EMULATORS`]: known emulator executables, the systems they run and an editable default
//!   argument template (see `launch::build_emulator_argv` for the template rules).
//! - Title helpers: [`clean_title`] strips region/revision/dump tags, [`disc_info`] finds
//!   `(Disc N)` markers, [`switch_extra_content`] recognises Switch updates and DLC.

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// One system. `extensions` are lower-case with a leading dot.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct System {
    pub id: &'static str,
    pub name: &'static str,
    /// Folder names that mean this system (matched with [`alias_key`]).
    pub aliases: &'static [&'static str],
    /// File formats collected for this system. Empty for `pc` (folders, not files).
    pub extensions: &'static [&'static str],
    /// Folder-based game formats this system recognises (see [`FolderFormat`]).
    #[serde(skip)]
    pub folder_formats: &'static [FolderFormat],
}

/// A game stored as a folder rather than a file. The folder is one game when one of
/// `markers` (paths relative to it, case-insensitive) exists; it launches `target`.
#[derive(Debug, Clone, Copy)]
pub struct FolderFormat {
    pub markers: &'static [&'static str],
    /// Relative path of the file to launch: the first of these that exists.
    pub targets: &'static [&'static str],
}

const PS3_FOLDER: FolderFormat = FolderFormat {
    markers: &[
        "PS3_DISC.SFB",
        "PS3_GAME/PARAM.SFO",
        "PS3_GAME/USRDIR/EBOOT.BIN",
        "USRDIR/EBOOT.BIN",
    ],
    targets: &["PS3_GAME/USRDIR/EBOOT.BIN", "USRDIR/EBOOT.BIN"],
};
const PS4_FOLDER: FolderFormat = FolderFormat {
    markers: &["sce_sys/param.sfo", "eboot.bin"],
    targets: &["eboot.bin"],
};
const WIIU_FOLDER: FolderFormat = FolderFormat {
    markers: &["code/app.xml", "meta/meta.xml"],
    targets: &[],
};
const X360_FOLDER: FolderFormat = FolderFormat {
    markers: &["default.xex"],
    targets: &["default.xex"],
};

const CART_ARCHIVES: &[&str] = &[".zip", ".7z"];

macro_rules! exts {
    ($($e:literal),* $(,)?) => { &[$($e),*] };
}

pub const PC: &str = "pc";

pub static SYSTEMS: &[System] = &[
    System {
        id: "ps1",
        name: "PlayStation",
        aliases: &[
            "PS1",
            "PSX",
            "PSOne",
            "PlayStation",
            "PlayStation 1",
            "Sony 1",
            "Sony PlayStation",
            "Sony PlayStation 1",
        ],
        extensions: exts![
            ".cue", ".chd", ".pbp", ".m3u", ".ccd", ".bin", ".img", ".iso", ".ecm", ".mds"
        ],
        folder_formats: &[],
    },
    System {
        id: "ps2",
        name: "PlayStation 2",
        aliases: &["PS2", "PlayStation 2", "Sony 2", "Sony PlayStation 2"],
        extensions: exts![
            ".iso", ".chd", ".cso", ".zso", ".cue", ".bin", ".gz", ".m3u", ".mdf", ".nrg"
        ],
        folder_formats: &[],
    },
    System {
        id: "ps3",
        name: "PlayStation 3",
        aliases: &["PS3", "PlayStation 3", "Sony 3", "Sony PlayStation 3"],
        extensions: exts![".iso", ".pkg"],
        folder_formats: &[PS3_FOLDER],
    },
    System {
        id: "ps4",
        name: "PlayStation 4",
        aliases: &["PS4", "PlayStation 4", "Sony 4", "Sony PlayStation 4"],
        extensions: exts![".pkg"],
        folder_formats: &[PS4_FOLDER],
    },
    System {
        id: "psp",
        name: "PlayStation Portable",
        aliases: &[
            "PSP",
            "PlayStation Portable",
            "Sony PSP",
            "Sony PlayStation Portable",
        ],
        extensions: exts![".iso", ".cso", ".chd", ".pbp", ".elf"],
        folder_formats: &[],
    },
    System {
        id: "psvita",
        name: "PlayStation Vita",
        aliases: &[
            "PS Vita",
            "PSVita",
            "Vita",
            "PSV",
            "PlayStation Vita",
            "Sony Vita",
        ],
        extensions: exts![".vpk"],
        folder_formats: &[],
    },
    System {
        id: "gc",
        name: "GameCube",
        aliases: &["GameCube", "GC", "NGC", "Nintendo GameCube"],
        extensions: exts![".iso", ".gcm", ".rvz", ".gcz", ".ciso", ".wia", ".dol", ".elf"],
        folder_formats: &[],
    },
    System {
        id: "wii",
        name: "Wii",
        aliases: &["Wii", "Nintendo Wii"],
        extensions: exts![".iso", ".wbfs", ".rvz", ".gcz", ".ciso", ".wia", ".wad"],
        folder_formats: &[],
    },
    System {
        id: "wiiu",
        name: "Wii U",
        aliases: &["Wii U", "WiiU", "Nintendo Wii U"],
        extensions: exts![".wud", ".wux", ".wua", ".rpx"],
        folder_formats: &[WIIU_FOLDER],
    },
    System {
        id: "switch",
        name: "Nintendo Switch",
        aliases: &["Switch", "Nintendo Switch", "NS", "NSW"],
        extensions: exts![".nsp", ".xci", ".nsz", ".xcz", ".nro"],
        folder_formats: &[],
    },
    System {
        id: "n3ds",
        name: "Nintendo 3DS",
        aliases: &["3DS", "N3DS", "Nintendo 3DS", "New 3DS"],
        extensions: exts![".3ds", ".cci", ".cxi", ".cia", ".3dsx", ".zcci", ".zcxi", ".z3dsx"],
        folder_formats: &[],
    },
    System {
        id: "nds",
        name: "Nintendo DS",
        aliases: &["DS", "NDS", "Nintendo DS", "DSi", "Nintendo DSi"],
        extensions: exts![".nds", ".dsi", ".srl", ".zip", ".7z"],
        folder_formats: &[],
    },
    System {
        id: "gba",
        name: "Game Boy Advance",
        aliases: &["GBA", "Game Boy Advance", "Gameboy Advance", "Nintendo GBA"],
        extensions: exts![".gba", ".agb", ".zip", ".7z"],
        folder_formats: &[],
    },
    System {
        id: "gb",
        name: "Game Boy / Game Boy Color",
        aliases: &[
            "GB",
            "GBC",
            "Game Boy",
            "Gameboy",
            "Game Boy Color",
            "Game Boy Colour",
            "Gameboy Color",
            "Gameboy Colour",
            "GB GBC",
            "Nintendo Game Boy",
        ],
        extensions: exts![".gb", ".gbc", ".sgb", ".zip", ".7z"],
        folder_formats: &[],
    },
    System {
        id: "n64",
        name: "Nintendo 64",
        aliases: &["N64", "Nintendo 64"],
        extensions: exts![".z64", ".n64", ".v64", ".zip", ".7z"],
        folder_formats: &[],
    },
    System {
        id: "snes",
        name: "Super Nintendo",
        aliases: &[
            "SNES",
            "Super Nintendo",
            "Super NES",
            "Super Famicom",
            "SFC",
            "Super Nintendo Entertainment System",
        ],
        extensions: exts![".sfc", ".smc", ".fig", ".swc", ".bs", ".zip", ".7z"],
        folder_formats: &[],
    },
    System {
        id: "nes",
        name: "NES",
        aliases: &[
            "NES",
            "Famicom",
            "FC",
            "Nintendo Entertainment System",
            "Famicom Disk System",
            "FDS",
        ],
        extensions: exts![".nes", ".fds", ".unf", ".unif", ".zip", ".7z"],
        folder_formats: &[],
    },
    System {
        id: "genesis",
        name: "Genesis / Mega Drive",
        aliases: &[
            "Genesis",
            "Mega Drive",
            "Megadrive",
            "MD",
            "Sega Genesis",
            "Sega Mega Drive",
            "Genesis Mega Drive",
        ],
        extensions: exts![".md", ".gen", ".smd", ".bin", ".zip", ".7z"],
        folder_formats: &[],
    },
    System {
        id: "dreamcast",
        name: "Dreamcast",
        aliases: &["Dreamcast", "DC", "Sega Dreamcast"],
        extensions: exts![".gdi", ".cdi", ".chd", ".cue", ".m3u"],
        folder_formats: &[],
    },
    System {
        id: "saturn",
        name: "Saturn",
        aliases: &["Saturn", "Sega Saturn"],
        extensions: exts![".cue", ".chd", ".ccd", ".mds", ".iso", ".m3u"],
        folder_formats: &[],
    },
    System {
        id: "xbox",
        name: "Xbox",
        aliases: &["Xbox", "Original Xbox", "Microsoft Xbox", "OG Xbox"],
        extensions: exts![".iso", ".xiso"],
        folder_formats: &[],
    },
    System {
        id: "xbox360",
        name: "Xbox 360",
        aliases: &["Xbox 360", "X360", "360", "Microsoft Xbox 360"],
        extensions: exts![".iso", ".xex", ".zar"],
        folder_formats: &[X360_FOLDER],
    },
    System {
        id: "arcade",
        name: "Arcade",
        aliases: &["Arcade", "MAME", "FBNeo", "FinalBurn Neo", "FBA", "Naomi"],
        extensions: CART_ARCHIVES,
        folder_formats: &[],
    },
    System {
        id: PC,
        name: "PC (Windows)",
        aliases: &[
            "PC",
            "Windows",
            "PC Game",
            "PC Games",
            "Windows Games",
            "Win",
            "Win32",
            "Win64",
        ],
        extensions: &[],
        folder_formats: &[],
    },
];

/// Lower-case letters and digits only: `"Sony 3"` → `"sony3"`, `"Wii-U"` → `"wiiu"`.
pub fn alias_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn system(id: &str) -> Option<&'static System> {
    SYSTEMS.iter().find(|s| s.id == id)
}

pub fn is_system_id(id: &str) -> bool {
    system(id).is_some()
}

/// The system a folder name stands for, if any (`"Sony 3"` → PS3).
pub fn system_for_folder(name: &str) -> Option<&'static System> {
    let key = alias_key(name);
    if key.is_empty() {
        return None;
    }
    SYSTEMS
        .iter()
        .find(|s| s.aliases.iter().any(|alias| alias_key(alias) == key))
}

/// Lower-case extension with a leading dot (`"Game.ISO"` → `".iso"`).
pub fn extension_of(file_name: &str) -> Option<String> {
    let (_, ext) = file_name.rsplit_once('.')?;
    (!ext.is_empty() && ext.len() <= 16).then(|| format!(".{}", ext.to_lowercase()))
}

pub fn systems_for_extension(ext: &str) -> Vec<&'static System> {
    SYSTEMS
        .iter()
        .filter(|s| s.extensions.contains(&ext))
        .collect()
}

/// Formats that only list other files (their referenced files are part of the same game).
pub const INDEX_FORMATS: &[&str] = &[".m3u", ".cue", ".gdi", ".ccd", ".mds"];

/// Files that are never games and are skipped silently (saves, art, notes, checksums).
pub const IGNORED_EXTENSIONS: &[&str] = &[
    ".txt", ".nfo", ".diz", ".md", ".pdf", ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp",
    ".sav", ".srm", ".state", ".sfv", ".md5", ".sha1", ".crc", ".xml", ".json", ".ini", ".cfg",
    ".log", ".db", ".dat", ".url", ".lnk", ".sub", ".sbi", ".mdf", ".ecm", ".bak", ".htm", ".html",
    ".dll", ".exe",
];

// ---------------------------------------------------------------------------
// Known emulators

/// A known emulator. `executables` are lower-case file names matched exactly; `prefixes`
/// match any `<prefix>*.exe` that isn't a helper (see [`HELPER_EXE_WORDS`]).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownEmulator {
    pub id: &'static str,
    pub name: &'static str,
    /// Lower-case executable file names, preferred first (a GUI build before a `nogui` one).
    pub executables: &'static [&'static str],
    /// Lower-case file name prefixes (`desmume` matches `desmume_x432r_x64.exe`).
    pub prefixes: &'static [&'static str],
    /// Systems in preference order relative to other emulators: see [`preference`].
    pub systems: &'static [&'static str],
    /// Default argument template (`{rom}`, `{romdir}`, `{romname}`); editable per profile.
    pub default_args: &'static str,
    /// The default template can't be trusted as is: such a profile is never picked
    /// automatically and won't launch until its arguments are changed (see
    /// `emulators::needs_configuration`).
    pub needs_configuration: bool,
    pub note: &'static str,
}

/// RetroArch's template placeholder that must be replaced with a real core.
pub const RETROARCH_CORE_PLACEHOLDER: &str = "CORE_NAME";

/// Words that mark an executable as a helper, never the emulator itself (prefix matches only;
/// exact names are listed explicitly).
pub const HELPER_EXE_WORDS: &[&str] = &[
    "update", "uninst", "unins", "setup", "install", "room", "cli", "cmd", "tool", "launcher",
    "helper", "crash", "debug", "server", "nogui", "headless", "manager",
];

/// Folder names an emulator scan never enters: emulator data (virtual consoles' drives,
/// shader and save data, cores, assets, ...) that can hold tens of thousands of files and
/// never contains another emulator's executable. Lower-case.
pub const HEAVY_EMULATOR_DIRS: &[&str] = &[
    "dev_hdd0",
    "dev_hdd1",
    "dev_flash",
    "dev_flash2",
    "dev_flash3",
    "dev_usb000",
    "dev_usb",
    "dev_bdvd",
    "dev_blind",
    "shaders",
    "shader",
    "shadercache",
    "shader_cache",
    "cache",
    "caches",
    "pipelinecache",
    "cores",
    "assets",
    "system",
    "saves",
    "savestates",
    "states",
    "sstates",
    "screenshots",
    "snaps",
    "thumbnails",
    "nand",
    "sdmc",
    "user",
    "users",
    "playlists",
    "database",
    "overlays",
    "autoconfig",
    "downloads",
    "logs",
    "memcards",
    "games",
    "roms",
    "isos",
    "textures",
    "load",
    "dump",
    "dumps",
    "covers",
    "cheats",
    "gamesettings",
    "inputprofiles",
    "resources",
    "translations",
    "locale",
    "lang",
    "languages",
    "data",
    "qt",
    "plugins",
    "platforms",
    "imageformats",
    "styles",
    "iconengines",
    "__pycache__",
    "node_modules",
];

/// File names skipped everywhere (Windows folder metadata).
pub const SKIPPED_FILE_NAMES: &[&str] = &["desktop.ini", "thumbs.db"];

macro_rules! emu {
    ($id:literal, $name:literal, [$($exe:literal),* $(,)?], [$($prefix:literal),* $(,)?],
     [$($sys:literal),* $(,)?], $args:literal, $needs:literal, $note:literal) => {
        KnownEmulator {
            id: $id,
            name: $name,
            executables: &[$($exe),*],
            prefixes: &[$($prefix),*],
            systems: &[$($sys),*],
            default_args: $args,
            needs_configuration: $needs,
            note: $note,
        }
    };
}

/// Catalog order is also the default-emulator preference within a system (see
/// [`preference`]): dedicated emulators first, multi-system ones (BizHawk, Mednafen) after,
/// RetroArch last.
pub static EMULATORS: &[KnownEmulator] = &[
    emu!("dolphin", "Dolphin", ["dolphin.exe"], [], ["gc", "wii"], "-b -e {rom}", false,
        "GameCube and Wii."),
    emu!("pcsx2", "PCSX2",
        ["pcsx2-qtx64-avx2.exe", "pcsx2-qtx64.exe", "pcsx2-qt.exe", "pcsx2.exe", "pcsx2x64.exe",
         "pcsx2x64-avx2.exe"], [],
        ["ps2"], "-batch -- {rom}", false, "PlayStation 2."),
    emu!("rpcs3", "RPCS3", ["rpcs3.exe"], [], ["ps3"], "--no-gui {rom}", false,
        "PlayStation 3. Folder games launch their EBOOT.BIN."),
    emu!("duckstation", "DuckStation",
        ["duckstation-qt-x64-releaseltcg.exe", "duckstation-qt-arm64-releaseltcg.exe",
         "duckstation-qt.exe", "duckstation.exe", "duckstation-nogui-x64-releaseltcg.exe",
         "duckstation-nogui.exe"], [],
        ["ps1"], "-batch -- {rom}", false, "PlayStation."),
    emu!("ppsspp", "PPSSPP", ["ppssppwindows64.exe", "ppssppwindows.exe", "ppssppwindowsarm64.exe"],
        [], ["psp"], "{rom}", false, "PlayStation Portable."),
    emu!("ryujinx", "Ryujinx", ["ryujinx.exe", "ryujinx.ava.exe"], [], ["switch"], "{rom}", false,
        "Nintendo Switch (Ryujinx and forks such as Ryubing)."),
    emu!("eden", "Eden", ["eden.exe"], [], ["switch"], "-g {rom}", false, "Nintendo Switch."),
    emu!("citron", "Citron", ["citron.exe"], [], ["switch"], "-g {rom}", false, "Nintendo Switch."),
    emu!("eglantine", "Eglantine", ["eglantine.exe"], [], ["switch"], "-g {rom}", false,
        "Nintendo Switch (Eden-based)."),
    emu!("lichii", "Lichii", ["lichii.exe"], [], ["switch"], "-g {rom}", false,
        "Nintendo Switch (Eden-based)."),
    emu!("orchid", "Orchid", ["orchid.exe"], [], ["switch"], "-g {rom}", false,
        "Nintendo Switch (Eden-based)."),
    emu!("selevet", "Selevet", ["selevet.exe"], [], ["switch"], "-g {rom}", false,
        "Nintendo Switch (Eden-based)."),
    emu!("sudachi", "Sudachi", ["sudachi.exe"], [], ["switch"], "-g {rom}", false,
        "Nintendo Switch."),
    emu!("suyu", "suyu", ["suyu.exe"], [], ["switch"], "-g {rom}", false, "Nintendo Switch."),
    emu!("torzu", "torzu", ["torzu.exe"], [], ["switch"], "-g {rom}", false, "Nintendo Switch."),
    emu!("yuzu", "yuzu", ["yuzu.exe"], [], ["switch"], "-g {rom}", false, "Nintendo Switch."),
    emu!("cemu", "Cemu", ["cemu.exe"], [], ["wiiu"], "-g {rom}", false, "Wii U."),
    emu!("xemu", "xemu", ["xemu.exe"], [], ["xbox"], "-dvd_path {rom}", false,
        "Original Xbox (needs its BIOS files configured in xemu)."),
    emu!("xenia-canary", "Xenia Canary", ["xenia_canary.exe"], [], ["xbox360"], "{rom}", false,
        "Xbox 360."),
    emu!("xenia", "Xenia", ["xenia.exe"], [], ["xbox360"], "{rom}", false, "Xbox 360."),
    emu!("melonds", "melonDS", ["melonds.exe"], [], ["nds"], "{rom}", false, "Nintendo DS."),
    emu!("desmume", "DeSmuME", ["desmume.exe"], ["desmume"], ["nds"], "{rom}", false,
        "Nintendo DS."),
    emu!("azahar", "Azahar", ["azahar.exe"], [], ["n3ds"], "{rom}", false, "Nintendo 3DS."),
    emu!("lime3ds", "Lime3DS", ["lime3ds.exe", "lime3ds-gui.exe", "lime3ds-qt.exe"], [], ["n3ds"],
        "{rom}", false, "Nintendo 3DS."),
    emu!("citra", "Citra", ["citra-qt.exe", "citra.exe"], [], ["n3ds"], "{rom}", false,
        "Nintendo 3DS."),
    emu!("mgba", "mGBA", ["mgba.exe"], [], ["gba", "gb"], "{rom}", false,
        "Game Boy Advance, Game Boy and Game Boy Color."),
    emu!("vbam", "VisualBoyAdvance-M", ["visualboyadvance-m.exe", "vbam.exe"], [], ["gba", "gb"],
        "{rom}", false, "Game Boy Advance, Game Boy and Game Boy Color."),
    emu!("project64", "Project64", ["project64.exe"], [], ["n64"], "{rom}", false,
        "Nintendo 64."),
    emu!("snes9x", "Snes9x", ["snes9x-x64.exe", "snes9x.exe"], [], ["snes"], "{rom}", false,
        "Super Nintendo."),
    emu!("bsnes", "bsnes", ["bsnes.exe", "bsnes_hd.exe"], [], ["snes"], "{rom}", false,
        "Super Nintendo (bsnes and bsnes-hd)."),
    emu!("mesen", "Mesen", ["mesen.exe"], [], ["nes", "snes", "gb"], "{rom}", false,
        "NES, SNES and Game Boy (Mesen 2)."),
    emu!("fceux", "FCEUX", ["fceux64.exe", "fceux.exe"], [], ["nes"], "{rom}", false, "NES."),
    emu!("kega-fusion", "Kega Fusion", ["fusion.exe"], [], ["genesis"], "{rom}", false,
        "Genesis / Mega Drive."),
    emu!("flycast", "Flycast", ["flycast.exe"], [], ["dreamcast"], "{rom}", false, "Dreamcast."),
    emu!("redream", "Redream", ["redream.exe"], [], ["dreamcast"], "{rom}", false, "Dreamcast."),
    emu!("vita3k", "Vita3K", ["vita3k.exe"], [], ["psvita"], "{rom}", false,
        "PlayStation Vita. Opening a .vpk installs it in Vita3K."),
    emu!("shadps4", "shadPS4", ["shadps4.exe"], [], ["ps4"], "-g {rom}", false,
        "PlayStation 4. Folder games launch their eboot.bin."),
    emu!("fbneo", "FinalBurn Neo", ["fbneo64.exe", "fbneo.exe"], [], ["arcade"], "{romname}", true,
        "Arcade. FinalBurn Neo starts a game by its ROM set name and looks for it in its own ROM folders: add your arcade folder in FinalBurn Neo first, then confirm the arguments here."),
    emu!("bizhawk", "BizHawk", ["emuhawk.exe"], [],
        ["nes", "snes", "gb", "gba", "n64", "nds", "genesis", "ps1", "saturn"], "{rom}", false,
        "Many systems; dedicated emulators are preferred when present."),
    emu!("mednafen", "Mednafen", ["mednafen.exe"], [],
        ["ps1", "saturn", "nes", "snes", "gba", "gb", "genesis"], "{rom}", false,
        "Many systems; dedicated emulators are preferred when present."),
    emu!("retroarch", "RetroArch", ["retroarch.exe"], [],
        ["nes", "snes", "gb", "gba", "n64", "nds", "genesis", "ps1", "psp", "saturn",
         "dreamcast", "arcade"],
        "-L cores\\CORE_NAME_libretro.dll {rom}", true,
        "Needs a core: replace CORE_NAME in the arguments (e.g. snes9x, mgba, swanstation)."),
];

/// The known emulator an executable file name belongs to (`"Dolphin.exe"` → Dolphin), and
/// the index of the matched name in its `executables` (prefix matches rank after them).
pub fn match_executable(file_name: &str) -> Option<(&'static KnownEmulator, usize)> {
    let lower = file_name.to_lowercase();
    if let Some((known, index)) = EMULATORS.iter().find_map(|e| {
        e.executables
            .iter()
            .position(|x| *x == lower)
            .map(|i| (e, i))
    }) {
        return Some((known, index));
    }
    let stem = lower.strip_suffix(".exe")?;
    if HELPER_EXE_WORDS.iter().any(|w| stem.contains(w)) {
        return None;
    }
    EMULATORS
        .iter()
        .find(|e| e.prefixes.iter().any(|p| stem.starts_with(p)))
        .map(|e| (e, e.executables.len()))
}

/// The known emulator an executable file name belongs to (`"Dolphin.exe"` → Dolphin).
pub fn emulator_for_executable(file_name: &str) -> Option<&'static KnownEmulator> {
    match_executable(file_name).map(|(known, _)| known)
}

pub fn known_emulator(id: &str) -> Option<&'static KnownEmulator> {
    EMULATORS.iter().find(|e| e.id == id)
}

/// Ranking used to pick a system's default emulator: dedicated emulators first, in catalog
/// order; RetroArch last. Lower is better; `None` when the emulator doesn't run the system.
pub fn preference(emulator_id: &str, system_id: &str) -> Option<usize> {
    let index = EMULATORS
        .iter()
        .filter(|e| e.systems.contains(&system_id))
        .position(|e| e.id == emulator_id)?;
    Some(index + if emulator_id == "retroarch" { 100 } else { 0 })
}

// ---------------------------------------------------------------------------
// Titles

static TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*(\([^()]*\)|\[[^\[\]]*\])").unwrap());
static DISC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\(\[]\s*(?:disc|disk|cd|dvd)\s*([0-9]{1,2})(?:\s*of\s*[0-9]{1,2})?\s*[\)\]]")
        .unwrap()
});
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static SWITCH_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[([0-9a-f]{16})\]").unwrap());
static SWITCH_EXTRA_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\(\[]\s*(?:upd|update|dlc|patch|add-?on)\b[^\)\]]*[\)\]]").unwrap()
});

/// A display title from a file or folder name (no extension): tags in `()` and `[]` are
/// removed (`(USA)`, `(Rev 1)`, `[!]`, `(Disc 1)`), `_` becomes a space, spaces collapse.
/// Falls back to the trimmed name when nothing would be left.
pub fn clean_title(stem: &str) -> String {
    let without = TAG.replace_all(stem, "");
    let spaced = without.replace('_', " ");
    let title = SPACES.replace_all(spaced.trim(), " ").trim().to_string();
    let title = title.trim_end_matches([' ', '-', ',', '.']).to_string();
    if title.is_empty() {
        let fallback = SPACES.replace_all(stem.trim(), " ").to_string();
        if fallback.is_empty() {
            "Untitled".into()
        } else {
            fallback
        }
    } else {
        title
    }
}

/// Key used to spot the same game in different files: the clean title, letters and digits
/// only, lower case.
pub fn title_key(title: &str) -> String {
    alias_key(&clean_title(title))
}

/// `(Disc 2)` style marker: (disc number, the name with the marker removed).
pub fn disc_info(stem: &str) -> Option<(u32, String)> {
    let caps = DISC.captures(stem)?;
    let number = caps[1].parse().ok()?;
    let rest = DISC.replace(stem, "");
    Some((number, SPACES.replace_all(rest.trim(), " ").to_string()))
}

/// True for Switch update or DLC files: a `[UPD]`/`[DLC]`/`(Update)` tag, or a 16-digit title
/// ID that isn't a base game (base IDs end in `000`; updates end in `800`).
pub fn switch_extra_content(stem: &str) -> bool {
    if SWITCH_EXTRA_WORD.is_match(stem) {
        return true;
    }
    SWITCH_ID
        .captures(stem)
        .is_some_and(|c| !c[1].to_lowercase().ends_with("000"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_aliases_ignore_case_spaces_and_punctuation() {
        for (name, id) in [
            ("Sony 3", "ps3"),
            ("Sony Playstation", "ps1"),
            ("Sony Playstation 2", "ps2"),
            ("Sony Playstation 3", "ps3"),
            ("Sony Playstation 4", "ps4"),
            ("Sony Playstation Portable", "psp"),
            ("sony   playstation portable", "psp"),
            ("PC Game", "pc"),
            ("PC Games", "pc"),
            ("pcgame", "pc"),
            ("PlayStation 3", "ps3"),
            ("ps3", "ps3"),
            ("PS-3", "ps3"),
            ("  playstation_3 ", "ps3"),
            ("Switch", "switch"),
            ("Nintendo Switch", "switch"),
            ("NS", "switch"),
            ("nintendo-switch", "switch"),
            ("PSX", "ps1"),
            ("Sony 2", "ps2"),
            ("PS Vita", "psvita"),
            ("GameCube", "gc"),
            ("Wii U", "wiiu"),
            ("WiiU", "wiiu"),
            ("Wii", "wii"),
            ("3DS", "n3ds"),
            ("Nintendo DS", "nds"),
            ("Game Boy Advance", "gba"),
            ("GBC", "gb"),
            ("Game Boy Color", "gb"),
            ("N64", "n64"),
            ("Super Famicom", "snes"),
            ("NES", "nes"),
            ("Mega Drive", "genesis"),
            ("Sega Genesis", "genesis"),
            ("Dreamcast", "dreamcast"),
            ("Sega Saturn", "saturn"),
            ("Xbox", "xbox"),
            ("Xbox 360", "xbox360"),
            ("MAME", "arcade"),
            ("PC", "pc"),
            ("Windows", "pc"),
        ] {
            assert_eq!(system_for_folder(name).map(|s| s.id), Some(id), "{name}");
        }
        for name in ["Wii Sports", "Sony", "Nintendo", "Roms", "", "---", "PS5"] {
            assert!(system_for_folder(name).is_none(), "{name}");
        }
    }

    #[test]
    fn catalog_is_consistent() {
        let mut ids = std::collections::HashSet::new();
        let mut aliases = std::collections::HashMap::new();
        for s in SYSTEMS {
            assert!(ids.insert(s.id), "duplicate system {}", s.id);
            for alias in s.aliases {
                let owner = aliases.entry(alias_key(alias)).or_insert(s.id);
                assert_eq!(*owner, s.id, "alias {alias} names two systems");
            }
            for ext in s.extensions {
                assert!(ext.starts_with('.') && *ext == ext.to_lowercase(), "{ext}");
            }
        }
        let mut exes = std::collections::HashSet::new();
        for e in EMULATORS {
            assert!(!e.systems.is_empty());
            for s in e.systems {
                assert!(is_system_id(s), "{} lists unknown system {s}", e.id);
            }
            for prefix in e.prefixes {
                assert_eq!(*prefix, prefix.to_lowercase());
            }
            for exe in e.executables {
                assert!(exe.ends_with(".exe") && *exe == exe.to_lowercase());
                assert!(exes.insert(*exe), "{exe} listed twice");
            }
            assert!(
                e.default_args.contains("{rom"),
                "{} template lacks {{rom}}",
                e.id
            );
            assert!(crate::launch::tokenize_template(e.default_args).is_ok());
        }
        assert!(known_emulator("retroarch").unwrap().needs_configuration);
    }

    /// Discovered profiles (emulator roots and launcher games folders) are saved with these
    /// defaults without asking: each must pass the same checks a template the user typed does.
    #[test]
    fn every_default_template_passes_the_template_checks() {
        for e in EMULATORS {
            let exe = format!(r"D:\Emulators\{}\{}", e.name, e.executables[0]);
            crate::launch::check_template(e.default_args, &exe)
                .unwrap_or_else(|err| panic!("{}: {err:#}", e.id));
            crate::launch::check_no_debugging_options(e.default_args, "emulator")
                .unwrap_or_else(|err| panic!("{}: {err:#}", e.id));
        }
    }

    #[test]
    fn executables_map_to_known_emulators() {
        let id = |name: &str| emulator_for_executable(name).map(|e| e.id);
        for (exe, expected) in [
            ("Dolphin.exe", "dolphin"),
            ("pcsx2-qtx64-avx2.exe", "pcsx2"),
            ("pcsx2-qt.exe", "pcsx2"),
            ("duckstation-qt-x64-ReleaseLTCG.exe", "duckstation"),
            ("duckstation-nogui-x64-ReleaseLTCG.exe", "duckstation"),
            ("PPSSPPWindows64.exe", "ppsspp"),
            ("Ryujinx.exe", "ryujinx"),
            ("citra-qt.exe", "citra"),
            ("citron.exe", "citron"),
            ("eden.exe", "eden"),
            ("eglantine.exe", "eglantine"),
            ("lichii.exe", "lichii"),
            ("orchid.exe", "orchid"),
            ("selevet.exe", "selevet"),
            ("yuzu.exe", "yuzu"),
            ("shadPS4.exe", "shadps4"),
            ("xenia_canary.exe", "xenia-canary"),
            ("retroarch.exe", "retroarch"),
            ("DeSmuME_X432R_x64.exe", "desmume"),
            ("DeSmuME.exe", "desmume"),
            ("EmuHawk.exe", "bizhawk"),
            ("fceux64.exe", "fceux"),
            ("bsnes_hd.exe", "bsnes"),
            ("Fusion.exe", "kega-fusion"),
            ("mednafen.exe", "mednafen"),
            ("redream.exe", "redream"),
            ("visualboyadvance-m.exe", "vbam"),
            ("fbneo64.exe", "fbneo"),
            ("Mesen.exe", "mesen"),
            ("Vita3K.exe", "vita3k"),
            ("Cemu.exe", "cemu"),
            ("azahar.exe", "azahar"),
            ("melonDS.exe", "melonds"),
            ("Project64.exe", "project64"),
            ("snes9x-x64.exe", "snes9x"),
            ("xemu.exe", "xemu"),
        ] {
            assert_eq!(id(exe), Some(expected), "{exe}");
        }
        for exe in [
            "DolphinTool.exe",
            "yuzu-cmd.exe",
            "eden-cli.exe",
            "dolphin.dll",
            "XeniaManager.exe",
            "GR2Launcher.exe",
            "blueMSX.exe",
            "kyty_emulator.exe",
            "SharpEmu.exe",
            "RegroupLogs.exe",
            "desmume-updater.exe",
            "desmume_uninstall.exe",
            "DeSmuME-cli.exe",
            "desmume_room.exe",
            "Mesen.2.1.exe",
        ] {
            assert_eq!(id(exe), None, "{exe}");
        }
        // The GUI build ranks before the no-GUI one.
        let (_, qt) = match_executable("duckstation-qt-x64-ReleaseLTCG.exe").unwrap();
        let (_, nogui) = match_executable("duckstation-nogui-x64-ReleaseLTCG.exe").unwrap();
        assert!(qt < nogui);
    }

    #[test]
    fn default_emulator_preference() {
        assert!(preference("mgba", "gba").unwrap() < preference("retroarch", "gba").unwrap());
        assert!(preference("ryujinx", "switch").unwrap() < preference("yuzu", "switch").unwrap());
        assert!(preference("mgba", "gb").unwrap() < preference("mesen", "gb").unwrap());
        assert_eq!(preference("dolphin", "ps2"), None);
        assert_eq!(preference("nope", "ps2"), None);
    }

    #[test]
    fn titles_drop_tags_and_find_discs() {
        assert_eq!(clean_title("Super Mario World (USA)"), "Super Mario World");
        assert_eq!(clean_title("Zelda (Europe) (Rev 1) [!]"), "Zelda");
        assert_eq!(
            clean_title("Final_Fantasy_VII (Disc 1)"),
            "Final Fantasy VII"
        );
        assert_eq!(clean_title("[BLUS30443] Demon's Souls"), "Demon's Souls");
        assert_eq!(clean_title("(USA)"), "(USA)");
        assert_eq!(clean_title("Game - "), "Game");
        assert_eq!(title_key("Zelda (USA)"), title_key("ZELDA (Europe)"));
        assert_eq!(
            disc_info("Final Fantasy VII (USA) (Disc 2)"),
            Some((2, "Final Fantasy VII (USA)".into()))
        );
        assert_eq!(disc_info("Game [CD3 of 4]").unwrap().0, 3);
        assert_eq!(
            disc_info("Game (Disk 1 of 2) (USA)").unwrap().1,
            "Game (USA)"
        );
        assert_eq!(disc_info("Game (USA)"), None);
    }

    #[test]
    fn switch_updates_and_dlc_are_recognised() {
        assert!(!switch_extra_content("Zelda [01007EF00011E000][v0]"));
        assert!(switch_extra_content("Zelda [01007EF00011E800][v65536]"));
        assert!(switch_extra_content(
            "Zelda DLC Pack [01007EF00011F001][v0]"
        ));
        assert!(switch_extra_content("Zelda (Update 1.6.0)"));
        assert!(switch_extra_content("Zelda [UPD]"));
        assert!(switch_extra_content("Zelda [DLC]"));
        assert!(!switch_extra_content("Updated Adventure"));
        assert!(!switch_extra_content("Zelda"));
    }

    #[test]
    fn extensions_are_lower_cased() {
        assert_eq!(extension_of("Game.ISO").as_deref(), Some(".iso"));
        assert_eq!(extension_of("noext"), None);
        assert_eq!(extension_of("trailing."), None);
        assert!(systems_for_extension(".nsp")
            .iter()
            .any(|s| s.id == "switch"));
        assert!(systems_for_extension(".iso").len() > 3);
    }
}
