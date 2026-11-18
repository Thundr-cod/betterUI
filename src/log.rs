// Tiny append-only log at %APPDATA%\betterUI\log.txt (kept under ~256 KB).

use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::storage::data_dir;

pub fn write(line: &str) {
    let path = data_dir().join("log.txt");
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            // Keep the previous log for diagnosis instead of deleting it.
            let _ = fs::rename(&path, data_dir().join("log.old.txt"));
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
