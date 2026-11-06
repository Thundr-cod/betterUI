// Test hooks for the CI feature recordings. When the environment variable
// BETTERUI_TEST_LAYOUT is set, each window writes where its clickable parts
// are (in screen coordinates) to %APPDATA%\betterUI\layout_<name>.txt, so the
// recording script can click real buttons instead of guessing positions.
// Without the variable this does nothing.

use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

use crate::ui::R;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BETTERUI_TEST_LAYOUT").is_some())
}

/// Record `rects` (client coordinates of `hwnd`) under `name`.
pub fn write(name: &str, hwnd: HWND, rects: &[(String, R)]) {
    if !enabled() {
        return;
    }
    let mut origin = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut origin);
    }
    let mut text = String::new();
    for (label, r) in rects {
        text.push_str(&format!(
            "{}|{}|{}|{}|{}\n",
            label,
            origin.left + r.x.round() as i32,
            origin.top + r.y.round() as i32,
            r.w.round() as i32,
            r.h.round() as i32
        ));
    }
    let _ = std::fs::write(crate::log::data_dir().join(format!("layout_{name}.txt")), text);
}
