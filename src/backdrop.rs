// Blur-behind ("acrylic") for borderless windows.
//
// The documented Windows 11 backdrop API (DWMWA_SYSTEMBACKDROP_TYPE) only
// draws on windows with a normal frame, so on the bar it showed up as solid
// grey. Like TranslucentTB and other taskbar tools, we use the
// SetWindowCompositionAttribute accent policy instead, which works on any
// window on Windows 10 and 11. It is looked up at runtime, so if a future
// Windows removes it the bar simply falls back to no blur.

use std::ffi::c_void;

use windows::core::{s, w};
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_DISABLED: u32 = 0;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32, // AABBGGRR
    animation_id: u32,
}

#[repr(C)]
struct CompositionAttribData {
    attribute: u32,
    data: *mut c_void,
    size: usize,
}

type SetWindowCompositionAttribute =
    unsafe extern "system" fn(HWND, *mut CompositionAttribData) -> BOOL;

pub fn set_blur(hwnd: HWND, enabled: bool) {
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return };
        let Some(proc) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else {
            crate::log::write("blur unavailable on this version of Windows");
            return;
        };
        let set: SetWindowCompositionAttribute = std::mem::transmute(proc);
        let mut policy = AccentPolicy {
            accent_state: if enabled { ACCENT_ENABLE_ACRYLICBLURBEHIND } else { ACCENT_DISABLED },
            accent_flags: 0,
            // Acrylic needs a non-zero alpha here; the visible tint is drawn
            // by the bar itself so the opacity setting controls it.
            gradient_color: 0x0100_0000,
            animation_id: 0,
        };
        let mut data = CompositionAttribData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut policy as *mut AccentPolicy as *mut c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        let _ = set(hwnd, &mut data);
    }
}
