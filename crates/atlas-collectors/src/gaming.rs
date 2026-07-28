//! Bounded, user-mode game discovery for Gaming Intelligence.
//!
//! Discovery deliberately follows launcher manifests and a short list of known
//! install roots. It never crawls an arbitrary drive. Executable paths are
//! accepted only when the expected file exists, and duplicate launcher entries
//! collapse by executable identity.

#![cfg(windows)]

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::ffi::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
use crate::reg::RegKey;
use crate::{read_version_info, verify_signature_info};

pub const GAMING_ADAPTER_VERSION: &str = "1.2.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamePlatform {
    Steam,
    Riot,
    Epic,
    Ea,
    BattleNet,
    Xbox,
    Standalone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameSupportLevel {
    Universal,
    PilotReadOnly,
    PilotVersionGated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredGame {
    pub id: String,
    pub catalog_id: String,
    pub display_name: String,
    pub platform: GamePlatform,
    pub executable_path: PathBuf,
    pub executable_identity: String,
    pub install_path: PathBuf,
    pub version: String,
    pub support_level: GameSupportLevel,
    pub last_seen_ms: i64,
    pub adapter_version: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryCapability {
    pub id: &'static str,
    pub available: bool,
    pub limited: bool,
    pub explanation: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameDiscoveryReport {
    pub games: Vec<DiscoveredGame>,
    pub capabilities: Vec<DiscoveryCapability>,
    pub limitations: Vec<String>,
    pub scanned_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrimaryDisplayReading {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub available: bool,
    pub limitation: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EpicManifest {
    display_name: Option<String>,
    install_location: Option<String>,
    launch_executable: Option<String>,
    app_version_string: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct Pilot {
    catalog_id: &'static str,
    display_name: &'static str,
    exe_names: &'static [&'static str],
}

const PILOTS: &[Pilot] = &[
    Pilot {
        catalog_id: "cs2",
        display_name: "Counter-Strike 2",
        exe_names: &["cs2.exe"],
    },
    Pilot {
        catalog_id: "valorant",
        display_name: "VALORANT",
        exe_names: &["valorant-win64-shipping.exe"],
    },
    Pilot {
        catalog_id: "fortnite",
        display_name: "Fortnite",
        exe_names: &["fortniteclient-win64-shipping.exe"],
    },
    Pilot {
        catalog_id: "apex-legends",
        display_name: "Apex Legends",
        exe_names: &["r5apex.exe", "r5apex_dx12.exe"],
    },
    Pilot {
        catalog_id: "overwatch",
        display_name: "Overwatch",
        exe_names: &["overwatch.exe"],
    },
    Pilot {
        catalog_id: "rocket-league",
        display_name: "Rocket League",
        exe_names: &["rocketleague.exe"],
    },
];

/// Discover the pilot catalog plus any recognized launcher install from bounded
/// manifests. Missing or inaccessible launchers become capability limitations,
/// not errors and never guessed installs.
pub fn discover_games() -> GameDiscoveryReport {
    let scanned_ms = now_ms();
    let mut out = Vec::new();
    let mut capabilities = Vec::new();
    let mut limitations = Vec::new();

    let steam_roots = steam_library_roots();
    let steam_found = discover_steam(&steam_roots, scanned_ms, &mut out);
    capabilities.push(DiscoveryCapability {
        id: "launcher.steam",
        available: !steam_roots.is_empty(),
        limited: false,
        explanation: if steam_roots.is_empty() {
            "Steam was not found in its registered or default locations.".into()
        } else {
            format!(
                "Checked {} Steam library location(s) from launcher metadata.",
                steam_roots.len()
            )
        },
    });

    let epic_manifest_dir = program_data().join("Epic/EpicGamesLauncher/Data/Manifests");
    let epic_accessible = epic_manifest_dir.is_dir();
    let epic_found = discover_epic(&epic_manifest_dir, scanned_ms, &mut out);
    capabilities.push(DiscoveryCapability {
        id: "launcher.epic",
        available: epic_accessible,
        limited: false,
        explanation: if epic_accessible {
            "Checked Epic launcher manifests without scanning game drives.".into()
        } else {
            "Epic launcher manifests were not present or accessible.".into()
        },
    });

    let riot_paths = riot_candidate_paths();
    let riot_found = discover_known_roots(GamePlatform::Riot, scanned_ms, &mut out, &riot_paths);
    capabilities.push(DiscoveryCapability {
        id: "launcher.riot",
        available: riot_found > 0,
        limited: riot_found == 0,
        explanation: if riot_found > 0 {
            "Found a verified VALORANT executable in Riot's known install layout.".into()
        } else {
            "Riot's known install layout did not expose a VALORANT executable.".into()
        },
    });

    let ea_paths = apex_candidate_paths();
    let ea_found = discover_known_roots(GamePlatform::Ea, scanned_ms, &mut out, &ea_paths);
    capabilities.push(DiscoveryCapability {
        id: "launcher.ea",
        available: ea_found > 0,
        limited: ea_found == 0,
        explanation: if ea_found > 0 {
            "Found Apex Legends in an EA launcher install location.".into()
        } else {
            "EA launcher manifests are not exposed through a stable local format; checked only known Apex locations.".into()
        },
    });

    let battlenet_found = discover_known_roots(
        GamePlatform::BattleNet,
        scanned_ms,
        &mut out,
        &[
            program_files_x86().join("Overwatch/_retail_/Overwatch.exe"),
            program_files().join("Overwatch/_retail_/Overwatch.exe"),
        ],
    );
    capabilities.push(DiscoveryCapability {
        id: "launcher.battlenet",
        available: battlenet_found > 0,
        limited: battlenet_found == 0,
        explanation: if battlenet_found > 0 {
            "Found Overwatch in Battle.net's known install layout.".into()
        } else {
            "Battle.net did not expose a supported Overwatch install in its known layout.".into()
        },
    });

    capabilities.push(DiscoveryCapability {
        id: "launcher.xbox",
        available: false,
        limited: true,
        explanation: "Xbox/MSIX package discovery is not enabled in this build. Atlas will not inspect protected WindowsApps folders directly.".into(),
    });
    limitations.push("Xbox/MSIX games require the package-catalog collector planned for a later validated build.".into());

    deduplicate(&mut out);
    out.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then(a.platform_name().cmp(b.platform_name()))
    });

    if steam_found + epic_found + riot_found + ea_found + battlenet_found == 0 {
        limitations.push("No pilot-game executable was found through supported launcher metadata or known install layouts.".into());
    }

    GameDiscoveryReport {
        games: out,
        capabilities,
        limitations,
        scanned_ms,
    }
}

/// Reads the active primary desktop mode without changing it. This is a
/// capability-labelled fallback until the full display-topology collector is
/// validated; HDR and VRR are intentionally not inferred from this API.
pub fn primary_display() -> PrimaryDisplayReading {
    const HORZRES: i32 = 8;
    const VERTRES: i32 = 10;
    const VREFRESH: i32 = 116;
    #[link(name = "user32")]
    extern "system" {
        fn GetDC(window: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn ReleaseDC(window: *mut std::ffi::c_void, dc: *mut std::ffi::c_void) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn GetDeviceCaps(dc: *mut std::ffi::c_void, index: i32) -> i32;
    }

    let dc = unsafe { GetDC(std::ptr::null_mut()) };
    if dc.is_null() {
        return PrimaryDisplayReading {
            limitation: "Windows did not return a primary display context.".into(),
            ..PrimaryDisplayReading::default()
        };
    }
    let width = unsafe { GetDeviceCaps(dc, HORZRES) }.max(0) as u32;
    let height = unsafe { GetDeviceCaps(dc, VERTRES) }.max(0) as u32;
    let refresh_hz = unsafe { GetDeviceCaps(dc, VREFRESH) }.max(0) as u32;
    unsafe { ReleaseDC(std::ptr::null_mut(), dc) };
    let available = width > 0 && height > 0 && refresh_hz > 0;
    PrimaryDisplayReading {
        width,
        height,
        refresh_hz,
        available,
        limitation: "Only the active primary desktop mode is available in this build; HDR, VRR capability, and multi-display routing are not inferred.".into(),
    }
}

impl DiscoveredGame {
    pub fn platform_name(&self) -> &'static str {
        match self.platform {
            GamePlatform::Steam => "Steam",
            GamePlatform::Riot => "Riot",
            GamePlatform::Epic => "Epic",
            GamePlatform::Ea => "EA",
            GamePlatform::BattleNet => "Battle.net",
            GamePlatform::Xbox => "Xbox",
            GamePlatform::Standalone => "Standalone",
        }
    }
}

fn discover_steam(roots: &[PathBuf], seen_ms: i64, out: &mut Vec<DiscoveredGame>) -> usize {
    let before = out.len();
    for root in roots {
        let steamapps = root.join("steamapps");
        for (appid, pilot, relative_exes) in [
            ("730", PILOTS[0], &["game/bin/win64/cs2.exe"] as &[&str]),
            (
                "1172470",
                PILOTS[3],
                &["r5apex_dx12.exe", "r5apex.exe"] as &[&str],
            ),
            ("2357570", PILOTS[4], &["_retail_/Overwatch.exe"] as &[&str]),
            (
                "252950",
                PILOTS[5],
                &["Binaries/Win64/RocketLeague.exe"] as &[&str],
            ),
        ] {
            let manifest_path = steamapps.join(format!("appmanifest_{appid}.acf"));
            let Ok(manifest) = fs::read_to_string(&manifest_path) else {
                continue;
            };
            let fields = quoted_pairs(&manifest);
            let Some(install_dir) = fields.get("installdir") else {
                continue;
            };
            let base = steamapps.join("common").join(install_dir);
            let version = fields.get("buildid").cloned().unwrap_or_default();
            for relative in relative_exes {
                let exe = base.join(relative);
                if exe.is_file() {
                    push_game(
                        out,
                        pilot,
                        GamePlatform::Steam,
                        exe,
                        version.clone(),
                        seen_ms,
                        Vec::new(),
                    );
                    break;
                }
            }
        }
    }
    out.len() - before
}

fn discover_epic(manifest_dir: &Path, seen_ms: i64, out: &mut Vec<DiscoveredGame>) -> usize {
    let before = out.len();
    let Ok(entries) = fs::read_dir(manifest_dir) else {
        return 0;
    };
    for entry in entries.flatten().take(2048) {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("item"))
        {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(manifest) = serde_json::from_slice::<EpicManifest>(&bytes) else {
            continue;
        };
        let display = manifest.display_name.as_deref().unwrap_or_default();
        let Some(pilot) = pilot_from_text(display) else {
            continue;
        };
        let Some(location) = manifest.install_location else {
            continue;
        };
        let launch = manifest.launch_executable.unwrap_or_default();
        let Some(exe) = resolve_epic_executable(pilot, Path::new(&location), &launch) else {
            continue;
        };
        push_game(
            out,
            pilot,
            GamePlatform::Epic,
            exe,
            manifest.app_version_string.unwrap_or_default(),
            seen_ms,
            Vec::new(),
        );
    }
    out.len() - before
}

fn resolve_epic_executable(pilot: Pilot, install: &Path, advertised: &str) -> Option<PathBuf> {
    let advertised = install.join(advertised);
    if advertised.is_file()
        && pilot_from_executable(&advertised)
            .is_some_and(|matched| matched.catalog_id.eq_ignore_ascii_case(pilot.catalog_id))
    {
        return Some(advertised);
    }

    // Epic's Rocket League manifest currently advertises Launcher.exe. Atlas
    // follows that bounded install root to the actual allowlisted game binary
    // so running-state and session correlation use RocketLeague.exe.
    let fallbacks: &[&str] = match pilot.catalog_id {
        "fortnite" => &["FortniteGame/Binaries/Win64/FortniteClient-Win64-Shipping.exe"],
        "rocket-league" => &["Binaries/Win64/RocketLeague.exe"],
        _ => &[],
    };
    fallbacks
        .iter()
        .map(|relative| install.join(relative))
        .find(|candidate| candidate.is_file())
}

fn riot_candidate_paths() -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    roots.insert(program_drive().join("Riot Games/VALORANT/live"));
    let metadata = program_data()
        .join("Riot Games/Metadata/valorant.live/valorant.live.product_settings.yaml");
    if let Ok(text) = fs::read_to_string(metadata) {
        if let Some(root) = riot_install_root_from_metadata(&text) {
            roots.insert(root);
        }
    }
    roots
        .into_iter()
        .flat_map(|root| {
            [
                root.join("VALORANT.exe"),
                root.join("ShooterGame/Binaries/Win64/VALORANT-Win64-Shipping.exe"),
            ]
        })
        .collect()
}

fn riot_install_root_from_metadata(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if !key.trim().eq_ignore_ascii_case("product_install_full_path") {
            return None;
        }
        let value = value.trim().trim_matches('"');
        (!value.is_empty()).then(|| PathBuf::from(value.replace('/', "\\")))
    })
}

