//! Windows integration: process listing, ShellExecute, DPAPI, detached spawning.
//! Non-Windows builds compile with inert fallbacks so the crate still type-checks.

use anyhow::{anyhow, bail, Result};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Runs `f` on a fresh OS thread and awaits it. Used for COM/WinRT and other blocking
/// calls so no thread-pool thread is left in a particular COM apartment.
pub async fn on_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await
        .map_err(|_| anyhow!("Background task stopped unexpectedly."))
}

/// `path.resolve` equivalent: absolute, `.`/`..` collapsed, separators normalized.
pub fn resolve(path: &str) -> PathBuf {
    let absolute =
        std::path::absolute(path.replace('/', "\\")).unwrap_or_else(|_| PathBuf::from(path));
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

// Path containment for process tracking lives in `tracker` (`normalize_path`, `is_within`).

/// `encodeURIComponent` equivalent.
pub fn encode_uri_component(value: &str) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');
    percent_encoding::utf8_percent_encode(value, SET).to_string()
}

/// Runs a console program without a window and returns its stdout.
pub async fn run_capture(
    program: &Path,
    args: &[&str],
    timeout: Duration,
    max_bytes: usize,
) -> Result<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| anyhow!("{} timed out.", program.display()))??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} failed: {}", program.display(), stderr.trim());
    }
    if output.stdout.len() > max_bytes {
        bail!("{} returned too much output.", program.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Starts a program detached from Game Lobby, so closing the launcher doesn't close the game.
pub fn spawn_detached(program: &Path, args: &[String]) -> Result<()> {
    let cwd = program.parent().filter(|d| d.is_dir());
    spawn_detached_in(program, args, cwd)
        .map(drop)
        .map_err(|e| anyhow!("Could not start {}: {e}", program.display()))
}

/// Like [`spawn_detached`] with an explicit working folder; returns the new process id.
/// Arguments are passed as separate argv entries (Rust quotes each one for
/// `CommandLineToArgvW`); no shell is involved, so `&`, `%`, `"` or spaces in an argument
/// can't start other commands or split it. Callers must only pass `.exe` programs: Windows
/// runs `.bat`/`.cmd` files through `cmd.exe`.
pub fn spawn_detached_in(
    program: &Path,
    args: &[String],
    cwd: Option<&Path>,
) -> std::io::Result<u32> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    command.spawn().map(|child| child.id())
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens a URL, file, or folder with its registered handler (ShellExecute "open").
/// Runs on its own STA thread, as the shell expects.
pub async fn shell_open(target: String) -> Result<()> {
    on_thread(move || shell_open_blocking(&target)).await?
}

#[cfg(windows)]
fn shell_open_blocking(target: &str) -> Result<()> {
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let verb = wide("open");
    let file = wide(target);
    unsafe {
        let com = CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        ) as isize;
        if com >= 0 {
            CoUninitialize();
        }
        if result <= 32 {
            bail!("Windows could not open {target} (error {result}).");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn shell_open_blocking(target: &str) -> Result<()> {
    bail!("Opening {target} is only supported on Windows.")
}

/// A running process: its id and full executable path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessInfo {
    pub path: String,
    pub pid: u32,
}

/// All running processes this user can query, sorted by path.
#[cfg(windows)]
pub fn running_processes() -> Result<Vec<ProcessInfo>> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let mut pids = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            bail!(
                "CreateToolhelp32Snapshot failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID != 0 {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    let mut processes = Vec::with_capacity(pids.len());
    let mut buffer = vec![0u16; 32_768];
    for pid in pids {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                continue;
            }
            let mut length = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut length,
            );
            CloseHandle(handle);
            if ok != 0 && length > 0 {
                processes.push(ProcessInfo {
                    path: String::from_utf16_lossy(&buffer[..length as usize]),
                    pid,
                });
            }
        }
    }
    processes.sort_unstable();
    Ok(processes)
}

#[cfg(not(windows))]
pub fn running_processes() -> Result<Vec<ProcessInfo>> {
    Ok(Vec::new())
}

