// Single instance: a second launch (e.g. from a keyboard shortcut or the right-click menu)
// hands its command to the running app and exits. On Linux and macOS through a Unix socket;
// on Windows through a local port, with a random key so only this user's launches get in.

use std::io::{BufRead, BufReader, Write};

pub use imp::{listen, send};

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    /// In a folder only this user can open: other users of the computer can't send commands.
    /// macOS and some Linux sessions have no XDG_RUNTIME_DIR; the cache folder is used then
    /// (never /tmp, which everyone shares).
    fn socket() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(crate::config::cache_dir)
            .join("toolswitch.sock")
    }

    /// Sends `command` to the running app. Returns false if none is running.
    pub fn send(command: &str) -> bool {
        let Ok(mut stream) = UnixStream::connect(socket()) else {
            return false;
        };
        writeln!(stream, "{command}").is_ok()
    }

    /// Becomes the running app: each command another launch sends is passed to `on_command`.
    pub fn listen(on_command: impl Fn(String) + Send + 'static) -> std::io::Result<()> {
        let path = socket();
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            crate::config::private_dir(path.parent().unwrap())?;
        }
        // Left behind by an app that crashed (send() already found nobody listening).
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    on_command(line);
                }
            }
        });
        Ok(())
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::time::Duration;

    /// "port key", written by the running app.
    fn file() -> PathBuf {
        crate::config::cache_dir().join("instance")
    }

    pub fn send(command: &str) -> bool {
        let Ok(text) = std::fs::read_to_string(file()) else {
            return false;
        };
        let Some((port, key)) = text.trim().split_once(' ') else {
            return false;
        };
        let Ok(addr) = format!("127.0.0.1:{port}").parse() else {
            return false;
        };
        let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
            return false;
        };
        // A crashed app leaves its port in the file, and another program may have it now:
        // only toolswitch answers "ok".
        let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
        if writeln!(stream, "{key}\t{command}").is_err() {
            return false;
        }
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).is_ok() && answer.trim() == "ok"
    }

    pub fn listen(on_command: impl Fn(String) + Send + 'static) -> std::io::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let key = random_key();
        std::fs::create_dir_all(crate::config::cache_dir())?;
        std::fs::write(file(), format!("{port} {key}"))?;
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // Something that connects and says nothing doesn't block the next ones.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let Ok(mut answer) = stream.try_clone() else { continue };
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_err() {
                    continue;
                }
                if let Some(command) = line.trim_end().strip_prefix(&format!("{key}\t")) {
                    let _ = writeln!(answer, "ok");
                    on_command(command.to_string());
                }
            }
        });
        Ok(())
    }

    /// Hard to guess for another user: the time, the process and an address from the system.
    fn random_key() -> String {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        hasher.write_u32(std::process::id());
        format!("{:016x}", hasher.finish())
    }
}
