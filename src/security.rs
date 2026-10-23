// Security check: looks at how this computer is set up (firewall, open ports, updates, screen
// lock, disk encryption, SSH, files with secrets...) and says what's risky and how to fix it.
// The checks run here, offline. With an Anthropic API key, "Ask Claude" sends the results (no
// files, no personal data: just the settings found) to Claude for a plan made for this computer.
// Nothing is changed by itself: each fix is a command to copy or a setting to change.

use crate::{config, desktop, theme};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{Receiver, Sender, channel};

const FILE: &str = "security.json";
const API: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-opus-5-5";
const KEYS_PAGE: &str = "https://console.anthropic.com/settings/keys";

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    api_key: String,
}

/// The settings hold the API key: only this user may read the file.
fn save(saved: &Saved) {
    config::write(FILE, saved);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            config::dir().join(FILE),
            std::fs::Permissions::from_mode(0o600),
        );
    }
}

#[derive(Clone, Copy, PartialEq, PartialOrd, Serialize)]
enum Level {
    High,
    Medium,
    Low,
    Ok,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::High => "High",
            Level::Medium => "Medium",
            Level::Low => "Low",
            Level::Ok => "OK",
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            Level::High => egui::Color32::from_rgb(0xef, 0x44, 0x44),
            Level::Medium => theme::WARNING,
            Level::Low => theme::MUTED,
            Level::Ok => theme::GREEN,
        }
    }
}

#[derive(Clone, Serialize)]
struct Finding {
    level: Level,
    title: String,
    /// What was found, in plain words.
    detail: String,
    /// How to fix it.
    fix: String,
    /// A command that fixes it, to copy and run in a terminal.
    command: Option<String>,
}

fn finding(level: Level, title: &str, detail: impl Into<String>, fix: &str) -> Finding {
    Finding {
        level,
        title: title.into(),
        detail: detail.into(),
        fix: fix.into(),
        command: None,
    }
}

impl Finding {
    fn command(mut self, command: &str) -> Self {
        self.command = Some(command.into());
        self
    }
}

/// A program's output, or None if it isn't there or failed to start.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = desktop::command(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// The system's name and version, as shown to Claude.
fn system_name() -> String {
    if cfg!(windows) {
        run("cmd", &["/C", "ver"]).map_or("Windows".into(), |v| v.trim().to_string())
    } else if cfg!(target_os = "macos") {
        let v = run("sw_vers", &["-productVersion"]).unwrap_or_default();
        format!("macOS {}", v.trim())
    } else {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Linux".into())
    }
}

/// Every check for this system, worst first.
fn scan() -> Vec<Finding> {
    let mut found = if cfg!(windows) {
        windows_checks()
    } else if cfg!(target_os = "macos") {
        mac_checks()
    } else {
        linux_checks()
    };
    found.extend(common_checks());
    found.sort_by(|a, b| a.level.partial_cmp(&b.level).unwrap_or(std::cmp::Ordering::Equal));
    found
}

// ---------- every system ----------

/// Files that hold passwords or keys must not be readable by other users.
fn common_checks() -> Vec<Finding> {
    #[allow(unused_mut)]
    let mut found = vec![];
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let home = home();
        let secrets = [
            ".ssh/id_rsa",
            ".ssh/id_ed25519",
            ".ssh/id_ecdsa",
            ".aws/credentials",
            ".netrc",
            ".pgpass",
            ".docker/config.json",
            ".git-credentials",
            ".config/toolswitch/jira.json",
            ".config/toolswitch/calendar.json",
        ];
        let mut open = vec![];
        for name in secrets {
            if let Ok(meta) = std::fs::metadata(home.join(name))
                && meta.permissions().mode() & 0o077 != 0
            {
                open.push(name);
            }
        }
        if open.is_empty() {
            found.push(finding(
                Level::Ok,
                "Files with passwords and keys",
                "The ones found are readable only by you.",
                "",
            ));
        } else {
            let list = open.iter().map(|n| format!("~/{n}")).collect::<Vec<_>>().join(", ");
            let command = open
                .iter()
                .map(|n| format!("chmod 600 ~/{n}"))
                .collect::<Vec<_>>()
                .join(" && ");
            found.push(
                finding(
                    Level::High,
                    "Files with passwords or keys readable by others",
                    format!("Other users of this computer can read: {list}."),
                    "Make them readable only by you.",
                )
                .command(&command),
            );
        }
        if let Ok(meta) = std::fs::metadata(home.join(".ssh"))
            && meta.permissions().mode() & 0o077 != 0
        {
            found.push(
                finding(
                    Level::Medium,
                    "The .ssh folder is open to others",
                    "SSH refuses keys in a folder others can read, and it exposes which servers you use.",
                    "Make the folder yours only.",
                )
                .command("chmod 700 ~/.ssh"),
            );
        }
    }
    found
}