/// What process tracking needs to know about one top-level window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowInfo {
    pub pid: u32,
    pub visible: bool,
    /// `WS_EX_TOOLWINDOW`: palettes, tray helpers; never on the taskbar.
    pub tool_window: bool,
    /// `WS_EX_APPWINDOW`: forced onto the taskbar even when owned.
    pub app_window: bool,
    /// Has an owner window (dialogs, popups of another window).
    pub owned: bool,
    /// Hidden by DWM (`DWMWA_CLOAKED`): suspended UWP apps, other virtual desktops' shells.
    pub cloaked: bool,
    pub minimized: bool,
    pub width: i32,
    pub height: i32,
}

/// True for a window a user is actually playing in: visible, not cloaked, not a tool window,
/// a main (owner-less or app) window, with a real size. Minimized windows still count.
pub fn is_play_window(w: &WindowInfo) -> bool {
    w.visible
        && !w.cloaked
        && !w.tool_window
        && (!w.owned || w.app_window)
        && (w.minimized || (w.width > 0 && w.height > 0))
}

/// Ids of processes that own at least one [`is_play_window`] window (one `EnumWindows` pass).
pub fn processes_with_play_windows() -> std::collections::HashSet<u32> {
    top_level_windows()
        .iter()
        .filter(|w| is_play_window(w))
        .map(|w| w.pid)
        .collect()
}

#[cfg(windows)]
fn top_level_windows() -> Vec<WindowInfo> {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let windows = &mut *(lparam as *mut Vec<WindowInfo>);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let visible = IsWindowVisible(hwnd) != 0;
        // Most top-level windows are hidden; skip the remaining queries for them.
        if pid == 0 || !visible {
            return 1;
        }
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let mut cloaked = 0u32;
        let cloak_ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        ) >= 0;
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        windows.push(WindowInfo {
            pid,
            visible,
            tool_window: ex_style & WS_EX_TOOLWINDOW != 0,
            app_window: ex_style & WS_EX_APPWINDOW != 0,
            owned: !GetWindow(hwnd, GW_OWNER).is_null(),
            cloaked: cloak_ok && cloaked != 0,
            minimized: IsIconic(hwnd) != 0,
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
        });
        1
    }

    let mut windows: Vec<WindowInfo> = Vec::new();
    unsafe {
        EnumWindows(
            Some(collect),
            &mut windows as *mut Vec<WindowInfo> as LPARAM,
        );
    }
    windows
}

#[cfg(not(windows))]
fn top_level_windows() -> Vec<WindowInfo> {
    Vec::new()
}

/// Encrypts with Windows DPAPI for the current user.
#[cfg(windows)]
pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi(data, true)
}

#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi(data, false)
}

#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            bail!(
                "Windows secure storage failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        Ok(result)
    }
}

#[cfg(not(windows))]
pub fn protect(_: &[u8]) -> Result<Vec<u8>> {
    bail!("Secure storage is only available on Windows.")
}

#[cfg(not(windows))]
pub fn unprotect(_: &[u8]) -> Result<Vec<u8>> {
    bail!("Secure storage is only available on Windows.")
}

// ---------------------------------------------------------------------------
// Shortcuts and drives

/// Largest `.lnk` / `.url` file that is read.
pub const MAX_SHORTCUT_BYTES: u64 = 64 * 1024;

/// The `URL=` value of an Internet shortcut's `[InternetShortcut]` section. The caller must
/// still validate it (`launch::validate_launch_uri`).
pub fn read_url_shortcut(path: &Path) -> Result<String> {
    if std::fs::metadata(path)?.len() > MAX_SHORTCUT_BYTES {
        bail!("{} is too large for a shortcut.", path.display());
    }
    parse_url_shortcut(&String::from_utf8_lossy(&std::fs::read(path)?))
        .ok_or_else(|| anyhow!("{} has no link.", path.display()))
}

pub fn parse_url_shortcut(text: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case("[InternetShortcut]");
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("URL") {
                let value = value.trim();
                return (!value.is_empty()).then(|| value.to_string());
            }
        }
    }
    None
}

