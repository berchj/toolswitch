#![cfg_attr(windows, windows_subsystem = "windows")]
// toolswitch: small everyday tools in one app that lives in the tray.
//
//   toolswitch             open the window
//   toolswitch --hidden    start in the tray only (used at login)
//   toolswitch clipboard   show the clipboard history (what the keyboard shortcut runs)
//   toolswitch wallpaper   open the Wallpapers tab
//   toolswitch ocr         select part of the screen and copy its text
//   toolswitch compress [files or folders]   compress them (the right-click menu runs this)
//   toolswitch cutout [files or folders]     remove their background
//   toolswitch sign [PDF files]              sign them
//   toolswitch transcribe [audio or video files]   turn their speech into text
//   toolswitch reminders   open Reminders to add one
//   toolswitch calendar    open today's Google Calendar events
//   toolswitch jira        open the Jira issues assigned to you
//   toolswitch wallpaper-next | wallpaper-prev | wallpaper-random

mod audio;
mod backup;
mod calendar_view;
mod breaks;
mod cleanup;
mod clipboard;
mod clipboard_view;
mod compress;
mod compress_view;
mod config;
mod cutout;
mod cutout_view;
mod desktop;
mod gcal;
mod home;
mod instance;
mod jira;
mod jobs;
mod models;
mod monitor;
mod ocr;
mod ocr_view;
mod organize;
mod power;
mod reminders;
mod screen;
mod services;
mod shortcut_editor;
mod shortcuts;
mod sign;
mod sound;
mod theme;
mod transcribe;
mod transcribe_view;
mod tray;
mod updates;
mod wallpaper;
mod wallpaper_view;

use eframe::egui;
use std::sync::mpsc::{Receiver, channel};

/// What a shortcut, the tray or the command line can ask for.
#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    Clipboard,
    Wallpaper,
    Compress,
    Ocr,
    Cutout,
    Sign,
    Transcribe,
    Reminders,
    Calendar,
    Breaks,
    Organize,
    Backup,
    Cleanup,
    Monitor,
    Power,
    Updates,
    Jira,
}

impl Tool {
    const ALL: [(Tool, &'static str); 17] = [
        (Tool::Clipboard, "clipboard"),
        (Tool::Wallpaper, "wallpaper"),
        (Tool::Compress, "compress"),
        (Tool::Ocr, "ocr"),
        (Tool::Cutout, "cutout"),
        (Tool::Sign, "sign"),
        (Tool::Transcribe, "transcribe"),
        (Tool::Reminders, "reminders"),
        (Tool::Calendar, "calendar"),
        (Tool::Breaks, "breaks"),
        (Tool::Organize, "organize"),
        (Tool::Backup, "backup"),
        (Tool::Cleanup, "cleanup"),
        (Tool::Monitor, "monitor"),
        (Tool::Power, "power"),
        (Tool::Updates, "updates"),
        (Tool::Jira, "jira"),
    ];

    fn id(self) -> &'static str {
        Self::ALL.iter().find(|t| t.0 == self).map_or("", |t| t.1)
    }

    fn from_arg(arg: &str) -> Option<Self> {
        Self::ALL.iter().find(|t| t.1 == arg).map(|t| t.0)
    }

    /// Tools that take files from the command line (the right-click menu).
    fn takes_files(self) -> bool {
        matches!(
            self,
            Tool::Compress | Tool::Cutout | Tool::Sign | Tool::Transcribe
        )
    }

    /// The screen it opens in the main window.
    fn screen(self) -> Screen {
        match self {
            Tool::Clipboard => Screen::Clipboard,
            Tool::Wallpaper => Screen::Wallpapers,
            Tool::Compress => Screen::Compress,
            Tool::Ocr => Screen::Home,
            Tool::Cutout => Screen::Cutout,
            Tool::Sign => Screen::Sign,
            Tool::Transcribe => Screen::Transcribe,
            Tool::Reminders => Screen::Reminders,
            Tool::Calendar => Screen::Calendar,
            Tool::Breaks => Screen::Breaks,
            Tool::Organize => Screen::Organize,
            Tool::Backup => Screen::Backup,
            Tool::Cleanup => Screen::Cleanup,
            Tool::Monitor => Screen::Monitor,
            Tool::Power => Screen::Power,
            Tool::Updates => Screen::Updates,
            Tool::Jira => Screen::Jira,
        }
    }
}