// ---------- Linux ----------

fn linux_checks() -> Vec<Finding> {
    let mut found = vec![];

    // Firewall: ufw or firewalld.
    let ufw_on = std::fs::read_to_string("/etc/ufw/ufw.conf")
        .is_ok_and(|s| s.lines().any(|l| l.trim() == "ENABLED=yes"));
    let firewalld_on = run("systemctl", &["is-active", "firewalld"])
        .is_some_and(|s| s.trim() == "active");
    if ufw_on || firewalld_on {
        found.push(finding(Level::Ok, "Firewall", "The firewall is on.", ""));
    } else {
        let mut f = finding(
            Level::Medium,
            "The firewall is off",
            "Any program that opens a port can be reached from the network you're on (a café, an office).",
            "Turn on the firewall: it blocks incoming connections and lets yours out.",
        );
        if Path::new("/usr/sbin/ufw").exists() {
            f = f.command("sudo ufw enable");
        }
        found.push(f);
    }

    // Programs listening to the network (not just this computer).
    if let Some(out) = run("ss", &["-tulnH"]) {
        let mut ports: Vec<(String, u16)> = vec![];
        for line in out.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (Some(proto), Some(local)) = (cols.first(), cols.get(4)) else {
                continue;
            };
            let Some((addr, port)) = local.rsplit_once(':') else {
                continue;
            };
            let Ok(port) = port.parse::<u16>() else { continue };
            let local_only = addr.starts_with("127.")
                || addr == "[::1]"
                || addr.starts_with("[::ffff:127.")
                || addr.contains("%lo");
            // DHCP and mDNS (printers, phones) listen everywhere on every computer.
            if !local_only && ![68, 5353, 546].contains(&port) && !ports.iter().any(|p| p.1 == port)
            {
                ports.push((proto.to_string(), port));
            }
        }
        if ports.is_empty() {
            found.push(finding(
                Level::Ok,
                "Open ports",
                "No program accepts connections from other computers.",
                "",
            ));
        } else {
            let risky = |p: u16| {
                [21, 23, 445, 139, 3306, 5432, 6379, 27017, 9200, 11211, 5900, 3389].contains(&p)
            };
            let list = ports
                .iter()
                .map(|(proto, port)| format!("{port}/{proto} ({})", port_name(*port)))
                .collect::<Vec<_>>()
                .join(", ");
            let level = if ports.iter().any(|p| risky(p.1)) && !(ufw_on || firewalld_on) {
                Level::High
            } else {
                Level::Low
            };
            found.push(finding(
                level,
                "Programs reachable from the network",
                format!("Listening to other computers: {list}."),
                "If you don't use them from other computers, make them listen only on 127.0.0.1 (in each program's settings) or stop them, and keep the firewall on.",
            ));
        }
    }

    // Pending updates, security ones apart.
    if Path::new("/usr/bin/apt").exists()
        && let Some(out) = run("apt", &["list", "--upgradable"])
    {
        let lines: Vec<&str> = out.lines().filter(|l| l.contains('/')).collect();
        let security = lines.iter().filter(|l| l.contains("-security")).count();
        if security > 0 {
            found.push(
                finding(
                    Level::High,
                    "Security updates waiting",
                    format!("{security} security fixes (of {} updates) aren't installed.", lines.len()),
                    "Install them now, or from toolswitch › Updates.",
                )
                .command("sudo apt update && sudo apt upgrade"),
            );
        } else if !lines.is_empty() {
            found.push(
                finding(
                    Level::Low,
                    "Updates waiting",
                    format!("{} updates aren't installed (none marked as security).", lines.len()),
                    "Install them when you can.",
                )
                .command("sudo apt update && sudo apt upgrade"),
            );
        } else {
            found.push(finding(Level::Ok, "Updates", "Everything is up to date.", ""));
        }
        let auto = std::fs::read_to_string("/etc/apt/apt.conf.d/20auto-upgrades")
            .is_ok_and(|s| s.contains("Unattended-Upgrade \"1\""));
        if !auto {
            found.push(
                finding(
                    Level::Medium,
                    "Security updates don't install by themselves",
                    "Fixes for known holes wait until someone installs them by hand.",
                    "Turn on automatic security updates.",
                )
                .command("sudo apt install unattended-upgrades && sudo dpkg-reconfigure -plow unattended-upgrades"),
            );
        }
    }
    if Path::new("/var/run/reboot-required").exists() {
        found.push(finding(
            Level::Medium,
            "Restart pending",
            "Updates were installed that only protect you after restarting (often the kernel).",
            "Restart the computer when you can.",
        ));
    }

    // Screen lock (GNOME).
    let gsettings = |schema: &str, key: &str| run("gsettings", &["get", schema, key]);
    if let Some(lock) = gsettings("org.gnome.desktop.screensaver", "lock-enabled") {
        let idle = gsettings("org.gnome.desktop.session", "idle-delay")
            .and_then(|s| s.split_whitespace().last().and_then(|n| n.parse::<u32>().ok()))
            .unwrap_or(0);
        if lock.trim() != "true" {
            found.push(
                finding(
                    Level::Medium,
                    "The screen doesn't lock",
                    "Anyone who sits at the computer while you're away gets in.",
                    "Turn on the automatic screen lock.",
                )
                .command("gsettings set org.gnome.desktop.screensaver lock-enabled true"),
            );
        } else if idle == 0 || idle > 900 {
            found.push(
                finding(
                    Level::Low,
                    "The screen takes long to lock",
                    if idle == 0 {
                        "The screen never goes blank by itself, so it never locks.".to_string()
                    } else {
                        format!("It locks after {} minutes without use.", idle / 60)
                    },
                    "Lock it after 5 minutes or less.",
                )
                .command("gsettings set org.gnome.desktop.session idle-delay 300"),
            );
        } else {
            found.push(finding(Level::Ok, "Screen lock", "The screen locks by itself.", ""));
        }
    }
    if std::fs::read_to_string("/etc/gdm3/custom.conf")
        .is_ok_and(|s| s.lines().any(|l| l.trim().eq_ignore_ascii_case("AutomaticLoginEnable=true")))
    {
        found.push(finding(
            Level::Medium,
            "Automatic login is on",
            "Turning the computer on opens your session without a password.",
            "Turn off automatic login in Settings › Users.",
        ));
    }

    // Disk encryption.
    if let Some(out) = run("lsblk", &["-rno", "TYPE,FSTYPE"]) {
        if out.lines().any(|l| l.starts_with("crypt") || l.contains("crypto_LUKS")) {
            found.push(finding(Level::Ok, "Disk encryption", "The disk is encrypted.", ""));
        } else {
            found.push(finding(
                Level::Medium,
                "The disk isn't encrypted",
                "If the computer is lost or stolen, anyone can read your files by taking the disk out.",
                "Encryption is chosen when installing the system (\"Encrypt the new installation\"). On a laptop, plan it for the next reinstall, with a backup first.",
            ));
        }
    }

    // SSH server.
    let sshd_running = run("systemctl", &["is-active", "ssh"]).is_some_and(|s| s.trim() == "active")
        || run("systemctl", &["is-active", "sshd"]).is_some_and(|s| s.trim() == "active");
    if sshd_running && let Ok(conf) = std::fs::read_to_string("/etc/ssh/sshd_config") {
        let setting = |key: &str| {
            conf.lines()
                .map(str::trim)
                .filter(|l| !l.starts_with('#'))
                .find_map(|l| {
                    let (k, v) = l.split_once(char::is_whitespace)?;
                    k.eq_ignore_ascii_case(key).then(|| v.trim().to_lowercase())
                })
        };
        if setting("PermitRootLogin").as_deref() == Some("yes") {
            found.push(finding(
                Level::High,
                "SSH lets root log in",
                "Attackers try root's password all day long.",
                "Set PermitRootLogin no in /etc/ssh/sshd_config and restart ssh.",
            ));
        }
        if setting("PasswordAuthentication").as_deref() != Some("no") {
            found.push(finding(
                Level::Medium,
                "SSH accepts passwords",
                "An SSH server is running and takes passwords, which can be guessed.",
                "Use SSH keys and set PasswordAuthentication no in /etc/ssh/sshd_config. If you don't connect to this computer from others, turn the server off.",
            )
            .command("sudo systemctl disable --now ssh"));
        }
    }

    // Secure Boot.
    if let Some(out) = run("mokutil", &["--sb-state"]) {
        if out.contains("SecureBoot enabled") {
            found.push(finding(Level::Ok, "Secure Boot", "Secure Boot is on.", ""));
        } else if out.contains("SecureBoot disabled") {
            found.push(finding(
                Level::Low,
                "Secure Boot is off",
                "Secure Boot stops modified boot programs (bootkits) from starting before the system.",
                "Turn it on in the computer's firmware settings (UEFI), if your drivers allow it.",
            ));
        }
    }

    // Members of the docker group are root in practice.
    if run("id", &["-nG"]).is_some_and(|g| g.split_whitespace().any(|g| g == "docker")) {
        found.push(finding(
            Level::Low,
            "Your user is in the docker group",
            "Any program you run can use Docker to get administrator powers without a password.",
            "If you don't use Docker often, leave the group and use sudo docker instead.",
        ));
    }
    found
}

