// User settings, stored as plain `key = value` lines in
// %APPDATA%\betterUI\config.ini so they can also be edited by hand.

use std::fs;

#[derive(Clone, Copy)]
pub struct Config {
    /// Bar background opacity, 0-100 (%). 100 = solid.
    pub bar_opacity: u32,
    /// Blur whatever is behind the bar (acrylic glass).
    pub bar_blur: bool,
    /// Show the dock at the bottom of the screen.
    pub dock_enabled: bool,
    pub dock_mode: DockMode,
    /// A Start button at the left end of the dock.
    pub dock_start: bool,
    /// When the top bar hides (same choices as the dock; default: always shown).
    pub bar_mode: DockMode,
    /// Collect tray icons for the bar's tray dropdown.
    pub tray_enabled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DockMode {
    /// Visible unless a window overlaps it; mouse at the bottom edge brings it back.
    Smart,
    /// Hidden until the mouse touches the bottom edge.
    AutoHide,
    /// Always visible.
    Always,
}

impl DockMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DockMode::Smart => "smart",
            DockMode::AutoHide => "autohide",
            DockMode::Always => "always",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self { bar_opacity: 60, bar_blur: true, dock_enabled: true, dock_mode: DockMode::Smart, dock_start: true, bar_mode: DockMode::Always, tray_enabled: true }
    }
}

fn path() -> std::path::PathBuf {
    crate::log::data_dir().join("config.ini")
}

impl Config {
    pub fn load() -> Self {
        Self::load_checked().unwrap_or_default()
    }

    /// None when config.ini exists but can't be read (locked, bad encoding),
    /// so callers don't save defaults over the user's settings.
    pub fn load_checked() -> Option<Self> {
        let mut c = Config::default();
        let text = match fs::read_to_string(path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(c),
            Err(e) => {
                crate::log::write(&format!("could not read config.ini: {e}"));
                return None;
            }
        };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "bar_opacity" => {
                    if let Ok(v) = value.trim_end_matches('%').parse::<u32>() {
                        c.bar_opacity = v.min(100);
                    }
                }
                "bar_blur" => c.bar_blur = matches!(value, "true" | "on" | "yes" | "1"),
                "dock_enabled" => c.dock_enabled = matches!(value, "true" | "on" | "yes" | "1"),
                "dock_start" => c.dock_start = matches!(value, "true" | "on" | "yes" | "1"),
                "tray_enabled" => c.tray_enabled = matches!(value, "true" | "on" | "yes" | "1"),
                "bar_mode" => {
                    c.bar_mode = match value {
                        "autohide" => DockMode::AutoHide,
                        "smart" => DockMode::Smart,
                        _ => DockMode::Always,
                    }
                }
                "dock_mode" => {
                    c.dock_mode = match value {
                        "autohide" => DockMode::AutoHide,
                        "always" => DockMode::Always,
                        _ => DockMode::Smart,
                    }
                }
                _ => {}
            }
        }
        Some(c)
    }

    /// Load the saved settings, change them, save, and return the result.
    /// Used instead of saving a stale copy, so the bar and the dock never
    /// overwrite each other's settings.
    /// Callers keep their own copy up to date; an unreadable file is left as
    /// it is rather than overwritten with defaults.
    pub fn update(change: impl FnOnce(&mut Config)) {
        let Some(mut c) = Config::load_checked() else { return };
        change(&mut c);
        c.save();
    }

    pub fn save(&self) {
        let text = format!(
            "# betterUI settings (also changeable by right-clicking the bar)\n\
             # bar_opacity: 0-100, how solid the bar background is\n\
             bar_opacity = {}\n\
             # bar_blur: true/false, blur what is behind the bar\n\
             bar_blur = {}\n\
             # dock_enabled: true/false\n\
             dock_enabled = {}\n\
             # dock_mode: smart (hide when a window covers it), autohide, always\n\
             dock_mode = {}\n\
             # dock_start: true/false, a Start button at the left end of the dock\n\
             dock_start = {}\n\
             # bar_mode: always (keeps its strip), autohide, smart (hide when a window covers it)\n\
             bar_mode = {}\n\
             # tray_enabled: true/false, show the system tray dropdown on the bar\n\
             tray_enabled = {}\n",
            self.bar_opacity,
            self.bar_blur,
            self.dock_enabled,
            self.dock_mode.as_str(),
            self.dock_start,
            self.bar_mode.as_str(),
            self.tray_enabled
        );
        if let Err(e) = crate::log::save_file(&path(), &text) {
            crate::log::write(&format!("could not save config: {e}"));
        }
    }
}
