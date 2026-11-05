// User settings, stored as plain `key = value` lines in
// %APPDATA%\betterUI\config.ini so they can also be edited by hand.

use std::fs;

#[derive(Clone, Copy)]
pub struct Config {
    /// Bar background opacity, 0-100 (%). 100 = solid.
    pub bar_opacity: u32,
    /// Blur whatever is behind the bar (acrylic glass).
    pub bar_blur: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { bar_opacity: 60, bar_blur: true }
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
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let text = format!(
            "# betterUI settings (also changeable by right-clicking the bar)\n\
             # bar_opacity: 0-100, how solid the bar background is\n\
             bar_opacity = {}\n\
             # bar_blur: true/false, blur what is behind the bar\n\
             bar_blur = {}\n",
            self.bar_opacity, self.bar_blur
        );
        if let Err(e) = fs::write(path(), text) {
            crate::log::write(&format!("could not save config: {e}"));
        }
    }
}