fn port_name(port: u16) -> &'static str {
    match port {
        21 => "FTP",
        22 => "SSH",
        23 => "Telnet",
        25 => "mail",
        53 => "DNS",
        80 | 8080 | 8000 | 3000 | 5000 => "web",
        443 | 8443 => "web, secure",
        139 | 445 => "Windows sharing",
        631 => "printing",
        3306 => "MySQL",
        3389 => "remote desktop",
        5432 => "PostgreSQL",
        5900 => "VNC remote screen",
        5672 => "RabbitMQ",
        15672 => "RabbitMQ admin",
        902 => "VMware",
        5678 => "n8n",
        6379 => "Redis",
        9200 => "Elasticsearch",
        11211 => "Memcached",
        27017 => "MongoDB",
        _ => "other",
    }
}

// ---------- Windows ----------

fn windows_checks() -> Vec<Finding> {
    let mut found = vec![];
    let ps = |script: &str| run("powershell", &["-NoProfile", "-NonInteractive", "-Command", script]);
    if let Some(out) = ps("(Get-NetFirewallProfile | Where-Object { -not $_.Enabled }).Name") {
        let off: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if off.is_empty() {
            found.push(finding(Level::Ok, "Firewall", "Windows Firewall is on.", ""));
        } else {
            found.push(finding(
                Level::High,
                "Windows Firewall is off",
                format!("Off for: {}.", off.join(", ")),
                "Turn it on in Windows Security › Firewall & network protection.",
            ));
        }
    }
    if let Some(out) = ps("$s = Get-MpComputerStatus; \"$($s.RealTimeProtectionEnabled) $($s.AntivirusSignatureAge)\"") {
        let mut parts = out.split_whitespace();
        let on = parts.next() == Some("True");
        let age: u32 = parts.next().and_then(|a| a.parse().ok()).unwrap_or(0);
        if !on {
            found.push(finding(
                Level::High,
                "Real-time virus protection is off",
                "Microsoft Defender isn't checking files as they open.",
                "Turn it on in Windows Security › Virus & threat protection.",
            ));
        } else if age > 7 {
            found.push(finding(
                Level::Medium,
                "Virus definitions are old",
                format!("Defender's definitions are {age} days old."),
                "Update them in Windows Security › Virus & threat protection › Protection updates.",
            ));
        } else {
            found.push(finding(Level::Ok, "Antivirus", "Microsoft Defender is on and up to date.", ""));
        }
    }
    if let Some(out) = run("reg", &["query", r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System", "/v", "EnableLUA"])
        && out.contains("0x0")
    {
        found.push(finding(
            Level::High,
            "User Account Control is off",
            "Programs get administrator powers without asking.",
            "Turn UAC back on in Control Panel › User Accounts › Change User Account Control settings.",
        ));
    }
    if let Some(out) = ps("(Get-BitLockerVolume -MountPoint $env:SystemDrive -ErrorAction SilentlyContinue).ProtectionStatus") {
        match out.trim() {
            "On" => found.push(finding(Level::Ok, "Disk encryption", "BitLocker protects the system disk.", "")),
            "Off" => found.push(finding(
                Level::Medium,
                "The disk isn't encrypted",
                "If the computer is lost or stolen, anyone can read your files.",
                "Turn on BitLocker (or Device encryption) in Settings › Privacy & security.",
            )),
            _ => {}
        }
    }
    found
}

// ---------- macOS ----------

fn mac_checks() -> Vec<Finding> {
    let mut found = vec![];
    if let Some(out) = run("fdesetup", &["status"]) {
        if out.contains("On") {
            found.push(finding(Level::Ok, "Disk encryption", "FileVault is on.", ""));
        } else {
            found.push(finding(
                Level::Medium,
                "FileVault is off",
                "If the Mac is lost or stolen, anyone can read your files.",
                "Turn on FileVault in System Settings › Privacy & Security.",
            ));
        }
    }
    if let Some(out) = run("/usr/libexec/ApplicationFirewall/socketfilterfw", &["--getglobalstate"]) {
        if out.contains("enabled") {
            found.push(finding(Level::Ok, "Firewall", "The firewall is on.", ""));
        } else {
            found.push(finding(
                Level::Medium,
                "The firewall is off",
                "Programs that accept connections can be reached from the network you're on.",
                "Turn it on in System Settings › Network › Firewall.",
            ));
        }
    }
    if let Some(out) = run("spctl", &["--status"])
        && out.contains("disabled")
    {
        found.push(finding(
            Level::High,
            "Gatekeeper is off",
            "Apps from anywhere open without checks.",
            "Turn it back on.",
        )
        .command("sudo spctl --master-enable"));
    }
    if let Some(out) = run("csrutil", &["status"])
        && out.contains("disabled")
    {
        found.push(finding(
            Level::High,
            "System Integrity Protection is off",
            "Malware can change system files.",
            "Restart into Recovery and run: csrutil enable",
        ));
    }
    found
}

// ---------- asking Claude ----------

/// What goes to Claude: the system and the findings, with this user's folder hidden.
fn report(system: &str, found: &[Finding]) -> String {
    let home = home().to_string_lossy().into_owned();
    let mut text = format!("System: {system}\n\nFindings from a local security check:\n");
    for f in found {
        text.push_str(&format!("- [{}] {}: {}", f.level.label(), f.title, f.detail));
        if let Some(c) = &f.command {
            text.push_str(&format!(" (suggested fix: `{c}`)"));
        }
        text.push('\n');
    }
    if home.len() > 1 {
        text = text.replace(&home, "~");
    }
    text
}

const PROMPT: &str = "You help a non-technical person secure their own personal computer. Below are the results of a local configuration check of that computer. Write, in the same language as the person's system if it's clear (else English): 1) a two-sentence verdict; 2) the fixes in order of importance, each with why it matters in one sentence and exact steps or commands for this system; 3) anything worth checking that the scan can't see (for example backups, password manager, two-factor login). Be concrete and brief, use Markdown headings and lists, don't invent findings that aren't listed, and say when something listed is fine as it is.";

/// Claude's plan for these findings. The key goes to curl through its standard input, so it
/// doesn't show up in the list of running programs.
fn ask_claude(key: &str, report: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": 16000,
        "output_config": {"effort": "medium"},
        // If a safety check declines it, the server retries on a fitting model.
        "fallbacks": "default",
        "system": PROMPT,
        "messages": [{"role": "user", "content": report}],
    });
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut curl = desktop::command("curl");
    curl.args(["-sS", "--max-time", "300", "-K", "-", "-w", "\n%{http_code}"])
        .arg(API)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = curl.spawn().map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "header = \"x-api-key: {}\"", quote(key));
        let _ = writeln!(stdin, "header = \"anthropic-version: 2026-10-21\"");
        let _ = writeln!(stdin, "header = \"anthropic-beta: server-side-fallback-2026-07-01\"");
        let _ = writeln!(stdin, "header = \"content-type: application/json\"");
        let _ = writeln!(stdin, "data = \"{}\"", quote(&body.to_string()));
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    match code.trim() {
        "200" => {}
        "000" | "" => return Err("Could not reach Anthropic. Are you online?".into()),
        "401" => return Err("Anthropic didn't accept the API key.".into()),
        "429" => return Err("Too many requests right now; try again in a minute.".into()),
        code => {
            let message = value["error"]["message"].as_str().unwrap_or_default();
            return Err(format!("Anthropic answered with error {code}. {message}"));
        }
    }
    if value["stop_reason"] == "refusal" {
        return Err("Claude declined to answer this one.".into());
    }
    let answer: Vec<&str> = value["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    if answer.is_empty() {
        return Err("Claude sent an empty answer.".into());
    }
    Ok(answer.join("\n"))
}