fn apex_candidate_paths() -> Vec<PathBuf> {
    let mut roots = BTreeSet::from([
        program_files().join("EA Games/Apex"),
        program_files().join("Electronic Arts/Apex"),
    ]);
    for (root, view) in [
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_CURRENT_USER, 0),
    ] {
        let Some(uninstall) = RegKey::open(
            root,
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
            view,
        ) else {
            continue;
        };
        for name in uninstall.subkey_names().into_iter().take(4096) {
            let Some(key) = uninstall.open_subkey(&name) else {
                continue;
            };
            let is_apex = key
                .get_value("DisplayName")
                .and_then(|value| value.as_str().map(str::to_owned))
                .is_some_and(|display| display.eq_ignore_ascii_case("Apex Legends"));
            if !is_apex {
                continue;
            }
            if let Some(location) = key
                .get_value("InstallLocation")
                .and_then(|value| value.as_str().map(str::to_owned))
                .filter(|value| !value.trim().is_empty())
            {
                roots.insert(PathBuf::from(location));
            }
        }
    }
    roots
        .into_iter()
        .flat_map(|root| [root.join("r5apex_dx12.exe"), root.join("r5apex.exe")])
        .collect()
}

fn discover_known_roots(
    platform: GamePlatform,
    seen_ms: i64,
    out: &mut Vec<DiscoveredGame>,
    paths: &[PathBuf],
) -> usize {
    let before = out.len();
    for exe in paths {
        if !exe.is_file() {
            continue;
        }
        let Some(pilot) = pilot_from_executable(exe) else {
            continue;
        };
        push_game(
            out,
            pilot,
            platform,
            exe.clone(),
            file_version(exe),
            seen_ms,
            vec!["Launcher version metadata was unavailable; Atlas verified the executable identity instead.".into()],
        );
    }
    out.len() - before
}

