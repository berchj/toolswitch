// The tray icon and its menu. What the menu shows is built once here; it's drawn by ksni on
// Linux (D-Bus, so no GTK is needed) and by tray-icon on Windows and macOS. Items that
// change the window are sent to the app as a Command.

use crate::{Command, Tool, clipboard, config, desktop, shortcuts};
use eframe::egui;
use std::sync::mpsc::Sender;

/// What clicking a menu item does.
#[derive(Clone)]
pub enum Action {
    Send(Command),
    PauseBreaks,
    PauseClipboard,
    ClearHistory,
    Autostart,
    Quit,
}

pub enum Entry {
    /// A disabled item only shows something (the next reminder, say).
    Item(String, Option<Action>),
    Check(String, bool, Action),
    Separator,
}

#[derive(Clone)]
pub struct Tray {
    history: clipboard::History,
    commands: Sender<Command>,
    ctx: egui::Context,
}

impl Tray {
    fn perform(&self, action: &Action) {
        match action {
            Action::Send(command) => {
                let _ = self.commands.send(command.clone());
            }
            Action::PauseBreaks => crate::breaks::pause(60),
            Action::PauseClipboard => {
                config::update(|c| c.clipboard_paused = !c.clipboard_paused);
            }
            Action::ClearHistory => self.history.clear(),
            Action::Autostart => {
                if let Err(error) = desktop::set_autostart(!desktop::autostart_enabled()) {
                    eprintln!("Could not change start at login: {error}");
                }
            }
            Action::Quit => std::process::exit(0),
        }
        self.ctx.request_repaint();
    }

    fn entries(&self) -> Vec<Entry> {
        let item = |label: String, action: Action| Entry::Item(label, Some(action));
        let show = |tool| Action::Send(Command::Show(tool));
        let mut menu = vec![];

        // What's going on: next reminder, break.
        if let Some(r) = crate::reminders::next() {
            menu.push(Entry::Item(
                format!("⏰ {} · {}", crate::reminders::when(r.at), label(&r.text)),
                None,
            ));
        }
        if config::enabled("breaks") {
            menu.push(Entry::Item(format!("☕ {}", crate::breaks::status()), None));
            menu.push(item("Pause breaks for 1 hour".into(), Action::PauseBreaks));
        }
        if !menu.is_empty() {
            menu.push(Entry::Separator);
        }

        menu.extend([
            item("Open toolswitch".into(), Action::Send(Command::Home)),
            item(
                format!("Show clipboard history ({})", shortcuts::get("clipboard")),
                show(Tool::Clipboard),
            ),
            item(
                format!("New reminder… ({})", shortcuts::get("reminders")),
                show(Tool::Reminders),
            ),
            item(
                format!("Copy text from screen ({})", shortcuts::get("ocr")),
                show(Tool::Ocr),
            ),
            Entry::Separator,
        ]);

        menu.extend([
            Entry::Check(
                "Pause recording".into(),
                config::load().clipboard_paused,
                Action::PauseClipboard,
            ),
            item("Clear history".into(), Action::ClearHistory),
            item(
                "Change shortcut…".into(),
                Action::Send(Command::RecordShortcut(Tool::Clipboard)),
            ),
            Entry::Check(
                "Start at login".into(),
                desktop::autostart_enabled(),
                Action::Autostart,
            ),
            Entry::Separator,
            item("Quit".into(), Action::Quit),
        ]);
        menu
    }
}

/// One line, at most 40 characters.
fn label(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 40 {
        line.chars().take(39).chain(['…']).collect()
    } else {
        line
    }
}

pub use backend::{Handle, refresh, spawn};

// ---------- Linux: ksni ----------

#[cfg(target_os = "linux")]
mod backend {
    use super::{Action, Entry, Tray};
    use crate::{Command, Tool, clipboard};
    use eframe::egui;
    use ksni::menu::{CheckmarkItem, StandardItem};
    use ksni::{MenuItem, blocking::TrayMethods};
    use std::sync::mpsc::Sender;

    pub struct Linux(Tray);

    pub type Handle = ksni::blocking::Handle<Linux>;

    pub fn spawn(
        history: clipboard::History,
        commands: Sender<Command>,
        ctx: egui::Context,
    ) -> Option<Handle> {
        Linux(Tray {
            history,
            commands,
            ctx,
        })
        .spawn()
        .map_err(|e| eprintln!("No tray icon: {e}"))
        .ok()
    }

    /// Rebuilds the menu, e.g. after something new was copied. Any thread.
    pub fn refresh(tray: &Option<Handle>) {
        if let Some(tray) = tray {
            tray.update(|_| {});
        }
    }

    fn activate(action: Action) -> Box<dyn Fn(&mut Linux) + Send> {
        Box::new(move |t: &mut Linux| t.0.perform(&action))
    }

    impl ksni::Tray for Linux {
        fn id(&self) -> String {
            "toolswitch".into()
        }