/// The window's screens.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {
    Home,
    Clipboard,
    Wallpapers,
    Reminders,
    Calendar,
    Breaks,
    Compress,
    Cutout,
    Sign,
    Transcribe,
    Organize,
    Backup,
    Cleanup,
    Monitor,
    Power,
    Updates,
    Jira,
    /// The full-screen screenshot to select text on.
    Ocr,
    /// The small window with the text read.
    OcrResult,
}

/// Things the tray, the shortcuts or a second launch ask the window to do.
#[derive(Clone)]
pub enum Command {
    /// The main window with every tool's settings.
    Home,
    Show(Tool),
    /// Put this text on the clipboard.
    Copy(String),
    RecordShortcut(Tool),
    /// Files for a tool: to compress, cut out or sign.
    Files(Tool, Vec<std::path::PathBuf>),
}

/// A screen of the main window, for the tools that don't need anything from the app.
pub trait Page {
    fn ui(&mut self, ui: &mut egui::Ui);
    /// The screen was just opened.
    fn opened(&mut self) {}
    /// Files from the right-click menu or the command line.
    fn add_files(&mut self, _files: &[std::path::PathBuf]) {}
}

/// A sidebar row: the screen, its icon and its name.
type NavItem = (Screen, &'static str, &'static str);

/// The sidebar: groups of screens.
const NAV: &[(&str, &[NavItem])] = &[
    ("", &[(Screen::Home, "🛠", "Tools")]),
    (
        "EVERYDAY",
        &[
            (Screen::Clipboard, "📋", "Clipboard"),
            (Screen::Wallpapers, "🖼", "Wallpapers"),
            (Screen::Reminders, "⏰", "Reminders"),
            (Screen::Calendar, "📅", "Calendar"),
            (Screen::Breaks, "☕", "Breaks"),
        ],
    ),
    (
        "FILES",
        &[
            (Screen::Compress, "📦", "Compress"),
            (Screen::Cutout, "✂", "Remove background"),
            (Screen::Sign, "✒", "Sign PDF"),
            (Screen::Transcribe, "🎤", "Audio to text"),
            (Screen::Organize, "📂", "Tidy downloads"),
            (Screen::Backup, "💾", "Backup"),
        ],
    ),
    (
        "SYSTEM",
        &[
            (Screen::Cleanup, "♻", "Clean up"),
            (Screen::Monitor, "📈", "Watch the PC"),
            (Screen::Power, "🔌", "Shut down timer"),
            (Screen::Updates, "⟳", "Updates"),
        ],
    ),
    ("WORK", &[(Screen::Jira, "☑", "Jira")]),
];

/// A second launch's command line, as one line for the socket: fields separated by tabs.
fn to_line(args: &[String]) -> String {
    let mut fields: Vec<String> = vec![];
    let mut files = false;
    for arg in args {
        if files {
            // Relative paths mean nothing to the running app, which has its own folder.
            let path = std::path::absolute(arg).unwrap_or_else(|_| arg.into());
            fields.push(path.to_string_lossy().into_owned());
        } else if let Some(tool) = Tool::from_arg(arg) {
            files = tool.takes_files();
            fields.push(arg.clone());
        }
    }
    if fields.is_empty() {
        "show".into()
    } else {
        fields.join("\t")
    }
}

fn from_line(line: &str) -> Command {
    let mut fields = line.split('\t');
    let first = fields.next().unwrap_or_default();
    let files: Vec<std::path::PathBuf> = fields.map(Into::into).collect();
    match Tool::from_arg(first) {
        Some(tool) if tool.takes_files() && !files.is_empty() => Command::Files(tool, files),
        Some(tool) => Command::Show(tool),
        None => Command::Home,
    }
}