fn push_game(
    out: &mut Vec<DiscoveredGame>,
    pilot: Pilot,
    platform: GamePlatform,
    executable_path: PathBuf,
    mut version: String,
    seen_ms: i64,
    mut limitations: Vec<String>,
) {
    if version.trim().is_empty() {
        version = file_version(&executable_path);
    }
    let identity = executable_identity(&executable_path);
    if identity.is_empty() {
        limitations
            .push("The executable exists, but publisher identity could not be verified.".into());
    }
    if version.is_empty() {
        limitations.push(
            "The installed build could not be identified, so configuration changes are read-only."
                .into(),
        );
    } else {
        limitations.push("Atlas identified the installed build, but this release has no recognized writable configuration schema for it. Game-specific configuration remains read-only.".into());
    }
    let support_level = GameSupportLevel::PilotReadOnly;
    let install_path = executable_path
        .parent()
        .unwrap_or(Path::new(""))
        .to_path_buf();
    let id = stable_id(pilot.catalog_id, platform, &executable_path);
    out.push(DiscoveredGame {
        id,
        catalog_id: pilot.catalog_id.into(),
        display_name: pilot.display_name.into(),
        platform,
        executable_path,
        executable_identity: identity,
        install_path,
        version,
        support_level,
        last_seen_ms: seen_ms,
        adapter_version: GAMING_ADAPTER_VERSION.into(),
        limitations,
    });
}

