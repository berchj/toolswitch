// Fitting into the desktop on each system: start at login, the app menu entry (Linux), the
// right-click menu in Files (Linux), notifications, and running helper programs.

use crate::config;
use std::path::PathBuf;

/// The program to launch. An AppImage runs from a new temporary folder each time,
/// so point at the AppImage file itself; a snap's folder changes with each update, so point
/// at its fixed launcher.
fn executable() -> String {
    std::env::var("APPIMAGE")
        .ok()
        .or_else(|| std::env::var("SNAP_NAME").ok().map(|name| format!("/snap/bin/{name}")))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "toolswitch".into())
}

/// "/path/toolswitch" "clipboard"
pub fn command_line(args: &[&str]) -> String {
    std::iter::once(executable().as_str())
        .chain(args.iter().copied())
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A helper program to run. On Windows it gets no console window of its own.
pub fn command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    // Started from Finder or at login, macOS apps get a short PATH without Homebrew.
    #[cfg(target_os = "macos")]
    {
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{path}:/opt/homebrew/bin:/usr/local/bin"));
    }
    command
}

/// Starts a program and doesn't wait for it, but something does: a finished program nobody
/// waited for stays as a "zombie" process, and those pile up in an app that runs for weeks.
pub fn start(mut command: std::process::Command) {
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Where the app's icon is saved for menu entries and notifications, written if missing.
fn icon_file() -> PathBuf {
    let file = home().join(".local/share/icons/hicolor/256x256/apps/toolswitch.png");
    if std::fs::read(&file).ok().as_deref() != Some(crate::theme::APP_ICON_PNG) {
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let _ = std::fs::write(&file, crate::theme::APP_ICON_PNG);
    }
    file
}

fn home() -> PathBuf {
    config::home()
}

// ---------- start at login ----------

/// Start at login, as last read or set. Asking the system costs a process on Windows (reg),
/// and the window asks every frame, so the answer is kept: -1 unknown, 0 off, 1 on.
static AUTOSTART: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

pub fn autostart_enabled() -> bool {
    use std::sync::atomic::Ordering;
    match AUTOSTART.load(Ordering::Relaxed) {
        -1 => {
            let on = autostart::autostart_enabled();
            AUTOSTART.store(on as i8, Ordering::Relaxed);
            on
        }
        known => known == 1,
    }
}

pub fn set_autostart(on: bool) -> std::io::Result<()> {
    use std::sync::atomic::Ordering;
    let result = autostart::set_autostart(on);
    // On failure, ask the system again next time.
    AUTOSTART.store(if result.is_ok() { on as i8 } else { -1 }, Ordering::Relaxed);
    result
}

#[cfg(all(unix, not(target_os = "macos")))]
mod autostart {
    use super::command_line;
    use crate::config;
    use std::path::PathBuf;

    fn file() -> PathBuf {
        config::config_dir()
            .join("autostart")
            .join("toolswitch.desktop")
    }

    pub fn autostart_enabled() -> bool {
        file().exists()
    }

    pub fn set_autostart(on: bool) -> std::io::Result<()> {
        let file = file();
        if !on {
            return std::fs::remove_file(file).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            });
        }
        std::fs::create_dir_all(file.parent().unwrap())?;
        std::fs::write(
            file,
            format!(
                "[Desktop Entry]\nType=Application\nName=toolswitch\nExec={}\nIcon={}\nX-GNOME-Autostart-enabled=true\n",
                command_line(&["--hidden"]),
                super::icon_file().display()
            ),
        )
    }
}

/// Windows: a value in the registry's Run key.
#[cfg(windows)]
mod autostart {
    use super::{command, command_line};

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn autostart_enabled() -> bool {
        command("reg")
            .args(["query", KEY, "/v", "toolswitch"])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    pub fn set_autostart(on: bool) -> std::io::Result<()> {
        let mut reg = command("reg");
        if on {
            reg.args(["add", KEY, "/v", "toolswitch", "/t", "REG_SZ", "/f", "/d"])
                .arg(command_line(&["--hidden"]));
        } else {
            reg.args(["delete", KEY, "/v", "toolswitch", "/f"]);
        }
        let status = reg.status()?;
        if status.success() || !on {
            Ok(())
        } else {
            Err(std::io::Error::other("reg failed"))
        }
    }
}

/// macOS: a LaunchAgent that runs it at login.
#[cfg(target_os = "macos")]
mod autostart {
    use super::{executable, home};
    use std::path::PathBuf;

