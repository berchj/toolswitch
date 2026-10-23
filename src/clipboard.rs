// Clipboard history: a background thread notices when the clipboard changes and keeps the
// newest texts and images first. The list is saved to clipboard.json (texts in the same shape
// clipswitch used); images are PNG files in the cache folder, with a small one for the list.
//
// Noticing a change costs almost nothing: X11 sends an event (XFixes), Windows and macOS keep
// a counter. Only then is the clipboard read. On Wayland the X11 event (through XWayland) is
// used too, and the text is also compared every half second in case a copy doesn't reach X11.
// Without any event, images aren't kept.

use crate::config;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(500);
const MAX_ENTRIES: usize = 50;
// Very large copies (whole files, logs) are skipped.
const MAX_CHARS: usize = 100_000;
/// Bigger images (over 40 megapixels) are skipped.
const MAX_PIXELS: usize = 40_000_000;
const THUMB: u32 = 160;

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    /// For an image, empty.
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub time: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// An image: its id (the name of its files in the cache) and size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Picture>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Picture {
    pub id: String,
    pub width: u32,
    pub height: u32,
}

impl Entry {
    /// What tells entries apart: the text, or the image's id.
    pub fn key(&self) -> &str {
        self.image.as_ref().map_or(&self.text, |p| &p.id)
    }
}

#[derive(Clone, Default)]
pub struct History(Arc<Mutex<Vec<Entry>>>);

impl History {
    /// Loads the saved history. The first time, it brings over the texts from clipswitch.
    pub fn load() -> Self {
        let list = read(&file())
            .or_else(|| read(&clipswitch_file()))
            .unwrap_or_default();
        History(Arc::new(Mutex::new(list)))
    }

    /// Pinned entries first, then the rest, newest first.
    pub fn entries(&self) -> Vec<Entry> {
        let list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (pinned, rest): (Vec<_>, Vec<_>) = list.iter().cloned().partition(|e| e.pinned);
        pinned.into_iter().chain(rest).collect()
    }

    fn change(&self, edit: impl FnOnce(&mut Vec<Entry>)) {
        let mut list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let before: Vec<String> = images(&list);
        edit(&mut list);
        let _ = config::write_json(&file(), &*list);
        // The files of images no longer in the list go too.
        let after = images(&list);
        for id in before.iter().filter(|id| !after.contains(id)) {
            let _ = std::fs::remove_file(image_file(id));
            let _ = std::fs::remove_file(thumb_file(id));
        }
    }

    /// Copying something already in the list moves it to the top (still pinned if it was).
    fn add(&self, entry: Entry) {
        self.change(|list| {
            let key = entry.key().to_string();
            let pinned = list.iter().any(|e| e.key() == key && e.pinned);
            list.retain(|e| e.key() != key);
            list.insert(0, Entry { pinned, ..entry });
            keep(list, MAX_ENTRIES);
        });
    }

    pub fn remove(&self, key: &str) {
        self.change(|list| list.retain(|e| e.key() != key));
    }

    pub fn toggle_pin(&self, key: &str) {
        self.change(|list| {
            if let Some(e) = list.iter_mut().find(|e| e.key() == key) {
                e.pinned = !e.pinned;
            }
        });
    }

    /// Pinned entries survive.
    pub fn clear(&self) {
        self.change(|list| keep(list, 0));
    }