// The clipboard popup and the main window.
const POPUP_SIZE: [f32; 2] = [440.0, 520.0];
const OCR_RESULT_SIZE: [f32; 2] = [520.0, 360.0];
const WINDOW_SIZE: [f32; 2] = [920.0, 680.0];
const SIDEBAR_WIDTH: f32 = 210.0;
/// Time for the window to be gone before the screenshot.
const OCR_HIDE_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let hidden = args.iter().any(|a| a == "--hidden");

    // A wallpaper shortcut: done by the running app, or here before starting in the tray.
    let mut hidden = hidden;
    if let Some(action) = args.iter().find(|a| a.starts_with("wallpaper-")) {
        if instance::send(action) {
            return Ok(());
        }
        wallpaper::run(action);
        hidden = true;
    }

    // Already running: hand over what to show and leave.
    let line = to_line(&args);
    if instance::send(&line) {
        return Ok(());
    }

    desktop::ensure_menu_entry();
    desktop::ensure_nautilus_scripts();
    desktop::autostart_first_run();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("toolswitch")
        .with_app_id("toolswitch")
        .with_inner_size(WINDOW_SIZE)
        .with_icon(egui::IconData {
            rgba: theme::app_icon(64),
            width: 64,
            height: 64,
        })
        .with_min_inner_size([320.0, 300.0])
        .with_visible(false);
    // eframe always shows the window after its first frame. Started for the tray only, it
    // opens off screen and hides right away, so nothing flashes.
    if hidden {
        viewport = viewport.with_position([-10_000.0, -10_000.0]);
    }
    let options = eframe::NativeOptions {
        viewport,
        // winit has no file drag and drop on Wayland: the window runs through XWayland instead.
        #[cfg(target_os = "linux")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            if std::env::var_os("DISPLAY").is_some() {
                builder.with_x11();
            }
        })),
        ..Default::default()
    };
    eframe::run_native(
        "toolswitch",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            let (commands, inbox) = channel();

            let history = clipboard::History::load();
            let tray = tray::spawn(history.clone(), commands.clone(), ctx.clone());
            {
                let (ctx, tray) = (ctx.clone(), tray.clone());
                history.watch(move || {
                    tray::refresh(&tray);
                    ctx.request_repaint();
                });
            }
            // What a second launch or a shortcut asks for.
            let deliver = {
                let (ctx, commands) = (ctx.clone(), commands.clone());
                move |line: String| {
                    if line.starts_with("wallpaper-") {
                        return wallpaper::run(&line);
                    }
                    let _ = commands.send(from_line(&line));
                    ctx.request_repaint();
                }
            };
            shortcuts::set_handler(deliver.clone());
            instance::listen(deliver)
                .unwrap_or_else(|e| eprintln!("Shortcuts won't reach the app: {e}"));
            // Here, with the event loop running: Windows and macOS want that.
            shortcuts::register_all();
            wallpaper::start_rotation();
            {
                let tray = tray.clone();
                services::start(ctx.clone(), move || tray::refresh(&tray));
            }
            if !hidden {
                let _ = commands.send(from_line(&line));
            }

            Ok(Box::new(App {
                inbox,
                tray,
                screen: Screen::Home,
                popup: false,
                home: home::Home::default(),
                clipboard: clipboard_view::ClipboardView::new(history),
                wallpapers: wallpaper_view::WallpaperView::new(&ctx),
                compress: compress_view::CompressView::new(&ctx),
                cutout: cutout_view::CutoutView::new(&ctx),
                welcomed: config::load().welcomed,
                pages: pages(&ctx),
                ocr: ocr_view::OcrView::new(),
                visible: false,
                hide_on_start: hidden,
                place_on_next_frame: false,
                cover_on_next_frame: false,
                ocr_capture_at: None,
                ocr_result_at: None,
                had_focus: false,
            }))
        }),
    )
}

struct App {
    inbox: Receiver<Command>,
    tray: Option<tray::Handle>,
    screen: Screen,
    /// The clipboard opened from its shortcut: a small window without tabs that hides
    /// when it loses focus.
    popup: bool,
    home: home::Home,
    clipboard: clipboard_view::ClipboardView,
    wallpapers: wallpaper_view::WallpaperView,
    compress: compress_view::CompressView,
    cutout: cutout_view::CutoutView,
    /// False until the welcome screen is closed, the first time the app is used.
    welcomed: bool,
    /// The other screens.
    pages: std::collections::HashMap<Screen, Box<dyn Page>>,
    ocr: ocr_view::OcrView,
    visible: bool,
    hide_on_start: bool,
    place_on_next_frame: bool,
    /// Text from screen: go full screen once the window is up.
    cover_on_next_frame: bool,
    /// The window was up when text from screen was asked for: it hides first, and the
    /// screenshot is taken once it's gone.
    ocr_capture_at: Option<std::time::Instant>,
    /// Out of full screen: size and place the small result window once the window manager let go.
    ocr_result_at: Option<std::time::Instant>,
    // The popup hides when it loses focus, but only after it really had it.
    had_focus: bool,
}

