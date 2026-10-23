// AI models used by some tools. They're not in the app: each is downloaded the first time
// its tool is used and kept in the cache folder.

use std::path::PathBuf;

pub fn dir() -> PathBuf {
    crate::config::cache_dir().join("models")
}

/// The model's file, downloaded from `url` if it isn't there yet. curl comes with Linux,
/// macOS and Windows 10+, so no HTTP library is needed.
pub fn get(name: &str, url: &str) -> Result<PathBuf, String> {
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
    std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Loads a model with rten.
pub fn load(name: &str, url: &str) -> Result<rten::Model, String> {
    let path = get(name, url)?;
    rten::Model::load_file(&path).map_err(|e| format!("{name}: {e}"))
}
