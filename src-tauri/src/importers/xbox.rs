//! Xbox / Microsoft Store: installed game packages via the Windows PackageManager API
//! (replaces `Get-AppxPackage` through PowerShell). Package-name matching is best-effort.

use super::states;
use crate::types::{Game, GameRoute};
use anyhow::{bail, Result};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

static LIKELY_GAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)game|minecraft|forza|halo|gears|flight|seaofthieves|stateofdecay|starfield|fallout|elder|outerworld|ori|psychonauts|fable|killerinstinct|asphalt").unwrap()
});
static EXCLUDED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)gamingservices|gamingapp|gamebar|xboxidentity|xboxspeech|xbox.tcui").unwrap()
});
static APPLICATION_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<Application\b[^>]*\bId="([^"]+)""#).unwrap());
static CAMEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([a-z])([A-Z])").unwrap());
static SEPARATORS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[-_]+").unwrap());

pub struct AppPackage {
    pub name: String,
    pub family_name: String,
    pub install_location: String,
}

#[cfg(windows)]
fn list_packages() -> Result<Vec<AppPackage>> {
    use windows::core::HSTRING;
    use windows::Management::Deployment::PackageManager;
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let manager = PackageManager::new()?;
    // An empty security ID means the current user, which needs no elevation.
    let packages = manager.FindPackagesByUserSecurityId(&HSTRING::new())?;
    let mut out = Vec::new();
    for package in packages {
        if package.IsFramework().unwrap_or(true) {
            continue;
        }
        let Ok(id) = package.Id() else { continue };
        let (Ok(name), Ok(family)) = (id.Name(), id.FamilyName()) else {
            continue;
        };
        let Ok(location) = package.InstalledPath() else {
            continue;
        };
        if location.is_empty() {
            continue;
        }
        out.push(AppPackage {
            name: name.to_string(),
            family_name: family.to_string(),
            install_location: location.to_string(),
        });
    }
    Ok(out)
}

#[cfg(not(windows))]
fn list_packages() -> Result<Vec<AppPackage>> {
    Ok(Vec::new())
}

fn title(name: &str) -> String {
    let rest = name.split('.').skip(1).collect::<Vec<_>>().join(" ");
    let spaced = CAMEL.replace_all(&rest, "$1 $2");
    let clean = SEPARATORS.replace_all(&spaced, " ").trim().to_string();
    if clean.is_empty() {
        name.to_string()
    } else {
        clean
    }
}

pub fn scan() -> Result<Vec<Game>> {
    let mut games = Vec::new();
    for package in list_packages()? {
        if EXCLUDED.is_match(&package.name) || !LIKELY_GAME.is_match(&package.name) {
            continue;
        }
        let manifest = Path::new(&package.install_location).join("AppxManifest.xml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            bail!(
                "Could not verify Xbox package {}. Existing entries were left unchanged.",
                package.name
            );
        };
        let Some(app_id) = APPLICATION_ID.captures(&text).map(|c| c[1].to_string()) else {
            bail!(
                "Xbox package {} has no readable application entry.",
                package.name
            );
        };
        let aumid = format!("{}!{app_id}", package.family_name);
        games.push(Game {
            id: format!("xbox:{}:{app_id}", package.family_name),
            title: title(&package.name),
            store: "Xbox".into(),
            routes: vec![GameRoute {
                source_id: "xbox-appx".into(),
                store: "Xbox".into(),
                launch_target: format!("shell:AppsFolder\\{aumid}"),
                app_id: Some(aumid),
                install_path: Some(package.install_location),
                installed: Some(true),
                install_states: states("xbox-appx", true),
                ..Default::default()
            }],
            ..Default::default()
        });
    }
    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_titles_from_package_names() {
        assert_eq!(title("Microsoft.MinecraftUWP"), "Minecraft UWP");
        assert_eq!(title("Microsoft.Halo_MCC"), "Halo MCC");
        assert_eq!(title("Solo"), "Solo");
    }

    #[cfg(windows)]
    #[test]
    fn lists_installed_packages() {
        let packages = list_packages().unwrap();
        assert!(
            !packages.is_empty(),
            "every Windows install has app packages"
        );
    }
}
