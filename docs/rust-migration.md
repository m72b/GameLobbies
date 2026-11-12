# Rust migration (`Rusted` branch)

> **Status: done.** Game Lobby now runs on Tauri 2 with a Rust backend in `src-tauri/`; the React UI is unchanged apart from `src/renderer/lobby-api.ts`, which maps the old `window.lobby` API onto Tauri commands. The native addon described below was a first step and has been folded into `src-tauri/src/platform.rs`. The analysis is kept for context.

## What moved where

| Electron (TypeScript) | Tauri (Rust) |
| --- | --- |
| `src/main/index.ts` (windows, IPC, launching, scans) | `lib.rs`, `windows.rs`, `commands.rs`, `launch.rs`, `scan.rs` |
| `src/main/db/index.ts` (better-sqlite3) | `db.rs` (rusqlite, same schema) |
| `src/main/validation.ts` (zod) | `validation.rs` |
| `src/main/importers/*` (PowerShell, `reg.exe`) | `importers/*` (WinRT PackageManager, `winreg`) |
| `src/main/process-tracker.ts` (PowerShell CIM) | `tracker.rs` + `platform.rs` (Toolhelp32) |
| `src/main/secrets.ts` (`safeStorage`) | `launch.rs` + `platform.rs` (DPAPI) |
| `src/main/themes/loader.ts` | `theme.rs` |
| `src/preload/index.ts` | `src/renderer/lobby-api.ts` |

### Local library modules

Modules added for emulators and ROM folders:

| Module | Responsibility |
| --- | --- |
| `catalog.rs` | Supported systems, folder-name aliases, file formats, known emulators |
| `roots.rs` | Library folders and their validation rules |
| `emulators.rs` | Emulator discovery and profiles |
| `localscan.rs` | Folder walking and game detection |
| `local.rs` | Applying scan results and review resolution |
| `local_commands.rs` | IPC commands for local library features |
| `review.rs` | Review queue |

`launch.rs` now builds emulator argv without a shell.

---


## Current architecture

Game Lobby is an Electron 44 app (~2,000 lines of TypeScript) built with electron-vite.

| Layer | Files | Responsibility |
| --- | --- | --- |
| Renderer (React 19, Tailwind, react-virtual, fuse.js) | `src/renderer/*` | Library grid, search, detail panel, settings window |
| Preload | `src/preload/index.ts` | Narrow `window.lobby` API over `ipcRenderer.invoke` |
| Main: app shell | `src/main/index.ts` | Windows, navigation guards, IPC sender checks, `lobby-cover://` protocol, scan orchestration, game launching |
| Main: persistence | `src/main/db/index.ts` | better-sqlite3 schema/migrations, route merge and reconcile rules, settings, secrets table |
| Main: importers | `src/main/importers/*` | Steam (ACF/VDF + Web API), Epic (`.item` + Legendary CLI), GOG (Galaxy SQLite), EA/Battle.net/Ubisoft/Amazon (registry), itch.io (folders), Xbox (AppX) |
| Main: process tracker | `src/main/process-tracker.ts` | Polls running process paths every 15 s to accumulate playtime |
| Main: security/validation | `security.ts`, `validation.ts`, `secrets.ts`, `themes/loader.ts` | Launch-URI allowlist, zod schemas at the IPC boundary, DPAPI via `safeStorage`, size-limited remote theme fetch |

The main process's Windows integration works by **spawning external programs and parsing their text output**:

- `process-tracker.ts` runs `powershell.exe Get-CimInstance Win32_Process` every 15 s while a game is tracked.
- `windows-clients.ts` runs `reg.exe query … /s` on seven or more keys and regex-parses the output.
- `steam.ts` runs `reg query` for `SteamPath` and regex-parses VDF/ACF files.
- `xbox.ts` runs `powershell.exe Get-AppxPackage | ConvertTo-Json`.

PowerShell takes roughly 300–1,000 ms to start and costs noticeable CPU. Text parsing of `reg.exe` output depends on the system locale and output format. This part of the app is where Rust gives the clearest benefit.

## Recommendation: a native addon, not a full rewrite

Build a small Rust crate (`native/`, using **napi-rs**) that the Electron main process loads like `better-sqlite3`. Keep the TypeScript interfaces (`Importer`, `watchGameProcess`) and swap in the Rust implementations one at a time.

A full Tauri rewrite is possible but not recommended now. It would change the IPC and security model, which was recently hardened (`safeSender`, navigation guards, zod schemas). It would also mean porting the merge/reconcile rules in `db/index.ts`, which are easy to get wrong, and replacing `safeStorage`. The app is small enough that bundle size is the main gain, and the risk is not worth that yet.

