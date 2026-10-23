// Global keyboard shortcuts, one per tool, in "Super+Shift+V" form.
// Linux: GNOME doesn't pass Super combinations to apps, so each shortcut is registered in
// GNOME itself as a custom keybinding that runs "toolswitch <tool>"; that reaches the running
// app through the single-instance socket.
// Windows, macOS and other Linux desktops (KDE, XFCE...): global-hotkey registers them, and a
// press calls the app's handler.

use crate::config;
use eframe::egui;

/// (shortcut id, name shown in GNOME's keyboard settings, default shortcut). The id's first
/// part is the tool it belongs to ("wallpaper-next" -> "wallpaper").
pub const TOOLS: &[(&str, &str, &str)] = &[
    (
        "clipboard",
        "toolswitch: clipboard history",
        "Super+Shift+V",
    ),
    ("wallpaper", "toolswitch: wallpapers", "Super+Shift+W"),
    ("compress", "toolswitch: compress files", "Super+Shift+T"),
    ("ocr", "toolswitch: text from screen", "Super+Shift+X"),
    ("cutout", "toolswitch: remove background", "Super+Shift+B"),
    ("reminders", "toolswitch: new reminder", "Super+Shift+R"),
    ("calendar", "toolswitch: calendar", "Super+Shift+E"),
    ("sign", "toolswitch: sign PDF", "Super+Shift+S"),
    ("breaks", "toolswitch: breaks", "Super+Shift+P"),
    ("organize", "toolswitch: tidy downloads", "Super+Shift+O"),
    ("backup", "toolswitch: backup", "Super+Shift+K"),
    ("cleanup", "toolswitch: clean up", "Super+Shift+L"),
    ("monitor", "toolswitch: watch the PC", "Super+Shift+M"),
    ("power", "toolswitch: shut down timer", "Super+Shift+Q"),
    ("updates", "toolswitch: updates", "Super+Shift+U"),
    ("jira", "toolswitch: Jira issues", "Super+Shift+J"),
    (
        "wallpaper-random",
        "toolswitch: random wallpaper",
        "Super+W",
    ),
    ("wallpaper-next", "toolswitch: next wallpaper", "Super+."),
    (
        "wallpaper-prev",
        "toolswitch: previous wallpaper",
        "Super+,",
    ),
];

pub fn get(tool: &str) -> String {
    config::load()
        .shortcuts
        .get(tool)
        .cloned()
        .unwrap_or_else(|| default(tool))
}

/// The shortcut it comes with. Windows already uses many Windows-key combinations, so there
/// they're Ctrl+Alt; on macOS Cmd+Option, since Cmd+Shift+V is "paste as plain text".
fn default(tool: &str) -> String {
    let accel = TOOLS.iter().find(|t| t.0 == tool).map_or("", |t| t.2);
    if cfg!(windows) {
        accel
            .replace("Super+Shift+", "Ctrl+Alt+")
            .replace("Super+", "Ctrl+Alt+")
    } else if cfg!(target_os = "macos") {
        accel.replace("Super+Shift+", "Super+Alt+")
    } else {
        accel.into()
    }
}

pub fn register_all() {
    for (tool, name, _) in TOOLS {
        // A turned-off tool keeps its keybinding in GNOME, but with no keys.
        let on = config::enabled(tool.split('-').next().unwrap_or(tool));
        let accel = if on { get(tool) } else { String::new() };
        if let Err(error) = register(tool, name, &accel) {
            eprintln!("Could not register the {tool} shortcut in GNOME: {error}");
        }
    }
}

/// `accel = None` goes back to the default. If GNOME refuses it, the old one stays.
pub fn set(tool: &str, accel: Option<String>) -> bool {
    let old = config::load().shortcuts.get(tool).cloned();
    let apply = |accel: Option<String>| {
        config::update(|c| match accel {
            Some(a) => _ = c.shortcuts.insert(tool.into(), a),
            None => _ = c.shortcuts.remove(tool),
        });
    };
    apply(accel);
    let name = TOOLS.iter().find(|t| t.0 == tool).map_or(tool, |t| t.1);
    if register(tool, name, &get(tool)).is_ok() {
        return true;
    }
    apply(old);
    let _ = register(tool, name, &get(tool));
    false
}

