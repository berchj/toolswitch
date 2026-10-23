// Which monitor the mouse is on, a screenshot of it, and whether Super is held. On Linux this
// asks X11 (on Wayland apps can't know, so these return None and windows open wherever the
// system puts them); on Windows and macOS it uses xcap.

pub use imp::{capture_under_mouse, monitor_under_mouse, super_down};

/// A monitor's place and size, in physical pixels.
pub struct Monitor {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Monitor;
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask};
    /// The monitor the mouse is on, in physical pixels.
    pub fn monitor_under_mouse() -> Option<Monitor> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        let pointer = conn.query_pointer(root).ok()?.reply().ok()?;
        let (px, py) = (pointer.root_x as i32, pointer.root_y as i32);
        let monitors = conn
            .randr_get_monitors(root, true)
            .ok()?
            .reply()
            .ok()?
            .monitors;
        let m = monitors.iter().find(|m| {
            let (x, y) = (m.x as i32, m.y as i32);
            px >= x && px < x + m.width as i32 && py >= y && py < y + m.height as i32
        })?;
        Some(Monitor {
            x: m.x as f32,
            y: m.y as f32,
            width: m.width as f32,
            height: m.height as f32,
        })
    }

    /// Is the Super (Windows) key held down right now?
    pub fn super_down() -> bool {
        let Ok((conn, screen)) = x11rb::connect(None) else {
            return false;
        };
        let root = conn.setup().roots[screen].root;
        conn.query_pointer(root)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|p| p.mask.contains(KeyButMask::MOD4))
    }

    /// A screenshot of the monitor the mouse is on, and where that monitor is.
    pub fn capture_under_mouse() -> Option<(Monitor, image::RgbImage)> {
        // On Wayland X11 only sees black: the desktop's screenshot portal is asked instead.
        if std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland") {
            let image = super::portal::screenshot()
                .map_err(|e| eprintln!("Screenshot portal: {e}"))
                .ok()?;
            let monitor = Monitor {
                x: 0.0,
                y: 0.0,
                width: image.width() as f32,
                height: image.height() as f32,
            };
            return Some((monitor, image));
        }
        x11_capture()
    }

    fn x11_capture() -> Option<(Monitor, image::RgbImage)> {
        use x11rb::protocol::xproto::ImageFormat;
        let monitor = monitor_under_mouse()?;
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        let (width, height) = (monitor.width as u16, monitor.height as u16);
        let reply = conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                root,
                monitor.x as i16,
                monitor.y as i16,
                width,
                height,
                !0,
            )
            .ok()?
            .reply()
            .ok()?;
        // 24 and 32 bit screens come as BGRX, four bytes per pixel.
        if reply.data.len() < width as usize * height as usize * 4 {
            return None;
        }
        let rgb = reply
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect();
        let image = image::RgbImage::from_raw(width.into(), height.into(), rgb)?;
        Some((monitor, image))
    }
}

/// The screenshot portal (org.freedesktop.portal.Screenshot), for Wayland.
#[cfg(target_os = "linux")]
mod portal {
    use futures_util::StreamExt;
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedValue, Value};

    pub fn screenshot() -> Result<image::RgbImage, String> {
        // zbus here runs on tokio, so a small one is started just for this.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        // Silent first; where the system doesn't allow that, with its own dialog.
        let path = runtime
            .block_on(ask(false))
            .or_else(|_| runtime.block_on(ask(true)))?;
        let image = image::open(&path).map_err(|e| e.to_string())?.into_rgb8();
        // The portal saves it like a normal screenshot; this one was only for us.
        let _ = std::fs::remove_file(&path);
        Ok(image)
    }

    async fn ask(interactive: bool) -> Result<std::path::PathBuf, String> {
        let e = |e: zbus::Error| e.to_string();
        let conn = zbus::Connection::session().await.map_err(e)?;
        // The answer comes as a signal on a path made of our name and a token; listening
        // starts before asking, so it can't be missed.
        let token = format!("toolswitch{}{}", std::process::id(), u8::from(interactive));
        let sender = conn
            .unique_name()
            .ok_or("no D-Bus name")?
            .trim_start_matches(':')
            .replace('.', "_");
        let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        let request = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            request_path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .await
        .map_err(e)?;
        let mut responses = request.receive_signal("Response").await.map_err(e)?;

        let portal = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
        )
        .await
        .map_err(e)?;
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("interactive", Value::from(interactive));
        portal
            .call_method("Screenshot", &("", options))
            .await
            .map_err(e)?;

        let message = responses.next().await.ok_or("no answer from the portal")?;
        let (code, results): (u32, HashMap<String, OwnedValue>) =
            message.body().deserialize().map_err(e)?;
        if code != 0 {
            return Err("the screenshot was refused".into());
        }
        let uri: String = results
            .get("uri")
            .and_then(|v| String::try_from(v.clone()).ok())
            .ok_or("no picture in the answer")?;
        let path = uri.strip_prefix("file://").ok_or("not a file")?;
        Ok(percent_decode(path).into())
    }

    /// "%20" -> " "
    fn percent_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            // Read as bytes: slicing the text could cut a letter in half.
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
                && let Ok(b) = u8::from_str_radix(hex, 16)
            {
                out.push(b);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::Monitor;

    /// The monitor with the mouse on Windows; the main one on macOS.
    fn current() -> Option<xcap::Monitor> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::POINT;
            use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
            let mut p = POINT { x: 0, y: 0 };
            // SAFETY: GetCursorPos only writes the point it's given.
            if unsafe { GetCursorPos(&mut p) } != 0
                && let Ok(m) = xcap::Monitor::from_point(p.x, p.y)
            {
                return Some(m);
            }
        }
        let all = xcap::Monitor::all().ok()?;
        let primary = all.iter().position(|m| m.is_primary().unwrap_or(false));
        all.into_iter().nth(primary.unwrap_or(0))
    }

    pub fn monitor_under_mouse() -> Option<Monitor> {
        let m = current()?;
        // macOS counts in points; the rest of the app wants pixels.
        let scale = if cfg!(target_os = "macos") {
            m.scale_factor().unwrap_or(1.0)
        } else {
            1.0
        };
        Some(Monitor {
            x: m.x().ok()? as f32 * scale,
            y: m.y().ok()? as f32 * scale,
            width: m.width().ok()? as f32 * scale,
            height: m.height().ok()? as f32 * scale,
        })
    }

    pub fn capture_under_mouse() -> Option<(Monitor, image::RgbImage)> {
        let monitor = monitor_under_mouse()?;
        let shot = current()?.capture_image().ok()?;
        let (w, h) = shot.dimensions();
        let rgb: Vec<u8> = shot
            .as_raw()
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let image = image::RgbImage::from_raw(w, h, rgb)?;
        Some((
            Monitor {
                width: w as f32,
                height: h as f32,
                ..monitor
            },
            image,
        ))
    }

    /// egui reports the Windows key itself on these systems.
    pub fn super_down() -> bool {
        false
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    /// Takes a screenshot through the portal (works on X11 too).
    /// cargo test --release portal_screenshot -- --ignored --nocapture
    #[test]
    #[ignore]
    fn portal_screenshot() {
        let image = super::portal::screenshot().unwrap();
        println!("{} x {}", image.width(), image.height());
    }
}