/// A Windows shortcut's (`.lnk`) target path and arguments, read with `IShellLinkW` without
/// resolving it (no search, no UI).
#[cfg(windows)]
pub fn resolve_shortcut(path: &Path) -> Result<(String, String)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    if std::fs::metadata(path)?.len() > MAX_SHORTCUT_BYTES {
        bail!("{} is too large for a shortcut.", path.display());
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let from_wide = |buffer: &[u16]| {
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    };
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let result = (|| -> windows::core::Result<(String, String)> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let file: IPersistFile = link.cast()?;
            file.Load(PCWSTR(wide.as_ptr()), STGM_READ)?;
            let mut target = vec![0u16; 32_768];
            link.GetPath(&mut target, std::ptr::null_mut(), 0)?;
            let mut arguments = vec![0u16; 32_768];
            link.GetArguments(&mut arguments)?;
            Ok((from_wide(&target), from_wide(&arguments)))
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result.map_err(|e| anyhow!("{} can't be read as a shortcut: {e}", path.display()))
    }
}

#[cfg(not(windows))]
pub fn resolve_shortcut(path: &Path) -> Result<(String, String)> {
    bail!("{} can't be read as a shortcut here.", path.display())
}

/// Writes a `.lnk` (tests only).
#[cfg(all(windows, test))]
pub fn create_shortcut(path: &Path, target: &str, arguments: &str) -> Result<()> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| -> windows::core::Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(target))?;
            link.SetArguments(&HSTRING::from(arguments))?;
            let file: IPersistFile = link.cast()?;
            file.Save(&HSTRING::from(path.as_os_str()), true)?;
            Ok(())
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result.map_err(|e| anyhow!("{e}"))
    }
}

/// True when the drive of an absolute `X:\...` path is a network drive (`DRIVE_REMOTE`).
#[cfg(windows)]
pub fn is_remote_drive(path: &str) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOTE: u32 = 4;
    let bytes = path.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false;
    }
    let root = wide(&format!("{}:\\", bytes[0] as char));
    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
}

#[cfg(not(windows))]
pub fn is_remote_drive(_path: &str) -> bool {
    false
}

