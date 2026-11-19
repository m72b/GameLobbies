// The app's commands are listed in the app manifest, so Tauri's command ACL applies to them:
// a webview may only call a command when a capability grants its `allow-<command>` permission,
// and capabilities/default.json grants them to the window labelled `main` only. Sign-in windows
// (remote store pages) get no permission at all.
include!("command_list.rs");

fn main() {
    println!("cargo:rerun-if-changed=command_list.rs");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run the Tauri build script");
}