impl App {
    fn show(&mut self, ctx: &egui::Context, screen: Screen, popup: bool) {
        // A shortcut toggles: pressed again while its window is up, it hides it.
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
        if self.visible
            && focused
            && self.screen == screen
            && self.popup == popup
            && screen != Screen::Home
        {
            self.hide(ctx);
            return;
        }
        let was_ocr = matches!(self.screen, Screen::Ocr | Screen::OcrResult);
        if was_ocr {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        }
        if popup != self.popup || !self.visible || was_ocr {
            let size = if popup { POPUP_SIZE } else { WINDOW_SIZE };
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.into()));
        }
        self.popup = popup;
        self.open_tab(screen);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        // The window manager ignores positions given to a hidden window, so it moves right after.
        self.place_on_next_frame = popup;
        self.visible = true;
        self.had_focus = false;
    }

    fn open_tab(&mut self, screen: Screen) {
        self.screen = screen;
        match screen {
            Screen::Clipboard => self.clipboard.opened(),
            Screen::Wallpapers => self.wallpapers.reload(),
            _ => {
                if let Some(page) = self.pages.get_mut(&screen) {
                    page.opened();
                }
            }
        }
    }

    /// Text from screen: the window covers the monitor with a screenshot of it.
    fn start_ocr(&mut self, ctx: &egui::Context) {
        if self.visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
            self.hide(ctx);
            self.ocr_capture_at = Some(std::time::Instant::now() + OCR_HIDE_WAIT);
            ctx.request_repaint_after(OCR_HIDE_WAIT);
            return;
        }
        let Some(monitor) = self.ocr.start(ctx) else {
            desktop::notify("Text from screen", "Could not take a screenshot.");
            return;
        };
        let scale = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0);
        self.popup = false;
        self.screen = Screen::Ocr;
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            monitor.width / scale,
            monitor.height / scale,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            monitor.x / scale,
            monitor.y / scale,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.cover_on_next_frame = true;
        self.visible = true;
        self.had_focus = false;
    }

    fn end_ocr(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        self.hide(ctx);
        self.screen = Screen::Home;
    }

    /// An area was picked: from the full-screen screenshot to the small result window.
    fn show_ocr_result(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        self.screen = Screen::OcrResult;
        // Leaving full screen takes the window manager a moment; sized before, it ends up maximized.
        self.ocr_result_at = Some(std::time::Instant::now() + OCR_HIDE_WAIT);
        ctx.request_repaint_after(OCR_HIDE_WAIT);
        self.had_focus = false;
    }

    /// Centered horizontally, in the upper part of the screen the mouse is on.
    fn place(&self, ctx: &egui::Context, width: f32) {
        let Some(m) = screen::monitor_under_mouse() else {
            return;
        };
        let scale = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0);
        let x = m.x / scale + (m.width / scale - width) / 2.0;
        let y = m.y / scale + m.height / scale * 0.18;
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
    }

    pub fn hide(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.visible = false;
    }

    /// The navigation on the left: every screen, what's coming, and start at login.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .exact_size(SIDEBAR_WIDTH)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::SIDEBAR)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(12, 18)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    let logo = theme::logo(ui.ctx());
                    ui.add(egui::Image::new(&logo).fit_to_exact_size(egui::vec2(20.0, 20.0)));
                    ui.label(egui::RichText::new("toolswitch").size(17.0).strong());
                });
                ui.add_space(12.0);
                let footer = 64.0;
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height() - footer)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for (group, items) in NAV {
                            if !group.is_empty() {
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.add_space(10.0);
                                    ui.label(
                                        egui::RichText::new(*group).size(11.0).color(theme::FAINT),
                                    );
                                });
                                ui.add_space(2.0);
                            }
                            for &(screen, icon, name) in *items {
                                if nav_item(ui, icon, name, self.screen == screen, true).clicked()
                                    && self.screen != screen
                                {
                                    self.open_tab(screen);
                                }
                            }
                        }
                    });

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                                .size(11.0)
                                .color(theme::FAINT),
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        let mut on = desktop::autostart_enabled();
                        if theme::toggle(ui, &mut on).changed()
                            && let Err(error) = desktop::set_autostart(on)
                        {
                            eprintln!("Could not change start at login: {error}");
                        }
                        ui.label(
                            egui::RichText::new("Start at login")
                                .size(13.0)
                                .color(theme::MUTED),
                        );
                    });
                    ui.add_space(4.0);
                    ui.separator();
                });
            });
    }
}

