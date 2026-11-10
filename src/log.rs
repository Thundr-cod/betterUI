// Tiny append-only log at %APPDATA%\betterUI\log.txt (kept under ~256 KB).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

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
pub fn save_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

pub fn write(line: &str) {
    let path = data_dir().join("log.txt");
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = fs::remove_file(&path);
        }
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {line}");
    }
}