/// True when `path` itself (not its target) is a symbolic link, junction or other reparse
/// point.
pub fn is_reparse_point(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Version resources larger than this are not read.
const MAX_VERSION_INFO_BYTES: u32 = 1024 * 1024;

/// A version-resource name that can serve as a game title: 1 to 100 characters, with a
/// letter, and not an engine, runtime or operating-system name.
pub fn sensible_product_name(raw: &str) -> Option<String> {
    let name = raw.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 100 || !name.chars().any(char::is_alphabetic) {
        return None;
    }
    let lower = name.to_lowercase();
    const GENERIC: &[&str] = &[
        "unity",
        "unityplayer",
        "unity player",
        "game",
        "launcher",
        "application",
        "app",
        "bootstrap",
        "bootstrappackagedgame",
        "unreal engine",
        "ue4",
        "ue5",
        "ue4game",
        "godot engine",
        "godot",
        "nw.js",
        "electron",
        "lime",
        "hxcpp",
        "java",
        "python",
        "mono",
        "renpy",
        "love",
        "sdl",
        "directx",
        "setup",
        "installer",
    ];
    if GENERIC.contains(&lower.as_str())
        || lower.contains("windows")
        || lower.contains("microsoft")
        || lower.contains("operating system")
    {
        return None;
    }
    Some(name)
}

/// A game title from an executable's version resource: `ProductName`, else
/// `FileDescription`, when [`sensible_product_name`]. Bounded and never panics; `None` when
/// the file has no usable version information.
#[cfg(windows)]
pub fn exe_product_name(path: &Path) -> Option<String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(file.as_ptr()), None);
        if size == 0 || size > MAX_VERSION_INFO_BYTES {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(
            PCWSTR(file.as_ptr()),
            None,
            size,
            data.as_mut_ptr() as *mut c_void,
        )
        .ok()?;
        let start = data.as_ptr() as usize;
        let end = start + data.len();
        // Looks a value up; the result must lie entirely inside `data`.
        let query = |sub: &str| -> Option<(*const u8, usize)> {
            let key = wide(sub);
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let mut len: u32 = 0;
            let ok = VerQueryValueW(
                data.as_ptr() as *const c_void,
                PCWSTR(key.as_ptr()),
                &mut ptr,
                &mut len,
            );
            if !ok.as_bool() || ptr.is_null() {
                return None;
            }
            Some((ptr as *const u8, len as usize))
        };
        let in_bounds = |ptr: *const u8, bytes: usize| {
            let p = ptr as usize;
            p >= start && p.checked_add(bytes).is_some_and(|e| e <= end)
        };
        let mut languages: Vec<(u16, u16)> = Vec::new();
        if let Some((ptr, len)) = query(r"\VarFileInfo\Translation") {
            if in_bounds(ptr, len) {
                let bytes = std::slice::from_raw_parts(ptr, len);
                for pair in bytes.as_chunks::<4>().0.iter().take(16) {
                    languages.push((
                        u16::from_le_bytes([pair[0], pair[1]]),
                        u16::from_le_bytes([pair[2], pair[3]]),
                    ));
                }
            }
        }
        languages.extend([(0x0409, 0x04b0), (0x0409, 0x04e4), (0x0000, 0x04b0)]);
        for field in ["ProductName", "FileDescription"] {
            for (language, codepage) in &languages {
                let sub = format!(r"\StringFileInfo\{language:04x}{codepage:04x}\{field}");
                let Some((ptr, chars)) = query(&sub) else {
                    continue;
                };
                // `chars` counts UTF-16 units (including the terminator).
                let Some(bytes) = chars.checked_mul(2) else {
                    continue;
                };
                if chars == 0
                    || chars > 512
                    || !in_bounds(ptr, bytes)
                    || !(ptr as usize).is_multiple_of(2)
                {
                    continue;
                }
                let units = std::slice::from_raw_parts(ptr as *const u16, chars);
                if let Some(name) = sensible_product_name(&String::from_utf16_lossy(units)) {
                    return Some(name);
                }
            }
        }
        None
    }
}

#[cfg(not(windows))]
pub fn exe_product_name(_path: &Path) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// Long (non-8.3) paths

/// The long form of a path: 8.3 short names (`RUNNER~1`) expanded to the real names with
/// `GetLongPathNameW`, for the longest part of the path that exists (the rest is kept as
/// given, so a file that was deleted still compares equal to how it was stored). Links are
/// NOT resolved. Paths without a `~` can't contain a short name and are returned unchanged
/// without touching the disk. Never fails: on any error the input is returned.
#[cfg(windows)]
pub fn long_path(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;
    if !path.as_os_str().to_string_lossy().contains('~') {
        return path.to_path_buf();
    }
    let mut existing = path;
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
    let wide: Vec<u16> = existing
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buffer = vec![0u16; 32_768];
    let len = unsafe { GetLongPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) };
    if len == 0 || len as usize >= buffer.len() {
        return path.to_path_buf();
    }
    let mut long = PathBuf::from(OsString::from_wide(&buffer[..len as usize]));
    for name in rest.into_iter().rev() {
        long.push(name);
    }
    long
}

