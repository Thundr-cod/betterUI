// Small shared UI helpers: rectangles, icon glyphs, theme colours.

use windows::core::w;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

use crate::render::Color;

#[derive(Clone, Copy, Default, Debug)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

// Segoe Fluent Icons
pub const ICON_VOLUME_0: &str = "\u{E992}";
pub const ICON_VOLUME_1: &str = "\u{E993}";
pub const ICON_VOLUME_2: &str = "\u{E994}";
pub const ICON_VOLUME_3: &str = "\u{E995}";
pub const ICON_MUTE: &str = "\u{E74F}";
pub const ICON_MIC: &str = "\u{E720}";
pub const ICON_CHECK: &str = "\u{E73E}";

/// Speaker icon matching a volume level (like the Windows tray icon).
pub fn volume_glyph(volume: f32, muted: bool) -> &'static str {
    if muted {
        ICON_MUTE
    } else if volume <= 0.0 {
        ICON_VOLUME_0
    } else if volume < 0.34 {
        ICON_VOLUME_1
    } else if volume < 0.67 {
        ICON_VOLUME_2
    } else {
        ICON_VOLUME_3
    }
}

pub const FG: Color = Color(1.0, 1.0, 1.0, 0.92);
pub const DIM: Color = Color(1.0, 1.0, 1.0, 0.55);
pub const FAINT: Color = Color(1.0, 1.0, 1.0, 0.07);
pub const HOVER: Color = Color(1.0, 1.0, 1.0, 0.12);

/// The Windows accent colour (Settings → Personalization → Colors).
pub fn accent() -> Color {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\DWM"),
            w!("AccentColor"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    if rc.is_err() {
        return Color(0.38, 0.65, 1.0, 1.0);
    }
    // Stored as 0xAABBGGRR.
    let r = (value & 0xFF) as f32 / 255.0;
    let g = ((value >> 8) & 0xFF) as f32 / 255.0;
    let b = ((value >> 16) & 0xFF) as f32 / 255.0;
    // Lighten very dark accents so they stay visible on the dark theme.
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if lum < 0.35 {
        let k = 0.45;
        Color(r + (1.0 - r) * k, g + (1.0 - g) * k, b + (1.0 - b) * k, 1.0)
    } else {
        Color(r, g, b, 1.0)
    }
}
