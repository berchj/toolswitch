// Settings shared by every tool, saved in ~/.config/toolswitch/config.json.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Recorded shortcuts by tool id ("clipboard", ...). A missing tool uses its default.
    pub shortcuts: BTreeMap<String, String>,
    pub clipboard_paused: bool,
    /// Tools turned off in the main window. Everything is on by default...
    pub disabled: BTreeSet<String>,
    /// ...except the ones in OFF_BY_DEFAULT, which are on only when listed here.
    pub enabled: BTreeSet<String>,
    /// Start at login is turned on the first time; after that the user's choice sticks.
    pub autostart_set: bool,
    /// The welcome screen was seen.
    pub welcomed: bool,
}

/// Tools that move or delete files, or interrupt you, start turned off.
pub const OFF_BY_DEFAULT: &[&str] = &["organize", "backup", "breaks", "jira", "calendar"];

/// Tools taken out of the app for now: not shown anywhere and never running. Their code stays.
pub const HIDDEN: &[&str] = &[
    "calendar", "jira", "security", "updates", "backup", "cleanup", "organize", "monitor",
];

pub fn hidden(tool: &str) -> bool {
    HIDDEN.contains(&tool)
}

pub fn enabled(tool: &str) -> bool {
    if hidden(tool) {
        return false;
    }
    with(|config| {
        if OFF_BY_DEFAULT.contains(&tool) {
            config.enabled.contains(tool)
        } else {
            !config.disabled.contains(tool)
        }
    })
}

pub fn set_enabled(tool: &str, on: bool) {
    update(|c| {
        if OFF_BY_DEFAULT.contains(&tool) {
            if on {
                c.enabled.insert(tool.into());
            } else {
                c.enabled.remove(tool);
            }
        } else if on {
            c.disabled.remove(tool);
        } else {
            c.disabled.insert(tool.into());
        }
    });
}

/// A tool's own settings file in the config folder, or its defaults.
pub fn read<T: serde::de::DeserializeOwned + Default>(name: &str) -> T {
    std::fs::read(dir().join(name))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

pub fn write<T: Serialize>(name: &str, value: &T) {
    if let Err(error) = write_json(&dir().join(name), value) {
        eprintln!("Could not save {name}: {error}");
    }
}

/// Where apps keep their settings: ~/.config on Linux, AppData\Roaming on Windows,
/// ~/Library/Application Support on macOS.
pub fn config_dir() -> PathBuf {
    if cfg!(windows) {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_default();
    }
    if cfg!(target_os = "macos") {
        return home().join("Library/Application Support");
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

/// toolswitch's cache (thumbnails, AI models): ~/.cache, AppData\Local or ~/Library/Caches.
pub fn cache_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    } else if cfg!(target_os = "macos") {
        home().join("Library/Caches")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".cache"))
    };
    base.join("toolswitch")
}

/// A user folder: "DOWNLOAD", "DOCUMENTS", "PICTURES", "VIDEOS" or "MUSIC". On Linux its
/// name comes from user-dirs.dirs, so it's "Descargas" on a computer in Spanish.
pub fn user_dir(xdg: &str, default: &str) -> PathBuf {
    #[cfg(windows)]
    if let Some(dir) = windows_known_folder(xdg) {
        return dir;
    }
    let home = home();
    if cfg!(all(unix, not(target_os = "macos"))) {
        let key = format!("XDG_{xdg}_DIR=");
        let from_xdg = std::fs::read_to_string(config_dir().join("user-dirs.dirs"))
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|l| l.strip_prefix(&key).map(str::to_string))
            })
            .map(|v| {
                PathBuf::from(
                    v.trim_matches('"')
                        .replace("$HOME", &home.to_string_lossy()),
                )
            });
        if let Some(dir) = from_xdg {
            return dir;
        }
    }
    home.join(default)
}

/// A path to show people: the home folder as "~".
pub fn short(path: &Path) -> String {
    let text = path.to_string_lossy();
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
        }
        _ => text.into_owned(),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn dir() -> PathBuf {
    config_dir().join("toolswitch")
}

fn file() -> PathBuf {
    dir().join("config.json")
}

/// The settings, read from the file once and then kept here: the window asks for them every
/// frame and the background thread every few seconds. Only this app writes the file.
static CACHE: Mutex<Option<Config>> = Mutex::new(None);

/// Runs `f` on the settings, reading the file the first time.
fn with<R>(f: impl FnOnce(&mut Config) -> R) -> R {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let config = cache.get_or_insert_with(|| {
        std::fs::read(file())
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default()
    });
    f(config)
}

pub fn load() -> Config {
    with(|c| c.clone())
}

/// The shortcut recorded for a tool, without copying the rest of the settings.
pub fn shortcut(tool: &str) -> Option<String> {
    with(|c| c.shortcuts.get(tool).cloned())
}

/// Changes the settings and saves them. `change` must not call back into this module.
pub fn update(change: impl FnOnce(&mut Config)) -> Config {
    with(|config| {
        change(config);
        let _ = write_json(&file(), config);
        config.clone()
    })
}

/// Writes to a temporary file first, so a crash never leaves half a file.
pub fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)
}

/// The real place of a user folder on Windows, which may be moved (to OneDrive, say).
#[cfg(windows)]
fn windows_known_folder(xdg: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
        FOLDERID_Videos, SHGetKnownFolderPath,
    };
    let id = match xdg {
        "DOWNLOAD" => FOLDERID_Downloads,
        "DOCUMENTS" => FOLDERID_Documents,
        "PICTURES" => FOLDERID_Pictures,
        "VIDEOS" => FOLDERID_Videos,
        "MUSIC" => FOLDERID_Music,
        _ => return None,
    };
    let mut path = std::ptr::null_mut();
    // SAFETY: the system writes a string it allocated into `path`; it's read up to its end
    // and then freed with the function the documentation names.
    unsafe {
        if SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut path) != 0 || path.is_null() {
            return None;
        }
        let len = (0..).take_while(|&i| *path.add(i) != 0).count();
        let dir = std::ffi::OsString::from_wide(std::slice::from_raw_parts(path, len));
        CoTaskMemFree(path.cast());
        Some(dir.into())
    }
}
