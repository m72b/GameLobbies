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
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = program.parent().filter(|d| d.is_dir()) {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    command
        .spawn()
        .map(drop)
        .map_err(|e| anyhow!("Could not start {}: {e}", program.display()))
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
}