// ---------- the screen ----------

enum News {
    Scanned(String, Vec<Finding>),
    Answer(Result<String, String>),
}

pub struct SecurityPage {
    saved: Saved,
    key_input: String,
    system: String,
    found: Option<Vec<Finding>>,
    scanning: bool,
    asking: bool,
    answer: Option<Result<String, String>>,
    show_sent: bool,
    news: (Sender<News>, Receiver<News>),
}

impl Default for SecurityPage {
    fn default() -> Self {
        SecurityPage {
            saved: config::read(FILE),
            key_input: String::new(),
            system: String::new(),
            found: None,
            scanning: false,
            asking: false,
            answer: None,
            show_sent: false,
            news: channel(),
        }
    }
}

impl SecurityPage {
    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let (sender, ctx) = (self.news.0.clone(), ctx.clone());
        std::thread::spawn(move || {
            let found = std::panic::catch_unwind(scan).unwrap_or_default();
            let _ = sender.send(News::Scanned(system_name(), found));
            ctx.request_repaint();
        });
    }

    fn start_asking(&mut self, ctx: &egui::Context) {
        let Some(found) = &self.found else { return };
        self.asking = true;
        self.answer = None;
        let (sender, ctx) = (self.news.0.clone(), ctx.clone());
        let (key, report) = (self.saved.api_key.clone(), report(&self.system, found));
        std::thread::spawn(move || {
            let _ = sender.send(News::Answer(ask_claude(&key, &report)));
            ctx.request_repaint();
        });
    }

    fn finding_card(ui: &mut egui::Ui, f: &Finding) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(f.level.label()).strong().color(f.level.color()));
                ui.label(egui::RichText::new(&f.title).strong());
            });
            ui.label(egui::RichText::new(&f.detail).color(theme::MUTED));
            if !f.fix.is_empty() {
                ui.add_space(4.0);
                ui.label(&f.fix);
            }
            if let Some(command) = &f.command {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(command).monospace()).wrap());
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(command.clone());
                    }
                });
            }
        });
        ui.add_space(6.0);
    }
}