    /// Starts the watcher; `on_change` runs after each new entry.
    pub fn watch(&self, on_change: impl Fn() + Send + 'static) {
        let history = self.clone();
        std::thread::spawn(move || {
            let Ok(mut clipboard) = arboard::Clipboard::new() else {
                return;
            };
            let mut changes = changes::Watcher::new();
            // What's already on the clipboard at startup isn't new.
            let mut last_text = clipboard.get_text().unwrap_or_default();
            let mut last_image: Option<String> = None;
            loop {
                // Waits for a change: an event, a counter that moved, or half a second.
                let Some(changed) = changes.wait(POLL) else {
                    continue;
                };
                let cfg = config::load();
                if cfg.clipboard_paused || cfg.disabled.contains("clipboard") {
                    continue;
                }
                if let Ok(text) = clipboard.get_text() {
                    if text == last_text {
                        continue;
                    }
                    last_text = text.clone();
                    if !text.trim().is_empty() && text.chars().count() <= MAX_CHARS {
                        history.add(Entry {
                            text,
                            time: now(),
                            pinned: false,
                            image: None,
                        });
                        on_change();
                    }
                } else if changed
                    && let Ok(image) = clipboard.get_image()
                    && let Some(picture) = save_image(&image)
                    && last_image.as_ref() != Some(&picture.id)
                {
                    last_image = Some(picture.id.clone());
                    last_text.clear();
                    history.add(Entry {
                        text: String::new(),
                        time: now(),
                        pinned: false,
                        image: Some(picture),
                    });
                    on_change();
                }
            }
        });
    }
}

fn images(list: &[Entry]) -> Vec<String> {
    list.iter()
        .filter_map(|e| e.image.as_ref().map(|p| p.id.clone()))
        .collect()
}

/// Pinned entries are always kept; the rest is capped at `unpinned`.
fn keep(list: &mut Vec<Entry>, unpinned: usize) {
    let mut kept = 0;
    list.retain(|e| {
        e.pinned || {
            kept += 1;
            kept <= unpinned
        }
    });
}

// ---------- images ----------

fn images_dir() -> PathBuf {
    config::cache_dir().join("clipboard")
}

fn image_file(id: &str) -> PathBuf {
    images_dir().join(format!("{id}.png"))
}

pub fn thumb_file(id: &str) -> PathBuf {
    images_dir().join(format!("{id}-small.png"))
}

/// Saves a copied image (and a small one for the list). Its id comes from its pixels, so the
/// same image copied twice is one entry.
fn save_image(image: &arboard::ImageData) -> Option<Picture> {
    use std::hash::{Hash, Hasher};
    if image.width * image.height > MAX_PIXELS {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    image.bytes.hash(&mut hasher);
    let id = format!("img-{:016x}", hasher.finish());
    let rgba = image::RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.to_vec(),
    )?;
    std::fs::create_dir_all(images_dir()).ok()?;
    if !image_file(&id).exists() {
        rgba.save(image_file(&id)).ok()?;
        image::imageops::thumbnail(&rgba, THUMB, THUMB * rgba.height() / rgba.width().max(1))
            .save(thumb_file(&id))
            .ok()?;
    }
    Some(Picture {
        id,
        width: rgba.width(),
        height: rgba.height(),
    })
}

/// Puts a text on the clipboard. On X11 the text lives only while this
/// handle exists, so the app keeps one for its whole life.
pub fn copy(clipboard: &mut Option<arboard::Clipboard>, text: &str) {
    if clipboard.is_none() {
        *clipboard = arboard::Clipboard::new().ok();
    }
    if let Some(clipboard) = clipboard {
        let _ = clipboard.set_text(text);
    }
}

/// Puts a saved image back on the clipboard.
pub fn copy_image(clipboard: &mut Option<arboard::Clipboard>, picture: &Picture) {
    if clipboard.is_none() {
        *clipboard = arboard::Clipboard::new().ok();
    }
    let Some(clipboard) = clipboard else { return };
    let Ok(image) = image::open(image_file(&picture.id)) else {
        return;
    };
    let rgba = image.into_rgba8();
    let _ = clipboard.set_image(arboard::ImageData {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        bytes: rgba.into_raw().into(),
    });
}