fn steam_library_roots() -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for (root, view, subkey) in [
        (HKEY_CURRENT_USER, 0, r"Software\Valve\Steam"),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, r"Software\Valve\Steam"),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY, r"Software\Valve\Steam"),
    ] {
        if let Some(key) = RegKey::open(root, subkey, view) {
            for name in ["SteamPath", "InstallPath"] {
                if let Some(path) = key
                    .get_value(name)
                    .and_then(|v| v.as_str().map(str::to_owned))
                {
                    let root = PathBuf::from(path.replace('/', "\\"));
                    if root.is_dir() {
                        roots.insert(root);
                    }
                }
            }
        }
    }
    let default = program_files_x86().join("Steam");
    if default.is_dir() {
        roots.insert(default);
    }

    let initial: Vec<_> = roots.iter().cloned().collect();
    for root in initial {
        let file = root.join("steamapps/libraryfolders.vdf");
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for path in steam_paths_from_vdf(&text) {
            if path.is_dir() {
                roots.insert(path);
            }
        }
    }
    roots.into_iter().collect()
}

fn steam_paths_from_vdf(text: &str) -> Vec<PathBuf> {
    quoted_strings(text)
        .windows(2)
        .filter(|pair| {
            (pair[0].eq_ignore_ascii_case("path") || pair[0].bytes().all(|b| b.is_ascii_digit()))
                && (pair[1].contains(":\\")
                    || pair[1].contains(":/")
                    || pair[1].starts_with("\\\\"))
        })
        .map(|pair| PathBuf::from(pair[1].replace("\\\\", "\\").replace('/', "\\")))
        .collect()
}