impl crate::Page for SecurityPage {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        while let Ok(news) = self.news.1.try_recv() {
            match news {
                News::Scanned(system, found) => {
                    self.system = system;
                    self.found = Some(found);
                    self.scanning = false;
                }
                News::Answer(answer) => {
                    self.answer = Some(answer);
                    self.asking = false;
                }
            }
        }
        if self.found.is_none() && !self.scanning {
            self.start_scan(&ctx);
        }

        theme::page_header(
            ui,
            "Security check",
            "Looks at how this computer is set up and says what's risky and how to fix it. Nothing changes by itself.",
        );
        ui.add_space(12.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // Claude.
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(egui::RichText::new("Ask Claude for a plan").strong());
                if self.saved.api_key.is_empty() {
                    ui.label(
                        egui::RichText::new("With an Anthropic API key, Claude explains the results and writes the steps for this computer. Only the list below is sent: no files, no names.")
                            .color(theme::MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.key_input)
                                .password(true)
                                .hint_text("API key (sk-ant-…)")
                                .desired_width(260.0),
                        );
                        if ui.add_enabled(!self.key_input.trim().is_empty(), egui::Button::new("Save")).clicked() {
                            self.saved.api_key = std::mem::take(&mut self.key_input).trim().to_string();
                            save(&self.saved);
                        }
                        if ui.button("🔗 Get a key").clicked() {
                            // The same program that shows a folder opens a web address.
                            theme::open_folder(Path::new(KEYS_PAGE));
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        let ask = egui::Button::new(
                            egui::RichText::new(if self.asking { "  Asking Claude…  " } else { "  Ask Claude  " })
                                .color(egui::Color32::WHITE),
                        )
                        .fill(theme::ACCENT);
                        if ui.add_enabled(!self.asking && self.found.is_some(), ask).clicked() {
                            self.start_asking(&ctx);
                        }
                        if self.asking {
                            ui.spinner();
                        }
                        ui.toggle_value(&mut self.show_sent, "What's sent");
                        if ui.button("Remove key").clicked() {
                            self.saved.api_key.clear();
                            save(&self.saved);
                        }
                    });
                    if self.show_sent && let Some(found) = &self.found {
                        ui.add(egui::Label::new(
                            egui::RichText::new(report(&self.system, found)).monospace().size(11.0).color(theme::MUTED),
                        ).wrap());
                    }
                }
                match &self.answer {
                    Some(Ok(text)) => {
                        ui.add_space(8.0);
                        markdown(ui, text);
                        if ui.small_button("Copy the plan").clicked() {
                            ctx.copy_text(text.clone());
                        }
                    }
                    Some(Err(error)) => {
                        ui.label(egui::RichText::new(error).color(theme::WARNING));
                    }
                    None => {}
                }
            });
            ui.add_space(12.0);

            // The checks.
            ui.horizontal(|ui| {
                match &self.found {
                    Some(found) => {
                        let count = |l| found.iter().filter(|f| f.level == l).count();
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {} high, {} medium, {} low, {} fine",
                                self.system,
                                count(Level::High),
                                count(Level::Medium),
                                count(Level::Low),
                                count(Level::Ok)
                            ))
                            .color(theme::MUTED),
                        );
                    }
                    None => {
                        ui.spinner();
                        ui.label(egui::RichText::new("Checking…").color(theme::MUTED));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!self.scanning, egui::Button::new("Check again")).clicked() {
                        self.answer = None;
                        self.start_scan(&ctx);
                    }
                });
            });
            ui.add_space(8.0);
            if let Some(found) = &self.found {
                for f in found {
                    Self::finding_card(ui, f);
                }
            }
        });
    }
}