        fn title(&self) -> String {
            "toolswitch".into()
        }

        /// No theme icon: GNOME's tray would prefer it over our own picture.
        fn icon_name(&self) -> String {
            String::new()
        }

        /// Our own icon.
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [22u32, 32, 48]
                .into_iter()
                .map(|size| {
                    // ksni wants ARGB, each pixel in network byte order.
                    let data = crate::theme::app_icon(size)
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|p| [p[3], p[0], p[1], p[2]])
                        .collect();
                    ksni::Icon {
                        width: size as i32,
                        height: size as i32,
                        data,
                    }
                })
                .collect()
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.0
                .perform(&Action::Send(Command::Show(Tool::Clipboard)));
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            self.0
                .entries()
                .into_iter()
                .map(|entry| match entry {
                    // A single underscore would become a keyboard accelerator.
                    Entry::Item(label, action) => StandardItem {
                        label: label.replace('_', "__"),
                        enabled: action.is_some(),
                        activate: action
                            .map_or_else(|| Box::new(|_: &mut Linux| {}) as _, activate),
                        ..Default::default()
                    }
                    .into(),
                    Entry::Check(label, checked, action) => CheckmarkItem {
                        label: label.replace('_', "__"),
                        checked,
                        activate: activate(action),
                        ..Default::default()
                    }
                    .into(),
                    Entry::Separator => MenuItem::Separator,
                })
                .collect()
        }
    }
}

// ---------- Windows and macOS: tray-icon ----------

#[cfg(not(target_os = "linux"))]
mod backend {
    use super::{Action, Entry, Tray};
    use crate::{Command, clipboard};
    use eframe::egui;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Mutex, OnceLock};
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

    /// The tray icon lives on the main thread; other threads ask for a rebuild with this.
    #[derive(Clone)]
    pub struct Handle;

    static TRAY: OnceLock<Tray> = OnceLock::new();
    static ACTIONS: OnceLock<Mutex<HashMap<String, Action>>> = OnceLock::new();
    static REBUILD: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static ICON: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    }

    /// Call on the main thread, once the event loop runs.
    pub fn spawn(
        history: clipboard::History,
        commands: Sender<Command>,
        ctx: egui::Context,
    ) -> Option<Handle> {
        let tray = TRAY.get_or_init(|| Tray {
            history,
            commands,
            ctx: ctx.clone(),
        });
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            let action = ACTIONS
                .get()
                .and_then(|a| a.lock().ok()?.get(event.id.as_ref()).cloned());
            if let (Some(action), Some(tray)) = (action, TRAY.get()) {
                tray.perform(&action);
            }
        }));
        TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } = event
                && let Some(tray) = TRAY.get()
            {
                tray.perform(&Action::Send(Command::Home));
            }
        }));
        let icon = TrayIconBuilder::new()
            .with_tooltip("toolswitch")
            .with_icon(icon())
            .with_menu(Box::new(menu(tray)))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| eprintln!("No tray icon: {e}"))
            .ok()?;
        ICON.with(|i| *i.borrow_mut() = Some(icon));
        Some(Handle)
    }

    /// Any thread: the menu is rebuilt on the main thread next frame.
    pub fn refresh(_tray: &Option<Handle>) {
        REBUILD.store(true, Ordering::SeqCst);
        if let Some(tray) = TRAY.get() {
            tray.ctx.request_repaint();
        }
    }

    /// Main thread, every frame: rebuilds the menu if asked.
    pub fn poll() {
        if !REBUILD.swap(false, Ordering::SeqCst) {
            return;
        }
        let Some(tray) = TRAY.get() else { return };
        ICON.with(|i| {
            if let Some(icon) = i.borrow().as_ref() {
                icon.set_menu(Some(Box::new(menu(tray))));
            }
        });
    }

    fn menu(tray: &Tray) -> Menu {
        let menu = Menu::new();
        let mut actions = HashMap::new();
        for (i, entry) in tray.entries().into_iter().enumerate() {
            let id = format!("item-{i}");
            let _ = match entry {
                Entry::Item(label, action) => {
                    let item = MenuItem::with_id(id.clone(), label, action.is_some(), None);
                    if let Some(action) = action {
                        actions.insert(id, action);
                    }
                    menu.append(&item)
                }
                Entry::Check(label, checked, action) => {
                    actions.insert(id.clone(), action);
                    menu.append(&CheckMenuItem::with_id(id, label, true, checked, None))
                }
                Entry::Separator => menu.append(&PredefinedMenuItem::separator()),
            };
        }
        *ACTIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = actions;
        menu
    }

    fn icon() -> tray_icon::Icon {
        let size = 32;
        tray_icon::Icon::from_rgba(crate::theme::app_icon(size), size, size).expect("valid icon")
    }
}

/// Windows and macOS rebuild the menu on the main thread; on Linux there's nothing to do.
pub fn poll() {
    #[cfg(not(target_os = "linux"))]
    backend::poll();
}
