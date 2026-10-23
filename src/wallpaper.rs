// Wallpapers: previous / next / random from the user's folders, and an automatic change every
// few minutes. Settings live in ~/.config/toolswitch/wallpaper.json, in the shape wallswitch used,
// and are brought over from wallswitch the first time.

use crate::config;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp"];
#[cfg(all(unix, not(target_os = "macos")))]
const SCHEMA: &str = "org.gnome.desktop.background";

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Folders scanned for images; None means the default ones.
    pub dirs: Option<Vec<PathBuf>>,
    /// Single images added by hand.
    pub files: Vec<PathBuf>,
    /// Images removed from the list (the files stay on disk).
    pub hidden: Vec<PathBuf>,
    /// Images picked for the carousel; empty means all of them.
    pub playlist: Vec<PathBuf>,
    /// Minutes between automatic changes.
    pub interval: u64,
    pub rotate: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            dirs: None,
            files: vec![],
            hidden: vec![],
            playlist: vec![],
            interval: 15,
            rotate: false,
        }
    }
}

fn file() -> PathBuf {
    config::dir().join("wallpaper.json")
}

pub fn load() -> Settings {
    let read = |p: PathBuf| {
        std::fs::read(p)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
    };
    read(file())
        .or_else(|| read(config::config_dir().join("wallswitch").join("config.json")))
        .unwrap_or_default()
}

pub fn update(change: impl FnOnce(&mut Settings)) -> Settings {
    let mut settings = load();
    change(&mut settings);
    let _ = config::write_json(&file(), &settings);
    settings
}

// ---------- images ----------

fn home() -> PathBuf {
    config::home()
}

fn pictures_dir() -> PathBuf {
    config::user_dir("PICTURES", "Pictures")
}

pub fn dirs(settings: &Settings) -> Vec<PathBuf> {
    settings.dirs.clone().unwrap_or_else(|| {
        let mut dirs = vec![home().join("Pictures/Wallpapers")];
        let local = pictures_dir().join("Wallpapers");
        if !dirs.contains(&local) {
            dirs.push(local);
        }
        // The ones that come with the system.
        if cfg!(windows) {
            dirs.push("C:\\Windows\\Web\\Wallpaper".into());
        } else if cfg!(target_os = "macos") {
            dirs.push("/System/Library/Desktop Pictures".into());
        } else {
            dirs.push(home().join(".local/share/backgrounds"));
            dirs.push("/usr/share/backgrounds".into());
        }
        dirs
    })
}

fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.contains(&e.to_lowercase().as_str()))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if is_image(&path) {
            out.push(path);
        }
    }
}

/// Every image in the folders plus the ones added by hand, sorted by folder then name.
pub fn scan(settings: &Settings) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = settings
        .files
        .iter()
        .filter(|f| f.exists())
        .cloned()
        .collect();
    for dir in dirs(settings) {
        walk(&dir, &mut found);
    }
    let hidden: HashSet<_> = settings.hidden.iter().collect();
    let mut seen = HashSet::new();
    found.retain(|f| !hidden.contains(f) && seen.insert(f.clone()));
    sort(&mut found);
    found
}

fn sort(files: &mut [PathBuf]) {
    files.sort_by_key(|p| {
        (
            p.parent().map(Path::to_path_buf),
            p.file_name().map(|n| n.to_string_lossy().to_lowercase()),
        )
    });
}

/// Carousel images in order; all images if none were picked.
pub fn carousel(settings: &Settings) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = settings
        .playlist
        .iter()
        .filter(|f| f.exists())
        .cloned()
        .collect();
    if list.is_empty() {
        return scan(settings);
    }
    sort(&mut list);
    list
}

// ---------- the desktop's wallpaper ----------

pub use desktop::{apply, current};

#[cfg(all(unix, not(target_os = "macos")))]
mod desktop {
    use super::{SCHEMA, percent_decode, percent_encode};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;

