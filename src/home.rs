// The Tools screen: every tool with its on/off switch and its shortcuts. A tool's name opens
// its screen.

use crate::shortcut_editor::ShortcutEditor;
use crate::{Screen, config, shortcuts, theme};
use eframe::egui;

struct ToolInfo {
    id: &'static str,
    icon: &'static str,
    name: &'static str,
    description: &'static str,
    screen: Option<Screen>,
    /// (shortcut id, what it does)
    shortcuts: &'static [(&'static str, &'static str)],
}

const GROUPS: &[(&str, &[ToolInfo])] = &[
    (
        "Everyday",
        &[
            ToolInfo {
                id: "clipboard",
                icon: "📋",
                name: "Clipboard history",
                description: "Keeps what you copy. Open it with the shortcut and pick an old text.",
                screen: Some(Screen::Clipboard),
                shortcuts: &[("clipboard", "Open")],
            },
            ToolInfo {
                id: "ocr",
                icon: "🔍",
                name: "Text from screen",
                description: "Select part of the screen and copy the text in it.",
                screen: None,
                shortcuts: &[("ocr", "Select")],
            },
            ToolInfo {
                id: "wallpaper",
                icon: "🖼",
                name: "Wallpapers",
                description: "Pick your wallpapers and switch them with a shortcut, or by themselves.",
                screen: Some(Screen::Wallpapers),
                shortcuts: &[
                    ("wallpaper", "Open"),
                    ("wallpaper-random", "Random"),
                    ("wallpaper-next", "Next"),
                    ("wallpaper-prev", "Previous"),
                ],
            },
            ToolInfo {
                id: "reminders",
                icon: "⏰",
                name: "Reminders",
                description: "\"In 20 min call Juan\": a notification at that time. No calendar, no account.",
                screen: Some(Screen::Reminders),
                shortcuts: &[("reminders", "New reminder")],
            },
            ToolInfo {
                id: "breaks",
                icon: "☕",
                name: "Breaks",
                description: "Reminds you to rest your eyes or stand up. Pomodoro too.",
                screen: Some(Screen::Breaks),
                shortcuts: &[("breaks", "Open")],
            },
        ],
    ),
    (
        "Files",
        &[
            ToolInfo {
                id: "compress",
                icon: "📦",
                name: "Compress files",
                description: "Makes images, PDFs and Office files smaller. Also from the right-click menu in Files.",
                screen: Some(Screen::Compress),
                shortcuts: &[("compress", "Open")],
            },
            ToolInfo {
                id: "cutout",
                icon: "✂",
                name: "Remove background",
                description: "Cuts the background out of photos, offline. Also from the right-click menu in Files.",
                screen: Some(Screen::Cutout),
                shortcuts: &[("cutout", "Open")],
            },
            ToolInfo {
                id: "sign",
                icon: "✒",
                name: "Sign PDF",
                description: "Puts your signature on PDFs without printing them. Also from the right-click menu.",
                screen: Some(Screen::Sign),
                shortcuts: &[("sign", "Open")],
            },
            ToolInfo {
                id: "organize",
                icon: "📂",
                name: "Tidy downloads",
                description: "Moves what you download to its folder by type, by itself. Every move can be undone.",
                screen: Some(Screen::Organize),
                shortcuts: &[("organize", "Open")],
            },
            ToolInfo {
                id: "backup",
                icon: "💾",
                name: "Backup",
                description: "Copies your folders to another disk every day, by itself.",
                screen: Some(Screen::Backup),
                shortcuts: &[("backup", "Open")],
            },
        ],
    ),
    (
        "System",
        &[
            ToolInfo {
                id: "cleanup",
                icon: "♻",
                name: "Clean up",
                description: "Shows what takes space (trash, caches, old downloads, big files) and frees it.",
                screen: Some(Screen::Cleanup),
                shortcuts: &[("cleanup", "Open")],
            },
            ToolInfo {
                id: "monitor",
                icon: "📈",
                name: "Watch the PC",
                description: "Warns when a disk is almost full or a program gets stuck eating the processor or memory.",
                screen: Some(Screen::Monitor),
                shortcuts: &[("monitor", "Open")],
            },
            ToolInfo {
                id: "power",
                icon: "🔌",
                name: "Shut down timer",
                description: "Shut down, restart or sleep later, at a time, or when the downloads finish.",
                screen: Some(Screen::Power),
                shortcuts: &[("power", "Open")],
            },
            ToolInfo {
                id: "updates",
                icon: "⟳",
                name: "Updates",
                description: "Updates the system and your apps with one click, and tells you when there's something new.",
                screen: Some(Screen::Updates),
                shortcuts: &[("updates", "Open")],
            },
        ],
    ),
];