/// "now", "5m", "3h", "2d"
pub fn ago(time: u64) -> String {
    let s = now().saturating_sub(time) / 1000;
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

// ---------- noticing changes ----------

mod changes {
    use std::time::Duration;

    /// Tells when the clipboard may have changed.
    pub enum Watcher {
        /// X11: a thread sleeps until X11 says the clipboard has a new owner. On Wayland the
        /// text is also compared every half second (the bool), in case a copy from a Wayland
        /// app doesn't reach X11; images need the event.
        #[cfg(target_os = "linux")]
        X11(std::sync::mpsc::Receiver<()>, bool),
        /// Windows and macOS: a number the system raises at each change.
        Counter(Option<u64>),
        /// Nothing better: look every time.
        Poll,
    }

    impl Watcher {
        pub fn new() -> Self {
            #[cfg(target_os = "linux")]
            if let Some(events) = x11::listen() {
                let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
                return Watcher::X11(events, wayland);
            }
            match counter() {
                Some(n) => Watcher::Counter(Some(n)),
                None => Watcher::Poll,
            }
        }

        /// Waits up to `timeout`. Some(true): it changed. Some(false): it may have (polling
        /// can't tell). None: nothing happened.
        pub fn wait(&mut self, timeout: Duration) -> Option<bool> {
            match self {
                #[cfg(target_os = "linux")]
                Watcher::X11(events, poll_too) => match events.recv_timeout(timeout) {
                    Ok(()) => {
                        // A copy often comes with several events: take them all.
                        while events.try_recv().is_ok() {}
                        Some(true)
                    }
                    Err(_) => poll_too.then_some(false),
                },
                Watcher::Counter(last) => {
                    std::thread::sleep(timeout);
                    let now = counter();
                    if now == *last {
                        return None;
                    }
                    *last = now;
                    Some(true)
                }
                Watcher::Poll => {
                    std::thread::sleep(timeout);
                    Some(false)
                }
            }
        }
    }

    #[cfg(windows)]
    fn counter() -> Option<u64> {
        // SAFETY: no arguments, it only reads a number.
        let n = unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
        (n != 0).then_some(n as u64)
    }

    #[cfg(target_os = "macos")]
    fn counter() -> Option<u64> {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        Some(pasteboard.changeCount() as u64)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn counter() -> Option<u64> {
        None
    }

    #[cfg(target_os = "linux")]
    mod x11 {
        use std::sync::mpsc::{Receiver, channel};
        use x11rb::connection::Connection;
        use x11rb::protocol::Event;
        use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
        use x11rb::protocol::xproto::ConnectionExt as _;

        /// Asks X11 to tell when the CLIPBOARD changes owner; each time, a () arrives.
        pub fn listen() -> Option<Receiver<()>> {
            let (conn, screen) = x11rb::connect(None).ok()?;
            conn.xfixes_query_version(5, 0).ok()?.reply().ok()?;
            let root = conn.setup().roots[screen].root;
            let clipboard = conn
                .intern_atom(false, b"CLIPBOARD")
                .ok()?
                .reply()
                .ok()?
                .atom;
            let mask = SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE;
            conn.xfixes_select_selection_input(root, clipboard, mask)
                .ok()?
                .check()
                .ok()?;
            conn.flush().ok()?;
            let (sender, receiver) = channel();
            std::thread::spawn(move || {
                // Blocks until X11 has something: no processor used while waiting.
                while let Ok(event) = conn.wait_for_event() {
                    if matches!(event, Event::XfixesSelectionNotify(_)) && sender.send(()).is_err() {
                        break;
                    }
                }
            });
            Some(receiver)
        }
    }
}

// ---------- files ----------

fn file() -> PathBuf {
    config::dir().join("clipboard.json")
}

fn clipswitch_file() -> PathBuf {
    config::config_dir().join("clipswitch").join("history.json")
}

/// clipswitch also kept images in its own way; those entries have no text and are left out.
fn read(path: &PathBuf) -> Option<Vec<Entry>> {
    let raw: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(
        raw.into_iter()
            .filter_map(|v| serde_json::from_value::<Entry>(v).ok())
            .filter(|e| !e.text.is_empty() || e.image.is_some())
            .collect(),
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