    /// The last one set, for desktops that can't be asked which one is up.
    static LAST: Mutex<Option<PathBuf>> = Mutex::new(None);

    fn run(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn gsettings(args: &[&str]) -> Option<String> {
        run("gsettings", args)
    }

    fn desktop() -> String {
        std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_uppercase()
    }

    pub fn current() -> Option<PathBuf> {
        let d = desktop();
        let from_desktop = if d.contains("CINNAMON") {
            gsettings(&["get", "org.cinnamon.desktop.background", "picture-uri"])
        } else if d.contains("MATE") {
            gsettings(&["get", "org.mate.background", "picture-filename"])
                .map(|p| format!("file://{}", p.trim_matches('\'')))
        } else if d.contains("KDE") || d.contains("XFCE") {
            None
        } else {
            gsettings(&["get", SCHEMA, "picture-uri"])
        };
        from_desktop
            .and_then(|uri| {
                let path = uri.trim_matches('\'').strip_prefix("file://")?.to_string();
                Some(PathBuf::from(percent_decode(&path)))
            })
            .or_else(|| LAST.lock().ok()?.clone())
    }

    pub fn apply(file: &Path) -> Result<(), String> {
        let path = file.to_string_lossy();
        let uri = format!("file://{}", percent_encode(&path));
        let d = desktop();
        if d.contains("KDE") {
            run("plasma-apply-wallpaperimage", &[&path]).ok_or("Plasma refused the wallpaper")?;
        } else if d.contains("XFCE") {
            // One setting per monitor and workspace.
            let props = run("xfconf-query", &["-c", "xfce4-desktop", "-l"])
                .ok_or("Couldn't reach XFCE's settings")?;
            for prop in props.lines().filter(|p| p.ends_with("/last-image")) {
                run("xfconf-query", &["-c", "xfce4-desktop", "-p", prop, "-s", &path]);
            }
        } else if d.contains("CINNAMON") {
            gsettings(&["set", "org.cinnamon.desktop.background", "picture-uri", &uri])
                .ok_or("Cinnamon refused the wallpaper")?;
        } else if d.contains("MATE") {
            gsettings(&["set", "org.mate.background", "picture-filename", &path])
                .ok_or("MATE refused the wallpaper")?;
        } else if let Some(keys) = gsettings(&["list-keys", SCHEMA]) {
            for key in ["picture-uri", "picture-uri-dark"] {
                if keys.lines().any(|k| k == key) {
                    gsettings(&["set", SCHEMA, key, &uri]).ok_or("GNOME refused the wallpaper")?;
                }
            }
        } else {
            // Window managers without a desktop: feh is the usual way.
            run("feh", &["--bg-fill", &path]).ok_or("Couldn't set the wallpaper on this desktop")?;
        }
        if let Ok(mut last) = LAST.lock() {
            *last = Some(file.to_path_buf());
        }
        Ok(())
    }
}

#[cfg(windows)]
mod desktop {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETDESKWALLPAPER, SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
        SystemParametersInfoW,
    };

    pub fn current() -> Option<PathBuf> {
        let mut buffer = [0u16; 1024];
        // SAFETY: the buffer is as long as the size given.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETDESKWALLPAPER,
                buffer.len() as u32,
                buffer.as_mut_ptr().cast(),
                0,
            )
        };
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(0);
        (ok != 0 && len > 0).then(|| std::ffi::OsString::from_wide(&buffer[..len]).into())
    }

    pub fn apply(file: &Path) -> Result<(), String> {
        let wide: Vec<u16> = file.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: the path is a valid, zero-terminated wide string.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_SETDESKWALLPAPER,
                0,
                wide.as_ptr() as *mut _,
                SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
            )
        };
        if ok != 0 {
            Ok(())
        } else {
            Err("Windows refused the wallpaper".into())
        }
    }
}