pub use hotkeys::set_handler;

pub fn register(tool: &str, name: &str, accel: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if gnome::available() {
        return gnome::register(tool, name, accel);
    }
    hotkeys::register(tool, name, accel)
}

// ---------- GNOME ----------

#[cfg(target_os = "linux")]
mod gnome {
    use crate::desktop;
    use std::process::Command;

    const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";

    fn gsettings(args: &[&str]) -> Result<String, String> {
        let out = Command::new("gsettings")
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().into())
    }

    /// GVariant string: 'it\'s'
    fn gvariant(s: &str) -> String {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }

    /// GNOME (and Ubuntu's Unity) take the shortcuts themselves.
    pub fn available() -> bool {
        std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| {
            d.split(':')
                .any(|d| d.eq_ignore_ascii_case("GNOME") || d.eq_ignore_ascii_case("Unity"))
        })
    }

    pub fn register(tool: &str, name: &str, accel: &str) -> Result<(), String> {
        let path = format!(
            "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/toolswitch-{tool}/"
        );
        let schema = format!("{MEDIA_KEYS}.custom-keybinding:{path}");
        gsettings(&["set", &schema, "name", &gvariant(name)])?;
        gsettings(&[
            "set",
            &schema,
            "command",
            &gvariant(&desktop::command_line(&[tool])),
        ])?;
        gsettings(&["set", &schema, "binding", &gvariant(&to_gnome(accel))])?;

        // "@as []" or "['/a/', '/b/']"
        let list = gsettings(&["get", MEDIA_KEYS, "custom-keybindings"])?;
        let mut paths: Vec<String> = list
            .trim_start_matches("@as")
            .trim()
            .trim_matches(|c| c == '[' || c == ']')
            .split(',')
            .map(|p| p.trim().trim_matches('\'').to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if !paths.contains(&path) {
            paths.push(path);
            let value = format!(
                "[{}]",
                paths
                    .iter()
                    .map(|p| gvariant(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            gsettings(&["set", MEDIA_KEYS, "custom-keybindings", &value])?;
        }
        Ok(())
    }

    /// "Super+Shift+V" -> "<Super><Shift>v"
    fn to_gnome(accel: &str) -> String {
        let mut out = String::new();
        for part in accel.split('+') {
            match part {
                "Super" => out.push_str("<Super>"),
                "Shift" => out.push_str("<Shift>"),
                "Alt" => out.push_str("<Alt>"),
                "Ctrl" => out.push_str("<Primary>"),
                key => out.push_str(&match key {
                    "." => "period".into(),
                    "," => "comma".into(),
                    "-" => "minus".into(),
                    "=" => "equal".into(),
                    "/" => "slash".into(),
                    ";" => "semicolon".into(),
                    "Space" => "space".into(),
                    k if k.chars().count() == 1 => k.to_lowercase(),
                    k => k.into(),
                }),
            }
        }
        out
    }
}

// ---------- everywhere else: global-hotkey ----------

mod hotkeys {
    use global_hotkey::hotkey::HotKey;
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::{Mutex, OnceLock};

    /// Shortcut id ("clipboard") for each registered key combination.
    static IDS: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();

    thread_local! {
        /// Lives on the main thread, like the system wants.
        static MANAGER: RefCell<Option<GlobalHotKeyManager>> = const { RefCell::new(None) };
        static REGISTERED: RefCell<HashMap<String, HotKey>> = RefCell::new(HashMap::new());
    }

    fn ids() -> std::sync::MutexGuard<'static, HashMap<u32, String>> {
        IDS.get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// What a press does: gets the shortcut id, like a second launch's command line.
    pub fn set_handler(handler: impl Fn(String) + Send + Sync + 'static) {
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state == HotKeyState::Pressed
                && let Some(id) = ids().get(&event.id).cloned()
            {
                handler(id);
            }
        }));
    }

    /// Main thread only. An empty `accel` takes the shortcut away.
    pub fn register(tool: &str, _name: &str, accel: &str) -> Result<(), String> {
        MANAGER.with(|m| {
            let mut m = m.borrow_mut();
            if m.is_none() {
                *m = Some(GlobalHotKeyManager::new().map_err(|e| e.to_string())?);
            }
            let manager = m.as_ref().unwrap();
            REGISTERED.with(|r| {
                let mut registered = r.borrow_mut();
                if let Some(old) = registered.remove(tool) {
                    let _ = manager.unregister(old);
                    ids().remove(&old.id());
                }
                if accel.is_empty() {
                    return Ok(());
                }
                let hotkey = HotKey::from_str(accel).map_err(|e| e.to_string())?;
                manager.register(hotkey).map_err(|e| e.to_string())?;
                ids().insert(hotkey.id(), tool.to_string());
                registered.insert(tool.to_string(), hotkey);
                Ok(())
            })
        })
    }
}