/// A small card for the welcome screen. Returns true if its switch changed.
fn tile(ui: &mut egui::Ui, tool: &ToolInfo) -> bool {
    let mut changed = false;
    theme::card()
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(54.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tool.icon).size(18.0));
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - 50.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(egui::RichText::new(tool.name).strong());
                    ui.label(egui::RichText::new(tool.description).size(12.0).color(theme::MUTED));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ALWAYS_ON.contains(&tool.id) {
                        return;
                    }
                    let mut on = config::enabled(tool.id);
                    if theme::toggle(ui, &mut on).changed() {
                        config::set_enabled(tool.id, on);
                        shortcuts::register_all();
                        changed = true;
                    }
                });
            });
        });
    changed
}

/// Tools without a switch: they only do something when you use them.
const ALWAYS_ON: &[&str] = &["cleanup", "power", "sign"];

#[derive(Default)]
pub struct Home {
    pub shortcuts: ShortcutEditor,
}

/// What the user did on this screen.
#[derive(Default)]
pub struct HomeAction {
    /// A setting changed: the tray menu needs rebuilding.
    pub changed: bool,
    pub open: Option<Screen>,
}

impl Home {
    pub fn ui(&mut self, ui: &mut egui::Ui) -> HomeAction {
        let mut action = HomeAction::default();
        self.shortcuts.poll(ui.ctx());
        theme::page_header(
            ui,
            "Tools",
            "Turn on the tools you want and use them from anywhere with their shortcut.",
        );
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (group, tools) in GROUPS {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(*group).size(13.0).color(theme::FAINT));
                    ui.add_space(4.0);
                    for tool in *tools {
                        self.card(ui, tool, &mut action);
                        ui.add_space(8.0);
                    }
                }
            });
        action
    }

    /// The first-time screen: what toolswitch is, every tool with its switch, and Start.
    /// Returns (Start was clicked, a tool was turned on or off).
    pub fn welcome(&mut self, ui: &mut egui::Ui) -> (bool, bool) {
        let mut done = false;
        let mut changed = false;
        ui.horizontal(|ui| {
            let logo = theme::logo(ui.ctx());
            ui.add(egui::Image::new(&logo).fit_to_exact_size(egui::vec2(30.0, 30.0)));
            ui.label(egui::RichText::new("Welcome to toolswitch").size(26.0).strong());
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Small everyday tools in one app that waits in the tray. Turn on the ones you want; you can change it any time in Tools.",
            )
            .color(theme::MUTED),
        );
        ui.add_space(16.0);
        let footer = 56.0;
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - footer)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Room for the scroll bar on the right.
                ui.set_max_width(ui.available_width() - 12.0);
                for (group, tools) in GROUPS {
                    ui.label(egui::RichText::new(*group).size(13.0).color(theme::FAINT));
                    ui.add_space(4.0);
                    // Two tiles per row.
                    for pair in tools.chunks(2) {
                        ui.columns(2, |cols| {
                            for (col, tool) in cols.iter_mut().zip(pair) {
                                changed |= tile(col, tool);
                            }
                        });
                        ui.add_space(6.0);
                    }
                    ui.add_space(8.0);
                }
            });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let mut on = crate::desktop::autostart_enabled();
            if theme::toggle(ui, &mut on).changed()
                && let Err(error) = crate::desktop::set_autostart(on)
            {
                eprintln!("Could not change start at login: {error}");
            }
            ui.label(egui::RichText::new("Start at login").color(theme::MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let start = egui::Button::new(
                    egui::RichText::new("  Start  ").size(15.0).color(egui::Color32::WHITE),
                )
                .fill(theme::ACCENT);
                if ui.add(start).clicked() {
                    done = true;
                }
            });
        });
        (done, changed)
    }

    fn card(&mut self, ui: &mut egui::Ui, tool: &ToolInfo, action: &mut HomeAction) {
        let mut on = config::enabled(tool.id);
        theme::card()
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tool.icon).size(20.0));
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(ui.available_width() - 60.0);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let name = ui
                            .horizontal(|ui| {
                                let name = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(tool.name).size(15.0).strong(),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                // A hint that the name leads to the tool's screen.
                                if tool.screen.is_some() {
                                    let color = if name.hovered() { theme::ACCENT } else { theme::FAINT };
                                    ui.label(egui::RichText::new("›").size(16.0).color(color));
                                }
                                name
                            })
                            .inner;
                        if let Some(screen) = tool.screen
                            && name
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("Open")
                                .clicked()
                        {
                            action.open = Some(screen);
                        }
                        ui.label(
                            egui::RichText::new(tool.description)
                                .size(13.0)
                                .color(theme::MUTED),
                        );
                    });
                    if !ALWAYS_ON.contains(&tool.id) {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::toggle(ui, &mut on).changed() {
                                config::set_enabled(tool.id, on);
                                shortcuts::register_all();
                                action.changed = true;
                            }
                        });
                    }
                });
                if on && !tool.shortcuts.is_empty() {
                    ui.add_space(6.0);
                    ui.separator();
                    for (id, what) in tool.shortcuts {
                        self.shortcuts.row(ui, id, what);
                    }
                }
            });
    }
}