    fn file() -> PathBuf {
        home().join("Library/LaunchAgents/com.berchj.toolswitch.plist")
    }

    pub fn autostart_enabled() -> bool {
        file().exists()
    }

    pub fn set_autostart(on: bool) -> std::io::Result<()> {
        let file = file();
        if !on {
            return std::fs::remove_file(file).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            });
        }
        let exe = executable().replace('&', "&amp;").replace('<', "&lt;");
        std::fs::create_dir_all(file.parent().unwrap())?;
        std::fs::write(
            file,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.berchj.toolswitch</string>
    <key>ProgramArguments</key><array><string>{exe}</string><string>--hidden</string></array>
    <key>RunAtLoad</key><true/>
</dict>
</plist>
"#
            ),
        )
    }
}

/// Start at login is on by default; if it's turned off later, that sticks.
pub fn autostart_first_run() {
    if config::load().autostart_set {
        return;
    }
    if let Err(error) = set_autostart(true) {
        eprintln!("Could not turn on start at login: {error}");
    }
    config::update(|c| c.autostart_set = true);
}

// ---------- Linux only: app menu and Files ----------

/// App menu entry (also gives the dock the right name). Windows and macOS get theirs from
/// the installer.
pub fn ensure_menu_entry() {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    let file = home().join(".local/share/applications/toolswitch.desktop");
    let entry = format!(
        "[Desktop Entry]\nName=toolswitch\nComment=Small everyday tools in one tiny app\nExec={}\nIcon={}\nTerminal=false\nType=Application\nCategories=Utility;\nStartupWMClass=toolswitch\n",
        command_line(&[]),
        icon_file().display()
    );
    if std::fs::read_to_string(&file).ok().as_deref() == Some(entry.as_str()) {
        return;
    }
    let _ = std::fs::create_dir_all(file.parent().unwrap());
    let _ = std::fs::write(file, entry);
}

/// Files (Nautilus) right-click menu: Scripts > "Compress with toolswitch" and the others.
pub fn ensure_nautilus_scripts() {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    for (name, tool) in [
        ("Compress with toolswitch", "compress"),
        ("Remove background with toolswitch", "cutout"),
        ("Sign PDF with toolswitch", "sign"),
        ("Audio to text with toolswitch", "transcribe"),
    ] {
        if let Err(error) = nautilus_script(name, tool) {
            eprintln!("Could not add \"{name}\" to the right-click menu: {error}");
        }
    }
}

#[cfg(unix)]
fn nautilus_script(name: &str, tool: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = home().join(".local/share/nautilus/scripts");
    let file = dir.join(name);
    let script = format!(
        "#!/bin/sh\n# Added by toolswitch.\nIFS='\n'\n[ -n \"$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\" ] && set -- $NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\nexec {} \"$@\"\n",
        command_line(&[tool])
    );
    if std::fs::read_to_string(&file).ok().as_deref() == Some(script.as_str()) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&file, &script)?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn nautilus_script(_name: &str, _tool: &str) -> std::io::Result<()> {
    Ok(())
}

// ---------- notifications ----------

/// A desktop notification. Best effort: if the system can't show it, nothing happens.
pub fn notify(title: &str, body: &str) {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut send = command("notify-send");
        send.arg("--app-name=toolswitch")
            .arg(format!("--icon={}", icon_file().display()))
            .args([title, body]);
        start(send);
    }
    #[cfg(windows)]
    {
        let (title, body) = (title.to_string(), body.to_string());
        // Showing it can take a moment on Windows; it doesn't hold up the caller.
        std::thread::spawn(move || {
            let _ = notify_rust::Notification::new()
                .appname("toolswitch")
                .summary(&title)
                .body(&body)
                .show();
        });
    }
    #[cfg(target_os = "macos")]
    {
        let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut script = command("osascript");
        script
            .args([
                "-e",
                &format!(
                    "display notification \"{}\" with title \"{}\"",
                    quote(body),
                    quote(title)
                ),
            ]);
        start(script);
    }
}