// ---------- recording ----------

pub enum Recorded {
    Keys(String),
    /// Backspace: back to the default.
    Default,
    Cancel,
}

/// Looks at this frame's key presses for a new shortcut. Esc cancels, Backspace asks for the default.
pub fn record(ctx: &egui::Context) -> Option<Recorded> {
    let pressed = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => Some((*key, *modifiers)),
            _ => None,
        })
    })?;
    let (key, mods) = pressed;
    match key {
        egui::Key::Escape => return Some(Recorded::Cancel),
        egui::Key::Backspace => return Some(Recorded::Default),
        _ => {}
    }
    let name = key_name(key)?;
    // egui doesn't report Super on Linux, so it is asked to X11.
    let parts: Vec<&str> = [
        (mods.ctrl, "Ctrl"),
        // macOS: Cmd is Super. Linux: egui doesn't see Super, X11 is asked.
        (
            crate::screen::super_down() || (cfg!(target_os = "macos") && mods.mac_cmd),
            "Super",
        ),
        (mods.alt, "Alt"),
        (mods.shift, "Shift"),
    ]
    .into_iter()
    .filter_map(|(down, name)| down.then_some(name))
    .collect();
    // Without Ctrl/Alt/Super the keys would fire while typing anywhere (Shift+W is just a
    // capital W); function keys are the exception.
    let is_function = name.starts_with('F') && name.len() > 1;
    if !parts.iter().any(|p| *p != "Shift") && !is_function {
        return None;
    }
    Some(Recorded::Keys(
        parts
            .into_iter()
            .chain([name.as_str()])
            .collect::<Vec<_>>()
            .join("+"),
    ))
}

fn key_name(key: egui::Key) -> Option<String> {
    use egui::Key;
    let name = key.name();
    Some(match key {
        Key::Period => ".".into(),
        Key::Comma => ",".into(),
        Key::Minus => "-".into(),
        Key::Equals => "=".into(),
        Key::Slash => "/".into(),
        Key::Semicolon => ";".into(),
        Key::Space => "Space".into(),
        Key::ArrowUp => "Up".into(),
        Key::ArrowDown => "Down".into(),
        Key::ArrowLeft => "Left".into(),
        Key::ArrowRight => "Right".into(),
        _ if name.len() == 1 && name.chars().all(|c| c.is_ascii_alphanumeric()) => {
            name.to_uppercase()
        }
        _ if name.starts_with('F') && name[1..].parse::<u8>().is_ok() => name.into(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    /// Every default shortcut, as each system writes it, is understood by global-hotkey.
    #[test]
    fn defaults_parse() {
        for (tool, _, accel) in super::TOOLS {
            for variant in [
                accel.to_string(),
                accel.replace("Super+Shift+", "Ctrl+Alt+").replace("Super+", "Ctrl+Alt+"),
                accel.replace("Super+Shift+", "Super+Alt+"),
            ] {
                assert!(
                    global_hotkey::hotkey::HotKey::from_str(&variant).is_ok(),
                    "{tool}: {variant}"
                );
            }
        }
    }
}