## Components to rewrite in Rust

Ranked by value versus risk:

| # | Component | Now | Rust replacement | Value | Risk |
| --- | --- | --- | --- | --- | --- |
| 1 | **Process tracker** (`process-tracker.ts` → `processPaths()`) | PowerShell + CIM every 15 s | `windows` crate: `CreateToolhelp32Snapshot` + `QueryFullProcessImageNameW` (or the `sysinfo` crate). Returns `string[]` in a few ms. | High: removes a recurring PowerShell spawn | Low: one pure function; the polling logic stays in TS |
| 2 | **Registry reads** (`windows-clients.ts` → `queryRegistry()`, `steam.ts` → `SteamPath`) | `reg.exe` + regex over text output | `winreg` crate: enumerate subkeys and values directly and return `{ key, values }[]` | High: faster, independent of locale and output format, and returns typed `REG_EXPAND_SZ` | Low: same output shape |
| 3 | **Xbox / AppX enumeration** (`xbox.ts`) | `Get-AppxPackage` via PowerShell, 30 s timeout | `windows` crate: `Windows.Management.Deployment.PackageManager.FindPackagesForUser("")`, plus reading `AppxManifest.xml` with `quick-xml` | Medium-high: removes the slowest scan step | Medium: WinRT bindings |
| 4 | **Steam VDF/ACF parsing** (`steam.ts`) | Regexes such as `"name"\s+"([^"]+)"` (break on escaped quotes and nested blocks) | `keyvalues-parser` crate (a real KeyValues parser) | Medium: correctness | Low |
| 5 | **Executable discovery** (`executablePath()`) | Directory scan two levels deep + term ranking | Same algorithm in Rust using `std::fs` | Low-medium | Low; can also stay in TS |

### Keep in TypeScript

- **Renderer, preload, and IPC wiring.** They're tied to React and the Electron APIs, and Rust would not help.
- **`db/index.ts`.** better-sqlite3 is already native C. The code that matters is the merge/reconcile business logic, which should stay where it's tested. Move it only as part of a future Tauri migration, using `rusqlite`.
- **`validation.ts` (zod).** It guards the JS IPC boundary and has to run in JS.
- **`secrets.ts`.** Electron `safeStorage` already uses DPAPI.
- **`themes/loader.ts`, Steam Web API, Legendary CLI.** These are network-bound or depend on external programs, so Rust would not make them faster.
- **GOG importer.** Already native through better-sqlite3. Port it only if the DB layer moves later.

## Proposed layout

```
native/
  Cargo.toml          # napi, napi-derive, windows, winreg, keyvalues-parser, quick-xml
  build.rs
  src/lib.rs          # #[napi] exports
  src/process.rs      # running_process_paths() -> Vec<String>
  src/registry.rs     # query_registry(key: String, recursive: bool) -> Vec<RegistryEntry>
  src/appx.rs         # list_app_packages() -> Vec<AppPackage>
  src/vdf.rs          # parse_app_manifest(text) / parse_library_folders(text)
```

Exports, keeping the shapes the TS code already uses:

```ts
export function runningProcessPaths(): string[]
export function queryRegistry(key: string): { key: string; values: Record<string, string> }[]
export function listAppPackages(): { name: string; packageFamilyName: string; installLocation: string; appIds: string[] }[]
export function parseAppManifest(text: string): { appId: string; name: string; installDir?: string; stateFlags: number }
```

## Build and packaging changes

- Install the toolchain: `rustup` with target `x86_64-pc-windows-msvc` (MSVC Build Tools are already needed for better-sqlite3).
- Add `@napi-rs/cli`; add a `build:native` script and run it before `electron-vite build`.
- Add the `.node` file to `asarUnpack` in `package.json` next to `better-sqlite3`.
- CI (`windows-latest`): add `dtolnay/rust-toolchain@stable` and `Swatinem/rust-cache`, then run `cargo test` and `cargo clippy`.
- Fallback: if the addon fails to load, or on non-Windows, keep the current TS/PowerShell path. This keeps development working without Rust.

## Suggested order

1. ✅ Scaffold `native/` with `runningProcessPaths()`, wire it into `process-tracker.ts` behind the fallback, and add CI. Build locally with `npm run build:native` (needs `rustup`); set `LOBBY_DISABLE_NATIVE=1` to force the PowerShell path.
2. `queryRegistry()`, which replaces `reg.exe` in `windows-clients.ts` and `steam.ts`.
3. `parseAppManifest()` / `parseLibraryFolders()` with tests on real ACF fixtures.
4. `listAppPackages()` for Xbox.
5. Re-evaluate a Tauri migration only after these are stable.