fn quoted_pairs(text: &str) -> HashMap<String, String> {
    let tokens = quoted_tokens(text);
    let mut fields = HashMap::new();
    let mut index = 0;
    while index + 1 < tokens.len() {
        let key = &tokens[index];
        let value = &tokens[index + 1];
        let separator = &text[key.end..value.start];
        if separator.chars().all(char::is_whitespace) {
            fields.insert(key.value.to_ascii_lowercase(), value.value.clone());
            index += 2;
        } else {
            // A brace between quoted tokens means the first token names a VDF
            // object (for example AppState), not a scalar key/value pair.
            index += 1;
        }
    }
    fields
}

fn quoted_strings(text: &str) -> Vec<String> {
    quoted_tokens(text)
        .into_iter()
        .map(|token| token.value)
        .collect()
}

struct QuotedToken {
    value: String,
    start: usize,
    end: usize,
}

fn quoted_tokens(text: &str) -> Vec<QuotedToken> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut value = String::new();
        let mut end = text.len();
        while let Some((position, next)) = chars.next() {
            match next {
                '"' => {
                    end = position + next.len_utf8();
                    break;
                }
                '\\' if chars.peek().is_some_and(|(_, next)| *next == '"') => {
                    chars.next();
                    value.push('"');
                }
                other => value.push(other),
            }
        }
        tokens.push(QuotedToken { value, start, end });
    }
    tokens
}

fn pilot_from_text(text: &str) -> Option<Pilot> {
    let lower = text.to_ascii_lowercase();
    PILOTS.iter().copied().find(|p| {
        lower.contains(p.catalog_id) || lower.contains(&p.display_name.to_ascii_lowercase())
    })
}

fn pilot_from_executable(path: &Path) -> Option<Pilot> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    PILOTS
        .iter()
        .copied()
        .find(|p| p.exe_names.iter().any(|exe| name.eq_ignore_ascii_case(exe)))
}

fn file_version(path: &Path) -> String {
    let text = path.to_string_lossy();
    let Some(info) = read_version_info(&text) else {
        return String::new();
    };
    if !info.product_version.is_empty() {
        info.product_version
    } else {
        info.file_version
    }
}

fn executable_identity(path: &Path) -> String {
    let text = path.to_string_lossy();
    let sig = verify_signature_info(&text);
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    if sig.publisher.is_empty() {
        file_name
    } else {
        format!("{file_name} | {}", sig.publisher)
    }
}

