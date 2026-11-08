//! Running-process enumeration.
//!
//! Replaces `powershell.exe Get-CimInstance Win32_Process`, which the process
//! tracker used to start every 15 seconds while a game was being tracked.

#[cfg(windows)]
pub fn running_process_paths() -> Result<Vec<String>, String> {
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

    // Collect process IDs from a Toolhelp snapshot.
    let mut pids = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "CreateToolhelp32Snapshot failed: {}",
                std::io::Error::last_os_error()
            ));
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

    // Resolve each PID to its full image path. Processes this user cannot open
    // (protected or system processes) are skipped, as CIM's ExecutablePath is null for them.
    let mut paths = Vec::with_capacity(pids.len());
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
                paths.push(String::from_utf16_lossy(&buffer[..length as usize]));
            }
        }
    }
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}

#[cfg(not(windows))]
pub fn running_process_paths() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn includes_the_current_process() {
        let me = std::env::current_exe().expect("current exe");
        let me = me.to_string_lossy();
        let paths = super::running_process_paths().expect("process list");
        assert!(
            paths.iter().any(|path| path.eq_ignore_ascii_case(&me)),
            "{me} was not in {} listed processes",
            paths.len()
        );
    }
}
