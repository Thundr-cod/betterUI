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
    fn as_str(self) -> &'static str {
        match self {
            DockMode::Smart => "smart",
            DockMode::AutoHide => "autohide",
            DockMode::Always => "always",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self { bar_opacity: 60, bar_blur: true, dock_enabled: true, dock_mode: DockMode::Smart }
    }
}

fn path() -> std::path::PathBuf {
    crate::log::data_dir().join("config.ini")
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config::default();
        let Ok(text) = fs::read_to_string(path()) else { return c };
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
        c
    }

    /// Load the saved settings, change them, save, and return the result.
    /// Used instead of saving a stale copy, so the bar and the dock never
    /// overwrite each other's settings.
    pub fn update(change: impl FnOnce(&mut Config)) -> Config {
        let mut c = Config::load();
        change(&mut c);
        c.save();
        c
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
             dock_mode = {}\n",
            self.bar_opacity,
            self.bar_blur,
            self.dock_enabled,
            self.dock_mode.as_str()
        );
        if let Err(e) = fs::write(path(), text) {
            crate::log::write(&format!("could not save config: {e}"));
        }
    }
}
