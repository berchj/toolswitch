// Watch the PC: warns when a disk is almost full, memory runs out, or a program keeps the
// processor busy for minutes (often a stuck one). The screen shows what uses the most and
// closes a program with a click.

use crate::{desktop, theme};
use eframe::egui;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use sysinfo::{Disks, Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// How often the background thread looks.
const EVERY: Duration = Duration::from_secs(60);
/// Busy for this many looks in a row (minutes) means stuck.
const BUSY_LOOKS: u32 = 3;
/// Share of the whole processor (all cores) that counts as busy.
const BUSY_SHARE: f32 = 0.5;
/// A program using more than this share of the memory.
const BIG_MEMORY: f64 = 0.4;
/// A disk with less than this free, and under 10%.
const LOW_DISK: u64 = 5 << 30;
const WARN_AGAIN: Duration = Duration::from_secs(3600);

pub struct Proc {
    pub pid: Pid,
    pub name: String,
    /// Share of the whole processor, 0..1.
    pub cpu: f32,
    pub memory: u64,
    /// Ours and not part of the desktop itself: closing it can't end the session.
    pub can_end: bool,
}

/// Programs the desktop needs: closing one would end the session or blank the screen.
const ESSENTIAL: &[&str] = &[
    "gnome-shell", "xorg", "xwayland", "systemd", "gdm", "gdm-x-session", "gnome-session",
    "plasmashell", "kwin_x11", "kwin_wayland", "xfwm4", "xfce4-session", "cinnamon",
    "mutter", "dbus-daemon", "pipewire", "pulseaudio", "explorer.exe", "dwm.exe",
    "csrss.exe", "winlogon.exe", "svchost.exe", "windowserver", "finder", "dock",
    "loginwindow", "toolswitch", "toolswitch.exe",
];

pub struct Disk {
    pub mount: String,
    pub total: u64,
    pub free: u64,
}

#[derive(Default)]
pub struct Snapshot {
    pub cpu: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    pub procs: Vec<Proc>,
    pub disks: Vec<Disk>,
}

struct Watch {
    system: System,
    disks: Disks,
    /// The last refresh (the screen refreshes every 2 s while it's open).
    last: Option<Instant>,
    /// The last time the warnings were checked.
    checked: Option<Instant>,
    busy: HashMap<Pid, u32>,
    warned: HashMap<String, Instant>,
    snapshot: Snapshot,
}

fn watch() -> MutexGuard<'static, Watch> {
    static WATCH: OnceLock<Mutex<Watch>> = OnceLock::new();
    WATCH
        .get_or_init(|| {
            Mutex::new(Watch {
                system: System::new(),
                disks: Disks::new(),
                last: None,
                checked: None,
                busy: HashMap::new(),
                warned: HashMap::new(),
                snapshot: Snapshot::default(),
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

impl Watch {
    fn refresh(&mut self) {
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_user(UpdateKind::OnlyIfNotSet),
        );
        self.disks.refresh(true);
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
        let me = sysinfo::get_current_pid()
            .ok()
            .and_then(|pid| self.system.process(pid))
            .and_then(|p| p.user_id().cloned());
        let procs: Vec<Proc> = self
            .system
            .processes()
            .values()
            // On Linux threads show up as processes too; only whole programs count.
            .filter(|p| p.thread_kind().is_none())
            .map(|p| {
                // The program's file says more than the process name ("node-MainThread").
                let name = p
                    .exe()
                    .and_then(|e| e.file_name())
                    // Some programs' file is named after their version ("2.1.309").
                    .filter(|f| f.to_string_lossy().chars().any(char::is_alphabetic))
                    .unwrap_or(p.name())
                    .to_string_lossy()
                    .into_owned();
                let ours = me.is_some() && p.user_id() == me.as_ref();
                let essential = ESSENTIAL.contains(&name.to_lowercase().as_str());
                Proc {
                    pid: p.pid(),
                    can_end: ours && !essential,
                    name,
                    cpu: p.cpu_usage() / 100.0 / cores,
                    memory: p.memory(),
                }
            })
            .collect();
        let disks = self
            .disks
            .iter()
            .filter(|d| d.total_space() > 1 << 30)
            .map(|d| Disk {
                mount: d.mount_point().to_string_lossy().into_owned(),
                total: d.total_space(),
                free: d.available_space(),
            })
            // Snaps and the like mount many small read-only disks.
            .filter(|d| !d.mount.starts_with("/snap") && !d.mount.starts_with("/boot"))
            .collect();
        self.snapshot = Snapshot {
            cpu: procs.iter().map(|p| p.cpu).sum::<f32>().min(1.0),
            memory_used: self.system.used_memory(),
            memory_total: self.system.total_memory(),
            procs,
            disks,
        };
        self.last = Some(Instant::now());
    }

    fn warn(&mut self, key: String, title: &str, body: &str) {
        if self
            .warned
            .get(&key)
            .is_some_and(|at| at.elapsed() < WARN_AGAIN)
        {
            return;
        }
        self.warned.insert(key, Instant::now());
        desktop::notify(title, body);
    }
}

pub fn tick() -> bool {
    let mut w = watch();
    if w.checked.is_some_and(|at| at.elapsed() < EVERY) {
        return false;
    }
    w.checked = Some(Instant::now());
    let first = w.last.is_none();
    w.refresh();
    // The first look has no processor numbers yet (they're measured between two looks).
    if first {
        return false;
    }

    let busy: Vec<(Pid, String, f32)> = w
        .snapshot
        .procs
        .iter()
        .filter(|p| p.cpu >= BUSY_SHARE)
        .map(|p| (p.pid, p.name.clone(), p.cpu))
        .collect();
    let mut still_busy = HashMap::new();
    for (pid, name, cpu) in busy {
        let looks = w.busy.get(&pid).copied().unwrap_or(0) + 1;
        still_busy.insert(pid, looks);
        if looks >= BUSY_LOOKS {
            w.warn(
                format!("cpu-{pid}"),
                &format!("📈 {name} keeps the processor busy"),
                &format!(
                    "It's been using {:.0}% for {looks} minutes. If it's stuck, close it from toolswitch › Watch the PC.",
                    cpu * 100.0
                ),
            );
        }
    }
    w.busy = still_busy;

    let total = w.snapshot.memory_total.max(1) as f64;
    let hog = w
        .snapshot
        .procs
        .iter()
        .max_by_key(|p| p.memory)
        .filter(|p| p.memory as f64 / total > BIG_MEMORY)
        .map(|p| (p.pid, p.name.clone(), p.memory));
    if let Some((pid, name, memory)) = hog {
        w.warn(
            format!("mem-{pid}"),
            &format!("📈 {name} is using a lot of memory"),
            &format!(
                "{} of {}. Close it from toolswitch › Watch the PC if it's not needed.",
                size(memory),
                size(total as u64)
            ),
        );
    }

    let full: Vec<(String, u64)> = w
        .snapshot
        .disks
        .iter()
        .filter(|d| d.free < LOW_DISK && (d.free as f64) < d.total as f64 * 0.1)
        .map(|d| (d.mount.clone(), d.free))
        .collect();
    for (mount, free) in full {
        w.warn(
            format!("disk-{mount}"),
            &format!("💽 The disk {mount} is almost full"),
            &format!(
                "Only {} free. toolswitch › Clean up shows what takes space.",
                size(free)
            ),
        );
    }
    true
}

pub fn size(bytes: u64) -> String {
    crate::compress::format_size(bytes)
}

/// Closes a program. Returns false if the system didn't let it (another user's, for example).
fn end(pid: Pid) -> bool {
    watch().system.process(pid).is_some_and(|p| p.kill())
}

#[derive(Default)]
pub struct MonitorPage {
    /// The program whose "End" was clicked once: a second click closes it.
    confirm: Option<Pid>,
    error: Option<String>,
}

impl crate::Page for MonitorPage {
    fn ui(&mut self, ui: &mut egui::Ui) {
        // While the screen is open the numbers move every two seconds.
        {
            let mut w = watch();
            if w.last
                .is_none_or(|at| at.elapsed() > Duration::from_secs(2))
            {
                w.refresh();
            }
        }
        ui.ctx().request_repaint_after(Duration::from_secs(2));
        theme::page_header_with_switch(
            ui,
            "Watch the PC",
            "Warns you when a disk is almost full or a program gets stuck using the processor or the memory.",
            "monitor",
        );
        ui.add_space(16.0);

        let w = watch();
        let snap = &w.snapshot;
        ui.columns(2, |cols| {
            gauge(
                &mut cols[0],
                "Processor",
                snap.cpu,
                &format!("{:.0}%", snap.cpu * 100.0),
            );
            let share = snap.memory_used as f32 / snap.memory_total.max(1) as f32;
            gauge(
                &mut cols[1],
                "Memory",
                share,
                &format!("{} of {}", size(snap.memory_used), size(snap.memory_total)),
            );
        });
        ui.add_space(12.0);

        let mut by_cpu: Vec<&Proc> = snap.procs.iter().collect();
        by_cpu.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        let mut by_memory: Vec<&Proc> = snap.procs.iter().collect();
        by_memory.sort_by_key(|p| std::cmp::Reverse(p.memory));
        let mut end_pid = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.columns(2, |cols| {
                    for (col, title, list) in [
                        (0, "Using the processor", &by_cpu),
                        (1, "Using the memory", &by_memory),
                    ] {
                        let ui = &mut cols[col];
                        theme::card().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(egui::RichText::new(title).strong());
                            ui.add_space(4.0);
                            for p in list.iter().take(6) {
                                ui.horizontal(|ui| {
                                    ui.set_min_height(26.0);
                                    ui.add(egui::Label::new(&p.name).truncate());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let sure = self.confirm == Some(p.pid);
                                            let label = if sure { "Sure?" } else { "End" };
                                            let button = egui::Button::new(
                                                egui::RichText::new(label).size(12.0),
                                            );
                                            // Not offered for the desktop's own programs.
                                            if !p.can_end {
                                                ui.add_space(44.0);
                                            } else if ui.add(button).clicked() {
                                                if sure {
                                                    end_pid = Some(p.pid);
                                                } else {
                                                    self.confirm = Some(p.pid);
                                                }
                                            }
                                            let value = if col == 0 {
                                                format!("{:.0}%", p.cpu * 100.0)
                                            } else {
                                                size(p.memory)
                                            };
                                            ui.label(
                                                egui::RichText::new(value)
                                                    .size(13.0)
                                                    .color(theme::MUTED),
                                            );
                                        },
                                    );
                                });
                            }
                        });
                    }
                });
                if let Some(error) = &self.error {
                    ui.label(egui::RichText::new(error).color(theme::WARNING));
                }
                ui.add_space(12.0);
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("Disks").strong());
                    ui.add_space(4.0);
                    for d in &snap.disks {
                        let used = 1.0 - d.free as f32 / d.total.max(1) as f32;
                        bar(
                            ui,
                            &d.mount,
                            used,
                            &format!("{} free of {}", size(d.free), size(d.total)),
                        );
                    }
                });
            });
        drop(w);
        if let Some(pid) = end_pid {
            self.confirm = None;
            self.error =
                (!end(pid)).then(|| "The system didn't let toolswitch close it.".to_string());
        }
    }
}

fn gauge(ui: &mut egui::Ui, title: &str, share: f32, text: &str) {
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(title).color(theme::MUTED));
        ui.label(egui::RichText::new(text).size(20.0).strong());
        ui.add_space(4.0);
        meter(ui, share);
    });
}

fn bar(ui: &mut egui::Ui, name: &str, share: f32, text: &str) {
    ui.horizontal(|ui| {
        ui.label(name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(text).size(13.0).color(theme::MUTED));
        });
    });
    meter(ui, share);
    ui.add_space(6.0);
}

/// A thin bar: blue, amber past 75%, red past 90%.
fn meter(ui: &mut egui::Ui, share: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 3.0, theme::BORDER);
    let mut fill = rect;
    fill.set_width(rect.width() * share.clamp(0.0, 1.0));
    let color = if share > 0.9 {
        egui::Color32::from_rgb(0xef, 0x44, 0x44)
    } else if share > 0.75 {
        theme::WARNING
    } else {
        theme::ACCENT
    };
    ui.painter().rect_filled(fill, 3.0, color);
}