#[cfg(not(windows))]
pub fn long_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// The 8.3 short form of an existing path, when the volume has short names (tests only).
#[cfg(all(windows, test))]
pub fn short_path(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buffer = vec![0u16; 32_768];
    let len = unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) };
    if len == 0 || len as usize >= buffer.len() {
        return path.to_path_buf();
    }
    PathBuf::from(OsString::from_wide(&buffer[..len as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path containment moved to `tracker`; same cases, now on `normalize_path` + `is_within`.
    #[test]
    fn path_containment_is_case_insensitive_and_prefix_safe() {
        use crate::tracker::{is_within, normalize_path};
        let is_inside = |folder: &str, file: &str| {
            is_within(
                &normalize_path(folder).unwrap(),
                &normalize_path(file).unwrap(),
            )
        };
        assert!(is_inside(r"C:\Games\Foo", r"c:\games\foo\bin\foo.exe"));
        assert!(is_inside("C:/Games/Foo/", r"C:\Games\Foo\foo.exe"));
        assert!(is_inside(r"C:\Games\Foo", r"\\?\C:\Games\Foo\foo.exe"));
        assert!(!is_inside(r"C:\Games\Foo", r"C:\Games\FooBar\foo.exe"));
        assert!(!is_inside(r"C:\Games\Foo", r"C:\Games\Foo\..\Bar\x.exe"));
    }

    #[test]
    fn only_real_visible_main_windows_count_as_playing() {
        let game = WindowInfo {
            pid: 1,
            visible: true,
            width: 1920,
            height: 1080,
            ..Default::default()
        };
        assert!(is_play_window(&game));
        assert!(is_play_window(&WindowInfo {
            minimized: true,
            width: 0,
            height: 0,
            ..game
        }));
        assert!(is_play_window(&WindowInfo {
            owned: true,
            app_window: true,
            ..game
        }));
        for not_playing in [
            WindowInfo {
                visible: false,
                ..game
            },
            WindowInfo {
                cloaked: true,
                ..game
            },
            WindowInfo {
                tool_window: true,
                ..game
            },
            WindowInfo {
                tool_window: true,
                app_window: true,
                ..game
            },
            WindowInfo {
                owned: true,
                ..game
            },
            WindowInfo { width: 0, ..game },
            WindowInfo { height: 0, ..game },
            WindowInfo {
                width: -5,
                height: 10,
                ..game
            },
        ] {
            assert!(!is_play_window(&not_playing), "{not_playing:?}");
        }
    }

    #[test]
    fn encodes_like_javascript() {
        assert_eq!(
            encode_uri_component("a b/c?d&e~f(g)"),
            "a%20b%2Fc%3Fd%26e~f(g)"
        );
    }

    #[cfg(windows)]
    #[test]
    fn lists_the_current_process_and_round_trips_dpapi() {
        let me = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let processes = running_processes().unwrap();
        assert!(processes
            .iter()
            .any(|p| p.path.eq_ignore_ascii_case(&me) && p.pid == std::process::id()));
        // A test runner has no visible window; the call itself must work.
        let _ = processes_with_play_windows();
        let sealed = protect(b"secret").unwrap();
        assert_ne!(sealed, b"secret");
        assert_eq!(unprotect(&sealed).unwrap(), b"secret");
    }
    #[test]
    fn product_names_must_look_like_a_title() {
        assert_eq!(
            sensible_product_name("  Return of the ").as_deref(),
            Some("Return of the")
        );
        assert_eq!(sensible_product_name("Cairn").as_deref(), Some("Cairn"));
        for bad in [
            "",
            "   ",
            "1.0.0.0",
            "Unity Player",
            "UnityPlayer",
            "Microsoft® Windows® Operating System",
            "Electron",
            &"x".repeat(101),
        ] {
            assert_eq!(sensible_product_name(bad), None, "{bad}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn version_resources_are_read_safely() {
        // A file without a version resource, a missing file and a real Windows program.
        let dir = std::env::temp_dir().join(format!("gamelobby-test-ver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("Cairn.exe");
        std::fs::write(&fake, b"MZ not really").unwrap();
        assert_eq!(exe_product_name(&fake), None);
        assert_eq!(exe_product_name(&dir.join("missing.exe")), None);
        std::fs::remove_dir_all(&dir).unwrap();
        let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let notepad = std::path::Path::new(&windows)
            .join("System32")
            .join("notepad.exe");
        if notepad.is_file() {
            // ProductName is the generic OS name, so FileDescription ("Notepad") is used.
            let name = exe_product_name(&notepad);
            assert!(
                name.as_deref().is_none_or(|n| !n.contains("Windows")),
                "{name:?}"
            );
        }
    }
}
