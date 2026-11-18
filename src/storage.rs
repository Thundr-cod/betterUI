// Where betterUI keeps its files (%APPDATA%\betterUI), and saving them safely.

use std::fs;
use std::path::{Path, PathBuf};

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("betterUI");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Replace a file's contents in one step: write a temporary file, then move
/// it over the old one, so a crash or power loss mid-write can't leave the
/// file empty.
pub fn save_file(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}