fn stable_id(catalog_id: &str, platform: GamePlatform, path: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    catalog_id.hash(&mut hasher);
    platform.hash(&mut hasher);
    path.to_string_lossy()
        .to_ascii_lowercase()
        .hash(&mut hasher);
    format!("{catalog_id}-{:016x}", hasher.finish())
}

impl Hash for GamePlatform {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (*self as u8).hash(state);
    }
}

fn deduplicate(games: &mut Vec<DiscoveredGame>) {
    games.sort_by_key(|game| game.executable_path.to_string_lossy().to_ascii_lowercase());
    games.dedup_by(|a, b| {
        a.executable_path
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.executable_path.to_string_lossy())
    });
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn program_files() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| program_drive().join("Program Files"))
}

fn program_files_x86() -> PathBuf {
    std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| program_drive().join("Program Files (x86)"))
}

fn program_data() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| program_drive().join("ProgramData"))
}

fn program_drive() -> PathBuf {
    std::env::var_os("SystemDrive")
        .map(|value| absolute_drive_root(&value.to_string_lossy()))
        .unwrap_or_else(|| PathBuf::from(r"C:\"))
}

fn absolute_drive_root(value: &str) -> PathBuf {
    let value = value.trim().trim_end_matches(['\\', '/']);
    if value.is_empty() {
        PathBuf::from(r"C:\")
    } else {
        PathBuf::from(format!(r"{value}\"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_steam_library_paths_without_drive_scan() {
        let text = r#""libraryfolders" { "0" { "path" "C:\\Program Files (x86)\\Steam" } "1" { "path" "D:\\Games" } }"#;
        let paths = steam_paths_from_vdf(text);
        assert!(paths.contains(&PathBuf::from(r"C:\Program Files (x86)\Steam")));
        assert!(paths.contains(&PathBuf::from(r"D:\Games")));
    }

    #[test]
    fn pairs_parser_preserves_unknown_manifest_fields() {
        let fields = quoted_pairs(
            r#""appid" "730" "installdir" "Counter-Strike Global Offensive" "future" "kept""#,
        );
        assert_eq!(
            fields.get("installdir").map(String::as_str),
            Some("Counter-Strike Global Offensive")
        );
        assert_eq!(fields.get("future").map(String::as_str), Some("kept"));
    }

    #[test]
    fn executable_matching_is_allowlisted_to_pilot_games() {
        assert_eq!(
            pilot_from_executable(Path::new(r"C:\Games\r5apex.exe"))
                .unwrap()
                .catalog_id,
            "apex-legends"
        );
        assert_eq!(
            pilot_from_executable(Path::new(r"C:\Games\r5apex_dx12.exe"))
                .unwrap()
                .catalog_id,
            "apex-legends"
        );
        assert!(pilot_from_executable(Path::new(r"C:\Games\unknown.exe")).is_none());
    }

    #[test]
    fn system_drive_is_normalized_to_an_absolute_root() {
        assert_eq!(absolute_drive_root("C:"), PathBuf::from(r"C:\"));
        assert_eq!(absolute_drive_root(r"D:\"), PathBuf::from(r"D:\"));
    }

    #[test]
    fn riot_metadata_exposes_the_absolute_valorant_install_root() {
        let metadata = r#"
locale_data:
  default_locale: "en_US"
product_install_full_path: "D:/Riot Games/VALORANT/live"
product_install_root: "D:/Riot Games"
"#;
        assert_eq!(
            riot_install_root_from_metadata(metadata),
            Some(PathBuf::from(r"D:\Riot Games\VALORANT\live"))
        );
    }

    #[test]
    fn rocket_league_is_allowlisted_for_epic_and_executable_discovery() {
        let from_manifest = pilot_from_text("Rocket League® Live").expect("pilot manifest");
        let from_executable = pilot_from_executable(Path::new(
            r"D:\Games\rocketleague\Binaries\Win64\RocketLeague.exe",
        ))
        .expect("pilot executable");

        assert_eq!(from_manifest.catalog_id, "rocket-league");
        assert_eq!(from_executable.catalog_id, "rocket-league");
        assert_eq!(from_executable.display_name, "Rocket League");
    }

    #[test]
    fn discovers_an_installed_rocket_league_steam_manifest() {
        let root = std::env::temp_dir().join(format!(
            "atlas-rocket-league-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let steamapps = root.join("steamapps");
        let exe = steamapps.join("common/rocketleague/Binaries/Win64/RocketLeague.exe");
        fs::create_dir_all(exe.parent().expect("fixture parent")).expect("fixture directories");
        fs::write(&exe, []).expect("fixture executable");
        fs::write(
            steamapps.join("appmanifest_252950.acf"),
            r#""AppState" { "appid" "252950" "installdir" "rocketleague" "buildid" "12345" }"#,
        )
        .expect("fixture manifest");

        let mut games = Vec::new();
        assert_eq!(
            discover_steam(std::slice::from_ref(&root), 42, &mut games),
            1
        );
        assert_eq!(games[0].catalog_id, "rocket-league");
        assert_eq!(games[0].platform, GamePlatform::Steam);
        assert_eq!(games[0].version, "12345");
        assert_eq!(games[0].support_level, GameSupportLevel::PilotReadOnly);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn epic_rocket_league_manifest_resolves_launcher_to_game_binary() {
        let root = std::env::temp_dir().join(format!(
            "atlas-rocket-league-epic-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let manifests = root.join("manifests");
        let install = root.join("rocketleague");
        let launcher = install.join("Binaries/Win64/Launcher.exe");
        let game = install.join("Binaries/Win64/RocketLeague.exe");
        fs::create_dir_all(&manifests).expect("manifest directory");
        fs::create_dir_all(launcher.parent().expect("launcher parent")).expect("install directory");
        fs::write(&launcher, []).expect("launcher fixture");
        fs::write(&game, []).expect("game fixture");
        let manifest = serde_json::json!({
            "DisplayName": "Rocket League®",
            "InstallLocation": install,
            "LaunchExecutable": "Binaries/Win64/Launcher.exe",
            "AppVersionString": "test-build"
        });
        fs::write(
            manifests.join("rocket-league.item"),
            serde_json::to_vec(&manifest).expect("manifest JSON"),
        )
        .expect("manifest fixture");

        let mut games = Vec::new();
        assert_eq!(discover_epic(&manifests, 42, &mut games), 1);
        assert_eq!(games[0].catalog_id, "rocket-league");
        assert_eq!(games[0].platform, GamePlatform::Epic);
        assert_eq!(games[0].executable_path, game);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn epic_fortnite_manifest_resolves_bootstrapper_to_game_binary() {
        let root = std::env::temp_dir().join(format!(
            "atlas-fortnite-epic-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let manifests = root.join("manifests");
        let install = root.join("Fortnite");
        let binaries = install.join("FortniteGame/Binaries/Win64");
        let bootstrapper = binaries.join("FortniteBootstrapper.exe");
        let game = binaries.join("FortniteClient-Win64-Shipping.exe");
        fs::create_dir_all(&manifests).expect("manifest directory");
        fs::create_dir_all(&binaries).expect("install directory");
        fs::write(&bootstrapper, []).expect("bootstrapper fixture");
        fs::write(&game, []).expect("game fixture");
        let manifest = serde_json::json!({
            "DisplayName": "Fortnite",
            "InstallLocation": install,
            "LaunchExecutable": "FortniteGame/Binaries/Win64/FortniteBootstrapper.exe",
            "AppVersionString": "test-build"
        });
        fs::write(
            manifests.join("fortnite.item"),
            serde_json::to_vec(&manifest).expect("manifest JSON"),
        )
        .expect("manifest fixture");

        let mut games = Vec::new();
        assert_eq!(discover_epic(&manifests, 42, &mut games), 1);
        assert_eq!(games[0].catalog_id, "fortnite");
        assert_eq!(games[0].platform, GamePlatform::Epic);
        assert_eq!(games[0].executable_path, game);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn pilot_support_does_not_imply_a_writable_config_adapter() {
        assert_ne!(
            GameSupportLevel::PilotReadOnly,
            GameSupportLevel::PilotVersionGated
        );
    }
}
