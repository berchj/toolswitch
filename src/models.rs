// AI models used by some tools. They're not in the app: each is downloaded the first time
// its tool is used and kept in the cache folder.

use std::path::PathBuf;

pub fn dir() -> PathBuf {
    crate::config::cache_dir().join("models")
}

/// The model's file, downloaded from `url` if it isn't there yet. curl comes with Linux,
/// macOS and Windows 10+, so no HTTP library is needed. A download whose SHA-256 isn't
/// `sha256` (cut short, changed upstream, tampered with) is thrown away, never used: the URLs
/// point at fixed versions, so the expected file never changes.
pub fn get(name: &str, url: &str, sha256: &str) -> Result<PathBuf, String> {
    let path = dir().join(name);
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let partial = path.with_extension("part");
    let status = crate::desktop::command("curl")
        .args(["-fsSL", "-o"])
        .arg(&partial)
        .arg(url)
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("Could not download {name}. Are you online?"));
    }
    match file_sha256(&partial) {
        Ok(found) if found == sha256 => {}
        _ => {
            let _ = std::fs::remove_file(&partial);
            return Err(format!("The download of {name} came out wrong. Try again in a while."));
        }
    }
    std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The file's SHA-256 in hex, read in pieces (the models are hundreds of MB).
fn file_sha256(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Loads a model with rten.
pub fn load(name: &str, url: &str, sha256: &str) -> Result<rten::Model, String> {
    let path = get(name, url, sha256)?;
    rten::Model::load_file(&path).map_err(|e| format!("{name}: {e}"))
}

/// glibc keeps freed memory for later instead of returning it to the system; after letting go
/// of a few hundred MB, ask it to return it.
pub fn give_back_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: only tells the allocator to return unused memory; no pointers involved.
        unsafe {
            malloc_trim(0);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sha256_of_a_file() {
        let path = std::env::temp_dir().join("toolswitch-sha256-test.txt");
        std::fs::write(&path, "abc").unwrap();
        // The SHA-256 of "abc" from FIPS 180-2.
        assert_eq!(
            super::file_sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(path);
    }

    /// Downloads a small file with a wrong SHA-256: it must be refused and not kept.
    /// XDG_CACHE_HOME=/tmp/x cargo test --release refuses_a_wrong_download -- --ignored
    #[test]
    #[ignore]
    fn refuses_a_wrong_download() {
        let url = "https://huggingface.co/monkt/paddleocr-onnx/resolve/7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65/languages/latin/dict.txt";
        let wrong = "0".repeat(64);
        assert!(super::get("wrong-test.txt", url, &wrong).is_err());
        assert!(!super::dir().join("wrong-test.txt").exists());
        assert!(!super::dir().join("wrong-test.part").exists());
    }
}