#[cfg(target_os = "macos")]
mod desktop {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn osascript(script: &str) -> Option<String> {
        let out = Command::new("osascript")
            .args(["-e", script])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn current() -> Option<PathBuf> {
        osascript("tell application \"System Events\" to get picture of desktop 1")
            .map(PathBuf::from)
    }

    pub fn apply(file: &Path) -> Result<(), String> {
        let path = file
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        osascript(&format!(
            "tell application \"System Events\" to tell every desktop to set picture to POSIX file \"{path}\""
        ))
        .map(|_| ())
        .ok_or_else(|| "macOS refused the wallpaper".into())
    }
}

/// Next (+1) or previous (-1) image of the carousel.
pub fn step(delta: i64) -> Result<PathBuf, String> {
    let files = carousel(&load());
    if files.is_empty() {
        return Err("No wallpapers found.".into());
    }
    let n = files.len() as i64;
    let next = match current().and_then(|c| files.iter().position(|f| *f == c)) {
        Some(i) => (i as i64 + delta).rem_euclid(n),
        None if delta > 0 => 0,
        None => n - 1,
    };
    let file = files[next as usize].clone();
    apply(&file)?;
    Ok(file)
}

pub fn random() -> Result<PathBuf, String> {
    let current = current();
    let files: Vec<_> = carousel(&load())
        .into_iter()
        .filter(|f| Some(f) != current.as_ref())
        .collect();
    if files.is_empty() {
        return Err("No other wallpapers available.".into());
    }
    // No rand crate needed: the clock's nanoseconds are random enough to pick a wallpaper.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let file = files[nanos as usize % files.len()].clone();
    apply(&file)?;
    Ok(file)
}

/// Runs a shortcut action: "wallpaper-next", "wallpaper-prev" or "wallpaper-random".
pub fn run(action: &str) {
    let result = match action {
        "wallpaper-next" => step(1),
        "wallpaper-prev" => step(-1),
        "wallpaper-random" => random(),
        _ => return,
    };
    if let Err(error) = result {
        eprintln!("{error}");
    }
}

/// Changes the wallpaper every `interval` minutes while rotation is on.
pub fn start_rotation() {
    std::thread::spawn(|| {
        let mut last = Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(20));
            let settings = load();
            let every = Duration::from_secs(settings.interval.max(1) * 60);
            if !settings.rotate || !config::enabled("wallpaper") {
                last = Instant::now();
            } else if last.elapsed() >= every {
                last = Instant::now();
                run("wallpaper-next");
            }
        }
    });
}

// ---------- file:// URIs ----------

#[cfg(all(unix, not(target_os = "macos")))]
fn percent_encode(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------- editing the list ----------

/// Adds or takes an image out of the carousel.
pub fn toggle_playlist(file: &Path) {
    update(|s| {
        if let Some(i) = s.playlist.iter().position(|p| p == file) {
            s.playlist.remove(i);
        } else {
            s.playlist.push(file.to_path_buf());
        }
    });
}

/// Takes an image off the list without touching the file on disk.
pub fn remove_image(file: &Path) {
    update(|s| {
        if s.files.iter().any(|p| p == file) {
            s.files.retain(|p| p != file);
        } else if !s.hidden.iter().any(|p| p == file) {
            s.hidden.push(file.to_path_buf());
        }
        s.playlist.retain(|p| p != file);
    });
}

pub fn add_dir(dir: PathBuf) {
    update(|s| {
        let mut list = dirs(s);
        if !list.contains(&dir) {
            list.push(dir);
        }
        s.dirs = Some(list);
    });
}

/// Single images go straight into the carousel.
pub fn add_files(files: Vec<PathBuf>) {
    let files: Vec<PathBuf> = files.into_iter().filter(|f| is_image(f)).collect();
    update(|s| {
        for f in files {
            s.hidden.retain(|p| *p != f);
            if !s.files.contains(&f) {
                s.files.push(f.clone());
            }
            if !s.playlist.contains(&f) {
                s.playlist.push(f);
            }
        }
    });
}
