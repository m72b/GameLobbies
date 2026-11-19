# Store account sign-in and first-time setup — design

Status: **Phase A implemented** (designed 2026-11-19 against v0.6.0; phase A built on v0.7.0, see [Phase A as built](#phase-a-as-built); phases B to D not started). Owner decisions: Steam official sign-in; GOG, Battle.net, Humble web sign-in; Xbox via an owner-created Azure app; Epic opt-in (unofficial). Ubisoft, itch.io, EA and others: installed files only.

## 0. Fix first

`src-tauri/build.rs` is a bare `tauri_build::build()` with no `AppManifest`, so Tauri 2 does not restrict the app's commands to the `main` window; only remote origins are kept out (no `remote` capability). Before any login window exists:

- Build with `tauri_build::try_build(Attributes::new().app_manifest(AppManifest::new().commands(&[...all commands...])))` and grant `allow-<cmd>` permissions only in `capabilities/default.json` (windows `["main"]`).
- Test: parse `capabilities/*.json`; fail if any `windows` entry is not `"main"` or any capability has `remote`. Test: every command in `generate_handler!` is in the manifest.
- `require_main(&window)?` in every new accounts/setup command.
- `lobby-cover` / `lobby-shot` protocol handlers return 403 unless the webview label is `main`.

## 1. Login windows (`src-tauri/src/accounts/signin.rs`)

- Renderer calls only `accounts_sign_in(store: AccountStore)` (enum: steam, gog, battlenet, humble, xbox, epic). URLs, hosts and labels are Rust constants.
- `WebviewWindowBuilder::new(app, "signin-<store>", WebviewUrl::External(start_url))`, parented to `main`, ~520×720, title "Sign in to <Store> – <current host>" (updated from `on_page_load`).
- `.incognito(true)` (WebView2 InPrivate) for every store: no profile on disk, no session fixation; users re-enter credentials on each sign-in. Fallback only if InPrivate is unusable: `.data_directory(<app_local_data_dir>\signin\<store>)`, deleted by exact path after the window is destroyed; failed deletes retried at startup (`accounts.pendingProfileDeletes`).
- No `initialization_script` of ours, devtools off in release, `.disable_drag_drop_handler()`. (As built: Tauri still injects its core scripts — `__TAURI_INTERNALS__`, the IPC bridge and, with `freezePrototype`, the prototype freeze — into every webview, the sign-in window included; every IPC call from it is refused by the ACL and the main-window check.)
- `.on_navigation(guard)`: https only, no userinfo/port, ASCII host equal to or a subdomain of the store's `LOGIN_HOSTS`, reject `xn--` labels. Third-party IdP buttons blocked ("Use your store email and password").
  - Steam `steamcommunity.com`; GOG `gog.com`, `login.gog.com`, `auth.gog.com`; Battle.net `battle.net`, `blizzard.com`; Humble `humblebundle.com`; Xbox `login.live.com`, `login.microsoftonline.com`, `account.live.com`; Epic `epicgames.com`.
- Popups denied (`on_new_window` → Deny, or `NewWindowRequested.Handled`), downloads denied, `LaunchingExternalUriScheme` cancelled.
- One sign-in at a time (`SignInFlight{store, state, nonce, verifier}` in `AppState.accounts`); ends on result, window close (Cancelled), 10-minute timeout, or `accounts_cancel_sign_in()`; Rust always closes the window; renderer receives only an `AccountStatus`.

Result capture:

| Store | Capture |
| --- | --- |
| Steam | OpenID `return_to` intercepted in `on_navigation` (never loads) |
| Xbox | navigation to `https://login.live.com/oauth20_desktop.srf?code=…&state=…` intercepted |
| Epic | navigation to `https://www.epicgames.com/id/api/redirect?…` intercepted; Rust GETs it once with the exported cookies and reads `authorizationCode` |
| GOG / Battle.net / Humble | `on_page_load(Finished)` on the logged-in URL (`www.gog.com/account`, `account.battle.net/overview`, `www.humblebundle.com/home/library`) → export cookies → store `probe()` must succeed |

Cookie export (`accounts/cookies.rs`, Windows): `ICoreWebView2_2::CookieManager().GetCookies(uri)` per `API_HOSTS` entry (dependency `webview2-com` pinned to wry's version), keep only allowlisted domains, serialise `{name, value, domain, path, secure, http_only, expires}`.

Steam OpenID (`accounts/steam_openid.rs`): `return_to = http://localhost/gamelobby/steam-return?n=<128-bit nonce>`, `realm = http://localhost/`, `checkid_setup`, identifier_select. Verify: `mode == id_res`; `op_endpoint == https://steamcommunity.com/openid/login`; `return_to` byte-equal and nonce matches; `claimed_id == identity` matching `^https://steamcommunity\.com/openid/id/(7656119\d{10})$`; `signed` covers op_endpoint, claimed_id, identity, return_to, response_nonce; `response_nonce` within ±5 min and unseen; then `check_authentication` returns `is_valid:true`. Store the SteamID64 in the existing `STEAM_OWNED_STEAM_ID` secret; the API key step is unchanged. Spike: confirm Steam accepts a localhost realm; fallback is a one-path loopback listener on `127.0.0.1:<random>`.

## 2. Session storage

- DPAPI-sealed secret `account-<store>-session-dpapi` holding a `Session` enum: `Tokens{access, refresh, expires_at, account_id}` (Xbox also caches `uhs`/`xsts` + expiry; Epic) or `Cookies(Vec<StoredCookie>)` (GOG, Battle.net, Humble). Diagnostics show yes/no only.
- Cookie stores are fetched **from Rust** with the exported cookie jar (one tested place for host allowlist, redirects, caps, timeouts; no JS in the store origin; no persistent profile). Send the WebView2 user agent captured at sign-in. Plan B if bot protection blocks reqwest: a hidden InPrivate webview loaded with the sealed cookies, `ExecuteScript` result returned natively (no IPC).
- Cookie refresh via reqwest `cookies` with a custom `CookieStore` that accepts `Set-Cookie` only for allowlisted domains; re-seal after each sync.
- `accounts_sign_out(store)` in one transaction: delete `account-<store>-*` secrets, the `library_connections` key `account:<store>`, `store_playtime` rows for that source, and untouched owned-not-installed games created by `<store>-account` (same rule as `steam_owned_remove`); mode back to installed-only.

## 3. Fetch layer (`src-tauri/src/accounts/`)

`mod.rs, signin.rs, cookies.rs, http.rs, session.rs, apply.rs, steam_openid.rs, gog.rs, battlenet.rs, humble.rs, xbox.rs, epic.rs, launcher_clients.rs`

```rust
pub struct OwnedGame { pub store_id: String, pub title: String, pub launch: Option<String>,
  pub playtime_seconds: Option<u64>, pub last_played_ms: Option<i64> }
pub struct FetchOutcome { pub games: Vec<OwnedGame>, pub complete: bool, pub session: Option<Session> }
pub trait StoreApi { const API_HOSTS: &'static [&'static str];
  async fn probe(&self, h: &StoreHttp, s: &Session) -> Result<(), FetchError>;
  async fn fetch_owned(&self, h: &StoreHttp, s: &Session) -> Result<FetchOutcome, FetchError>; }
```

- Move `steam_owned`'s `FetchError`, `read_capped`, `clean_name`, `network_error` into `accounts/http.rs` (behaviour-preserving).
- `StoreHttp`: host check before every request; redirects ≤3, https, on-list only; 20 s request / 10 s connect; 8 MiB per response, 200 pages, 50 000 games; 300 ms between requests; one sync per store; honour `Retry-After`, backoff up to 1 h; refresh every 6 h for unofficial stores (Steam 30 min), retry after 30 min on error.
- `API_HOSTS`: GOG `www.gog.com`, `menu.gog.com`, `embed.gog.com`; Battle.net `account.battle.net`; Humble `www.humblebundle.com`; Xbox `login.live.com`, `user.auth.xboxlive.com`, `xsts.auth.xboxlive.com`, `titlehub.xboxlive.com`, `userstats.xboxlive.com`; Epic `account-public-service-prod03.ol.epicgames.com`, `library-service.live.use1a.on.epicgames.com`, `catalog-public-service-prod06.ol.epicgames.com`, `www.epicgames.com` (Epic also restricted to exact method+path pairs).
- Errors are fixed strings per variant, never bodies or URLs. Extend `redact.rs`: cookie headers, `XBL3.0 x=…`, JWTs, query keys `code|gamekeys|openid.*|authorization_code`, `RpsTicket`; fixture tests for each.
- GOG: `menu.gog.com/v1/account/basic` (isLoggedIn, username), `embed.gog.com/user/data/games` (ownership), `www.gog.com/u/{username}/games/stats?sort=recent_playtime&page=N` (titles, playtime); username must match `^[A-Za-z0-9_.-]{1,64}$`.
- Battle.net: `/api/games-and-subs`, `/api/classic-games`.
- Humble: gamekeys from `#user-home-json-data` (bounded search, parse only that JSON), then `/api/v1/orders?all_tpkds=true&gamekeys=…` in batches of 40; keep subproducts with Windows downloads or Steam keys.
- Apply (`accounts/apply.rs`, generalising `steam_owned::apply`): per-store state in `library_connections` `account:<store>`; ids `gog:<productId>`, `epic:<appName>` (same as importers, so they dedupe), `battlenet:<code>`, `humble:<machine_name>`; never touch title, cover, hidden, favourite, notes or custom routes of existing games (only set `owned:true` on the matching store route); new entries `owned:true, installed:false`, `source_id "<store>-account"`, skipped if a same-store game with an equal normalised title exists; removals only for `<store>-account` routes, behind the mass-removal guard and only when `complete`.
- Launch links built by Rust from validated ids only: GOG `goggalaxy://openGameView/<digits>`; Epic `importers::epic::launch_uri(app_name, "launch")` with `^[A-Za-z0-9._-]{1,128}$`; Battle.net `battlenet://<code>` from a fixed slug→code map (unknown → no route); Humble no route ("Owned on Humble"); Xbox no owned entries (titlehub is play history). Every link passes `validate_launch_uri` (and `renderer_link_ok` where it round-trips), tested per shape.
- Playtime: write through the Statistics work's `store_playtime` table (agree the function signature), only for games that resolve to a library id.

## 4. First-time setup

- Setting `setup = { version: 1, completedAt?, skippedAt?, choices: { [store]: "signin" | "installed" | "skip" } }`.
- Shows when `setup` is absent and the library has no games and no roots. Existing users get a one-time dismissible "Add store accounts" banner.
- New Settings section **Accounts** (`accounts` in `windows::SETTINGS_SECTIONS` and `settings-view.tsx`): per-store status, sign in, sign out, sync now, mode, and "Run setup again".
- Full-window view in `main` (`src/renderer/setup-view.tsx`): Welcome → pick stores → per store Sign in / Installed files only / Skip (Epic shows the warning; Steam's sign-in leads into the existing API key form) → library folders (reuse `LibraryFoldersSection`) → API keys (reuse `ApiKeysSection`) → Done (start scan and syncs). Every step skippable; choices saved through `setup_save(patch)`, validated strictly in Rust.
- Commands (manifest-listed, `require_main`): `accounts_status`, `accounts_sign_in`, `accounts_cancel_sign_in`, `accounts_sign_out`, `accounts_set_mode`, `accounts_sync`, `accounts_enable_unofficial(store)` (native confirmation with the warning), `setup_get`, `setup_save`. Event `accounts:changed` to `main` only.

## 5. Epic (unofficial, opt-in)

- Client id/secret and user agent (`UELauncher/<ver> Windows/10.0`) only in `accounts/launcher_clients.rs`, with a comment naming their origin (the same public values Legendary and Heroic use). Using them impersonates the official launcher, which Epic's terms forbid: risk of account flags or a repo takedown request.
- Flow: login page → code → `oauth/token` (`grant_type=authorization_code`, `token_type=eg1`) → refresh; library `library/api/public/items` (cursor pages), playtime `library/api/public/playtime/account/{id}/all`, titles from catalog `bulk/items`.
- Kill switch `accounts.unofficial = { enabled: false, stores: { epic: false } }`; when off, Rust refuses sign-in and sync and setup doesn't offer it.
- Auto-disable after 3 consecutive bad/rejected responses: "Epic sign-in stopped working with this version. Your installed Epic games still work." No retry until a new version or a manual re-sign-in.
- Warning: "Unofficial sign-in. Game Lobby signs in to Epic as if it were the Epic Games Launcher. Epic does not allow this. Epic may block it or act on your account. It can stop working at any time. Installed Epic games work without it. Continue?"

## 6. Xbox

Owner creates (portal.azure.com → Microsoft Entra ID → App registrations → New registration):

1. Name "Game Lobby"; supported account types **Personal Microsoft accounts only**.
2. Redirect URI: **Public client/native (mobile & desktop)**, `https://login.live.com/oauth20_desktop.srf`.
3. Authentication → **Allow public client flows: Yes**; no client secret, no API permissions.
4. Copy the Application (client) ID.

The id is public: constant `XBOX_CLIENT_ID` in `accounts/xbox.rs`, overridable at build time with `option_env!("GAMELOBBY_XBOX_CLIENT_ID")`; empty → "not available in this build". Flow: authorize (`Xboxlive.signin Xboxlive.offline_access`, `state`, PKCE S256) → `oauth20_token.srf` → `user.auth.xboxlive.com/user/authenticate` (`RpsTicket "d=<token>"`) → XSTS (`RelyingParty http://xboxlive.com`, gives `uhs`, `xid`) → titlehub title history (last played, PFN) → `userstats.xboxlive.com/batch` `MinutesPlayed`. Rows match installed `xbox:<PFN>:…` games and feed only playtime and last played. No approval is known to be needed (unverified: spike in phase C); the consent screen says "unverified" unless the owner completes publisher verification.

## 7. Threat model

| Risk | Control |
| --- | --- |
| Login page calls app commands | remote origin, no `remote` capability; AppManifest ACL for `main` only; `require_main`; capability test |
| Page reads local data through custom protocols | protocol handlers reject labels other than `main` |
| Page navigates or launches elsewhere | host allowlist, popups/downloads denied, external schemes cancelled, devtools off |
| Look-alike / IDN domains | exact or suffix ASCII match, `xn--` rejected, https without port/userinfo, host in title |
| Cookie or token theft at rest | DPAPI per store; InPrivate (no profile on disk); never in backups, diagnostics or the renderer |
| Token leakage in logs/errors | fixed error strings, extended redaction, tests that log a fake session |
| Store response injects links/scripts/paths | links built from regex-validated ids; `validate_launch_uri` / `renderer_link_ok`; titles through `clean_name`; covers from the existing art pipeline |
| Off-host redirect leaks cookies | custom redirect policy, per-request host check, domain-filtered jar |
| Renderer abuses sign-in | enum-only API, one flight, Rust-built URLs, native confirmation for Epic, sign-out scoped to one store |
| OpenID CSRF/replay | nonce in `return_to`, `response_nonce` freshness and reuse set, `check_authentication`, `claimed_id`/`op_endpoint` checks |
| OAuth code injection/CSRF | `state` + PKCE; code accepted only from the active flight's intercepted redirect |
| Session fixation | fresh InPrivate session per sign-in; cookies exported only after a successful probe |
| Mass deletion from a bad response | mass-removal guard, `complete` flag, undo |
| Account lock from impersonation (Epic) | opt-in, native warning, kill switch, auto-disable |

Embedded login pages go against RFC 8252's system-browser advice (users type passwords into Game Lobby's window); accepted by the owner, mitigated above, and stated in the user guide.

## 8. Phases

- **A** — framework, setup, Steam: section 0, `accounts/{mod,signin,http,session,apply,steam_openid}`, `FetchError` refactor, redaction, setup view, Accounts section, `store_playtime` hook. Tests: navigation guard (`gog.com.evil.com`, `xn--`, userinfo, port, http, `data:`), OpenID fixtures + local `check_authentication` (valid, tampered `signed`, wrong `op_endpoint`, `claimed_id` ≠ `identity`, nonce reuse, stale nonce), ACL tests, redaction tests, unchanged `steam_owned` suite, setup-state validation. Independent senior-engineer review.
- **B** — GOG, Battle.net, Humble: cookie export and jar, probes, parsers, launch shapes, apply. Tests: hostile/oversized JSON fixtures, redirect policy against a local server, dedupe/no-overwrite, mass-removal guard, sign-out cleanup. Owner test run; review.
- **C** — Xbox (after the owner sends the client id): PKCE, XBL/XSTS, titlehub, userstats, playtime matching. Tests: state/PKCE mismatch, refresh, PFN matching. Review.
- **D** — Epic: kill switch, native opt-in, path allowlist, auto-disable. Tests: refused when disabled, auto-disable after 3 failures, path allowlist. Independent review.

The owner runs every installer build; the app is never run from the development sandbox.

## Phase A as built

Differences from the sections above, and choices the design left open. Everything not listed here was built as designed.

- **Steam in the browser (owner decision after the first test, 2026-11-20).** Two findings: Steam's edge network (Akamai) answers 403 Access Denied to an OpenID request with the port-less realm `http://localhost/` (realms with a port, `127.0.0.1:<port>` or `localhost:<port>`, are accepted; checked with curl), and in the embedded InPrivate WebView2 window Steam's login page loaded but Steam Guard's phone approval never completed (likely Steam's finalize/transfer step across its domains; nothing was blocked by our guard). So Steam signs in in the user's **default web browser** (RFC 8252 style):
  - `accounts_sign_in(steam)` binds a listener on `127.0.0.1:0`, builds the OpenID start URL in Rust (realm `http://127.0.0.1:<port>/`, `return_to` with a fresh nonce), checks it is `https://steamcommunity.com/openid/login` and opens exactly that URL with `platform::shell_open` (ShellExecute). The renderer never sees it and `windows::EXTERNAL_HOSTS` is unchanged. The URL is never logged.
  - The listener (`steam_openid::serve_return`) takes one connection at a time from loopback peers, with an 8 KiB head cap and 10-second read and write timeouts. Only `GET /gamelobby/steam-return?…` reaches the flight; any other method or path gets 404 and changes nothing. A return without exactly one `n` equal to the nonce, or while one is being checked, gets a fixed 400 page and the listener keeps waiting. The real answer goes through the full verification (checks, replay set, `check_authentication`); the listener answers with a fixed page (`Content-Security-Policy: default-src 'none'`, `no-store`, `nosniff`, `no-referrer`, nothing from the request echoed), stops listening, the flight ends and the main window comes to the front.
  - Cancel and the 10-minute timeout (skipped while the answer is checked) end the flight, which aborts the listener and closes the port. The generation / scan-lock protection, the account-switch cleanup and the replay set are unchanged.
  - The embedded sign-in window (section 1, and the review fixes below) stays for later stores; Steam's return interception in its navigation guard was removed. `Flight.expected` is now optional (window flights have none).
  - Trade-off: the one-time return URL (with `openid.sig`) stays in the browser history; it can't be replayed (nonce and replay set).

- **Section 0.** The command list lives in `src-tauri/command_list.rs`, included by both `build.rs` (`AppManifest::commands`) and the `lib.rs` tests; tests check it against `generate_handler!` and `capabilities/default.json` (one `allow-<command>` each, window `main` only, no `remote`, no `webviews`). `require_main(&window)?` is in every new command, and **every** existing command is covered too, without touching each one: the invoke handler is wrapped (`main_window_only`) and rejects any webview whose label is not `main`. `tauri-build` writes `src-tauri/permissions/autogenerated/` on each build; it is git-ignored.
- **Review fixes (after the independent review).**
  1. tauri-runtime-wry lets a navigation through unchecked when `url` cannot parse its URI (`unwrap_or(true)`), skipping our guard and the title update. `harden_webview2` adds a second WebView2 `NavigationStarting` handler (after wry's; it only ever cancels) that cancels anything `native_nav_ok` refuses: unparseable URIs, and everything but `about:blank` (while allowed) and allow-listed https pages. Steam's return address stays cancelled (the guard still catches it).
  2. `about:blank` is allowed only until the store page is requested (`Flight.loaded`, set just before `navigate`), in both handlers.
  3. A caught return is taken only when it carries exactly one `n` equal to the sign-in's nonce (`Expected::carries_nonce`), for the guard and the loopback listener alike; others stay cancelled and are ignored, so a stray or forged navigation can't use up the sign-in.
  4. Tauri's core scripts are injected into the sign-in window (see section 1); `freezePrototype` stays on for the app. The owner checks that Steam's page still works with it.
  5. Permission requests are denied, the default context menu is off, and browser accelerator keys are off (`SetAreBrowserAcceleratorKeysEnabled(false)`; per the WebView2 documentation this leaves editing and movement keys such as Ctrl+C, Ctrl+V, Ctrl+A and Ctrl+Z working). A runtime that can't do these refuses the sign-in.
  6. The timeout leaves a sign-in alone while its answer is being checked. Saving a verified sign-in runs under the scan lock and only if no sign-out happened since the sign-in started (`AccountsState::steam_generation`, bumped by `steam_owned::remove_all` under the scan lock before the credentials are cleared).
  7. **Installed files only** asks first, like Sign out. It is not one SQL transaction (`steam_owned::undo` runs its own): the pause is written first, so a failed removal can't be undone by the import, and choosing it again finishes.
  8. Signing in to a different Steam account than the saved one first removes the old account's not-installed games (`steam_owned::undo`; games with user data stay), under the scan lock.
  9. `start` reads the running flight and releases the lock before any window call.
  10. A test fails if `ipc::Channel` appears in `src-tauri/src` (channel calls are exempt from the ACL).
- **Section 1.** Tauri 2.12 / wry 0.57 support `.incognito(true)` (`SetIsInPrivateModeEnabled`), so the `data_directory` fallback was not built. wry silently falls back to a normal (non-private) controller on a WebView2 runtime without `ICoreWebView2Environment10`, so the window is created on `about:blank`, then `with_webview` checks `ICoreWebView2Profile::IsInPrivateModeEnabled` and installs the `LaunchingExternalUriScheme` cancel handler (and turns off password autosave and autofill); only then is the store page loaded. On a runtime too old for either, the sign-in refuses with a message to update WebView2 (fail closed). `webview2-com = "0.39"` and `windows-core = "0.62"` match wry 0.57. Popups: `on_new_window → Deny`; downloads: `on_download → false`. `about:blank` is the only non-https page the guard allows, and only before the store page is requested. Blocked navigations are logged as scheme and host only. `SignInFlight` holds the Steam nonce and `return_to` (`state` / `verifier` come with OAuth in phases C and D).
- **Steam OpenID.** As designed, plus: duplicate query keys and oversized answers are refused; `openid.mode=cancel` is a cancelled sign-in; `signed` must also list only fields that are present, and `sig` / `assoc_handle` must be present; the replay set takes the nonce before `check_authentication`, so two copies of one answer can't both pass; `check_authentication` must answer HTTP 200 with exactly one `is_valid:true` line (64 KiB cap, no redirects, https to steamcommunity.com only). The return address is `http://127.0.0.1:<random port>/gamelobby/steam-return?n=<nonce>` with realm `http://127.0.0.1:<port>/` (see **Steam in the browser** below); the `Intercept` mode and `RETURN_MODE` were removed. The verified SteamID64 goes into `STEAM_OWNED_STEAM_ID`; the API key step is the existing form.
- **Section 2.** Steam needs no session: nothing else is stored. `session.rs`, `cookies.rs` and the sealed `account-<store>-session-dpapi` secrets come with phase B. A store's mode is the `library_connections` row `account:<store>` (`{"mode":"installed"}`; absent = the default). For Steam, **Installed files only** pauses the owned-games import (also for forced syncs) and removes what it added (`steam_owned::undo`); **Account** resumes it and syncs; saving Steam credentials or signing in resumes it too. **Sign out** for Steam is the existing **Remove** (SteamID, API key, not-installed games without user data, `store_playtime` rows, import state) plus the mode row.
- **Section 3.** `FetchError`, `read_capped`, `clean_name` and `network_error` moved to `accounts/http.rs`; Steam keeps its own wording for a rejected key and private profiles, so every message is unchanged. `StoreHttp` and the generalised `apply.rs` were not needed for Steam and come with phase B; Steam keeps using `steam_owned::apply` and its existing `store_playtime` writes. Redaction gained `openid.*`, `code`, `authorization_code` and `gamekeys` query values, `Cookie` / `Set-Cookie` headers and JWTs; `XBL3.0 x=…` and `RpsTicket` come with phase C.
- **Section 4.** `setup` is its own row of the `settings` table, not a field of `Settings`, so it is not in backups. Whether the library is fresh (no setup row, no games, no library roots) is decided once at start-up, before any scan can add games. `setup_get` returns `{state, show, banner, stores}`; the store list (with whether sign-in is offered and a note) comes from Rust. Setup stores: Steam, Epic, GOG, Xbox, Battle.net, Humble, EA, Ubisoft, itch.io. A `signin` choice is refused for stores without sign-in in this version; `completed` / `skipped` may only be `true` (Rust sets the time). Choices are recorded only: **Installed files only** and **Skip** do not turn stores off under Libraries. The banner's dismiss saves `skipped`. `accounts_enable_unofficial` comes with phase D. Accounts statuses carry flags and fixed-text messages only; a new notice is also shown as a toast.
- **Tests added.** ACL (handler = list = capability, `main` only, no `remote`, build script uses the list, protocol guards before disk access); navigation guard (`gog.com.evil.com`, punycode and IDN look-alikes, user info, port, http, IP hosts, `data:`, `javascript:`, `file:`, app schemes); OpenID fixtures and a local `check_authentication` server (valid, `is_valid:false`, duplicate `is_valid`, HTML body, HTTP 500, tampered `signed`, missing signed field, missing `sig`, wrong `op_endpoint`, `claimed_id` ≠ `identity`, non-Steam claimed id, wrong `return_to`, other sign-in's nonce, wrong path, duplicate field, `cancel`, nonce reuse, stale and future nonces, https-only production endpoint); loopback listener (only the return path); redaction fixtures; setup patch and stored-row validation, fresh-install and banner logic; account enum and mode storage; every account and setup command calls `require_main`. The `steam_owned` suite is unchanged.
- **Not verified here** (the app is never run from the development sandbox): Steam accepting the `http://localhost/` realm, the sign-in window end to end, InPrivate on the owner's WebView2 runtime, and the setup flow. The owner tests these with an installer build.

