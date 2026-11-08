//! Native helpers for Game Lobby.
//!
//! Exposed to the Electron main process through Node-API. Node-API is ABI-stable,
//! so the same `.node` file loads in Electron without an electron-rebuild step.

use napi_derive::napi;

mod process;

/// Returns the full executable path of every running process this user can query.
/// Exported to JavaScript as `runningProcessPaths()`.
#[napi]
pub fn running_process_paths() -> napi::Result<Vec<String>> {
    process::running_process_paths().map_err(napi::Error::from_reason)
}