/// Enough Markdown for Claude's answer: headings, lists, bold and code.
fn markdown(ui: &mut egui::Ui, text: &str) {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(h) = trimmed.strip_prefix("### ").or_else(|| trimmed.strip_prefix("## ")).or_else(|| trimmed.strip_prefix("# ")) {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(h.replace("**", "")).strong().size(15.0));
        } else if trimmed.starts_with("```") {
            continue;
        } else if trimmed.is_empty() {
            ui.add_space(4.0);
        } else {
            let indent = (line.len() - trimmed.len()) as f32 * 4.0;
            let (bullet, rest) = match trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
                Some(rest) => ("•  ", rest),
                None => ("", trimmed),
            };
            ui.horizontal_wrapped(|ui| {
                ui.add_space(indent);
                let mut job = egui::text::LayoutJob::default();
                let normal = egui::TextFormat { color: theme::TEXT, ..Default::default() };
                job.append(bullet, 0.0, normal.clone());
                // **bold** and `code`, in turns.
                for (i, part) in rest.split("**").enumerate() {
                    for (j, piece) in part.split('`').enumerate() {
                        let mut format = normal.clone();
                        if i % 2 == 1 {
                            format.color = egui::Color32::WHITE;
                            format.font_id = egui::FontId::proportional(14.0);
                        }
                        if j % 2 == 1 {
                            format.font_id = egui::FontId::monospace(13.0);
                            format.background = theme::CARD_HOVER;
                        }
                        job.append(piece, 0.0, format);
                    }
                }
                ui.label(job);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    /// Prints this computer's findings: cargo test --release security_scan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn security_scan() {
        let found = super::scan();
        println!("{}", super::report(&super::system_name(), &found));
    }

    /// Asks Claude about this computer: SECURITY_TEST_KEY=sk-ant-... cargo test --release
    /// security_ask -- --ignored --nocapture
    #[test]
    #[ignore]
    fn security_ask() {
        let key = std::env::var("SECURITY_TEST_KEY").expect("set SECURITY_TEST_KEY");
        let report = super::report(&super::system_name(), &super::scan());
        match super::ask_claude(&key, &report) {
            Ok(plan) => println!("{plan}"),
            Err(error) => println!("error: {error}"),
        }
    }
}