/// A row in the sidebar. Disabled ones are only shown, faded.
fn nav_item(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), sense);
    let hover = ui
        .ctx()
        .animate_bool(response.id, enabled && response.hovered());
    let fill = if selected {
        theme::CARD_HOVER
    } else {
        theme::CARD.gamma_multiply(hover)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, fill);
    let color = if !enabled {
        theme::FAINT
    } else if selected {
        theme::TEXT
    } else {
        theme::MUTED
    };
    painter.text(
        rect.left_center() + egui::vec2(12.0, 0.0),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        color,
    );
    painter.text(
        rect.left_center() + egui::vec2(38.0, 0.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(14.0),
        color,
    );
    if selected {
        let bar = egui::Rect::from_center_size(
            rect.left_center() + egui::vec2(1.5, 0.0),
            egui::vec2(3.0, 16.0),
        );
        painter.rect_filled(bar, 2.0, theme::ACCENT);
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BACKGROUND.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.hide_on_start) {
            self.hide(ctx);
        }
        tray::poll();
        if self.visible && std::mem::take(&mut self.place_on_next_frame) {
            self.place(ctx, POPUP_SIZE[0]);
        }
        if self.visible && std::mem::take(&mut self.cover_on_next_frame) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        if self
            .ocr_capture_at
            .is_some_and(|at| std::time::Instant::now() >= at)
        {
            self.ocr_capture_at = None;
            self.start_ocr(ctx);
        }
        if self.visible
            && self
                .ocr_result_at
                .is_some_and(|at| std::time::Instant::now() >= at)
        {
            self.ocr_result_at = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(OCR_RESULT_SIZE.into()));
            self.place(ctx, OCR_RESULT_SIZE[0]);
        }
        // The text is copied as soon as it's read. If the result window was closed by then,
        // a notification says so.
        if let Some(result) = self.ocr.poll() {
            let showing = self.visible && self.screen == Screen::OcrResult;
            match result {
                Ok(text) if text.is_empty() => {
                    if !showing {
                        desktop::notify("Text from screen", "No text found there.");
                    }
                }
                Ok(text) => {
                    self.clipboard.copy(&text);
                    if !showing {
                        desktop::notify("Text copied", &text);
                    }
                }
                Err(error) => {
                    if !showing {
                        desktop::notify("Could not read the text", &error);
                    }
                }
            }
        }
        while let Ok(command) = self.inbox.try_recv() {
            match command {
                Command::Home => self.show(ctx, Screen::Home, false),
                Command::Show(Tool::Clipboard) => self.show(ctx, Screen::Clipboard, true),
                Command::Show(Tool::Ocr) => self.start_ocr(ctx),
                Command::Show(tool) => self.show(ctx, tool.screen(), false),
                Command::Files(tool, files) => {
                    if !(self.visible && self.screen == tool.screen()) {
                        self.show(ctx, tool.screen(), false);
                    }
                    match tool {
                        Tool::Compress => self.compress.add(&files),
                        Tool::Cutout => self.cutout.add(&files),
                        _ => {
                            if let Some(page) = self.pages.get_mut(&tool.screen()) {
                                page.add_files(&files);
                            }
                        }
                    }
                }
                Command::Copy(text) => self.clipboard.copy(&text),
                Command::RecordShortcut(tool) => {
                    self.show(ctx, Screen::Home, false);
                    self.home.shortcuts.record(tool.id());
                }
            }
        }

        // Closing the window only hides it: the app stays in the tray.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide(ctx);
        }

        // The popup hides when you click anywhere else.
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
        if self.visible && focused {
            self.had_focus = true;
        } else if self.visible
            && self.had_focus
            && !focused
            && self.popup
            && !self.clipboard.is_recording()
        {
            self.hide(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The clipboard popup and the screenshot to select text on are the whole window.
        let ctx = ui.ctx().clone();
        let step = match self.screen {
            Screen::Ocr => Some(self.ocr.select_ui(ui)),
            Screen::OcrResult => Some(self.ocr.result_ui(ui)),
            _ => None,
        };
        if let Some(step) = step {
            match step {
                ocr_view::Step::Stay => {}
                ocr_view::Step::Hide => self.end_ocr(&ctx),
                ocr_view::Step::ShowResult => self.show_ocr_result(&ctx),
                ocr_view::Step::Retry => self.start_ocr(&ctx),
                ocr_view::Step::Copy(text) => self.clipboard.copy(&text),
            }
            return;
        }
        if self.popup {
            let action = self.clipboard.ui(ui, true);
            if action.hide {
                self.hide(ui.ctx());
            }
            if action.changed {
                tray::refresh(&self.tray);
            }
            return;
        }

        // The first time, a welcome screen instead of the usual window.
        if !self.welcomed {
            let mut changed = false;
            egui::CentralPanel::no_frame().show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(36, 28))
                    .show(ui, |ui| {
                        let (done, tools_changed) = self.home.welcome(ui);
                        changed = tools_changed;
                        if done {
                            config::update(|c| c.welcomed = true);
                            self.welcomed = true;
                        }
                    });
            });
            if changed {
                tray::refresh(&self.tray);
            }
            return;
        }

        self.sidebar(ui);
        let mut changed = false;
        let mut open = None;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(28, 24))
                .show(ui, |ui| match self.screen {
                    Screen::Home => {
                        let action = self.home.ui(ui);
                        changed = action.changed;
                        open = action.open;
                    }
                    // Picking an entry or Esc closes the popup; in the main window it stays.
                    Screen::Clipboard => changed = self.clipboard.ui(ui, false).changed,
                    Screen::Wallpapers => self.wallpapers.ui(ui),
                    Screen::Compress => self.compress.ui(ui),
                    Screen::Cutout => self.cutout.ui(ui),
                    screen => {
                        if let Some(page) = self.pages.get_mut(&screen) {
                            page.ui(ui);
                        }
                    }
                });
        });
        if let Some(screen) = open {
            self.open_tab(screen);
        }
        if changed {
            tray::refresh(&self.tray);
        }
    }
}

/// The screens that are pages, each with its own state.
fn pages(ctx: &egui::Context) -> std::collections::HashMap<Screen, Box<dyn Page>> {
    let _ = ctx;
    let mut pages: std::collections::HashMap<Screen, Box<dyn Page>> = Default::default();
    pages.insert(
        Screen::Reminders,
        Box::<reminders::RemindersPage>::default(),
    );
    pages.insert(Screen::Breaks, Box::<breaks::BreaksPage>::default());
    pages.insert(Screen::Power, Box::<power::PowerPage>::default());
    pages.insert(Screen::Monitor, Box::<monitor::MonitorPage>::default());
    pages.insert(Screen::Cleanup, Box::<cleanup::CleanupPage>::default());
    pages.insert(Screen::Organize, Box::<organize::OrganizePage>::default());
    pages.insert(Screen::Backup, Box::<backup::BackupPage>::default());
    pages.insert(Screen::Updates, Box::<updates::UpdatesPage>::default());
    pages.insert(Screen::Sign, Box::<sign::SignPage>::default());
    pages.insert(
        Screen::Transcribe,
        Box::<transcribe_view::TranscribePage>::default(),
    );
    pages.insert(Screen::Jira, Box::<jira::JiraPage>::default());
    pages.insert(Screen::Calendar, Box::new(calendar_view::CalendarPage::new()));
    for (screen, name) in [] {
        pages.insert(screen, Box::new(Placeholder(name)));
    }
    pages
}

/// A screen still being built.
struct Placeholder(&'static str);

impl Page for Placeholder {
    fn ui(&mut self, ui: &mut egui::Ui) {
        theme::page_header(ui, self.0, "Coming soon.");
    }
}

/// What a tool's view asks the app to do after drawing.
#[derive(Default)]
pub struct Action {
    pub hide: bool,
    /// The data changed: the tray menu needs rebuilding.
    pub changed: bool,
}
