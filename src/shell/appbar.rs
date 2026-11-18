// Registering the bar as a Windows "app bar" so Windows reserves screen space
// for it: maximized windows stop below the bar instead of going under it.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA};

// Constants from shellapi.h (declared here so their types are exact).
pub const ABM_NEW: u32 = 0x0;
pub const ABM_REMOVE: u32 = 0x1;
pub const ABM_QUERYPOS: u32 = 0x2;
pub const ABM_SETPOS: u32 = 0x3;
pub const ABM_GETSTATE: u32 = 0x4;
pub const ABM_ACTIVATE: u32 = 0x6;
pub const ABM_WINDOWPOSCHANGED: u32 = 0x9;
pub const ABM_SETSTATE: u32 = 0xA;

pub const ABN_POSCHANGED: usize = 0x1;
pub const ABN_FULLSCREENAPP: usize = 0x2;

pub const ABE_TOP: u32 = 1;
pub const ABS_AUTOHIDE: u32 = 0x1;

fn data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        ..Default::default()
    }
}

pub fn register(hwnd: HWND, callback_msg: u32) {
    let mut abd = data(hwnd);
    abd.uCallbackMessage = callback_msg;
    unsafe {
        SHAppBarMessage(ABM_NEW, &mut abd);
    }
}

pub fn unregister(hwnd: HWND) {
    let mut abd = data(hwnd);
    unsafe {
        SHAppBarMessage(ABM_REMOVE, &mut abd);
    }
}

/// Reserve a strip `height` pixels tall at the top of `monitor`.
/// Returns the rectangle the bar window should occupy.
pub fn reserve_top(hwnd: HWND, monitor: RECT, height: i32) -> RECT {
    let mut abd = data(hwnd);
    abd.uEdge = ABE_TOP;
    abd.rc = RECT {
        left: monitor.left,
        top: monitor.top,
        right: monitor.right,
        bottom: monitor.top + height,
    };
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.bottom = abd.rc.top + height;
        SHAppBarMessage(ABM_SETPOS, &mut abd);
    }
    abd.rc
}

pub fn activated(hwnd: HWND) {
    let mut abd = data(hwnd);
    abd.lParam = LPARAM(0);
    unsafe {
        SHAppBarMessage(ABM_ACTIVATE, &mut abd);
    }
}

pub fn moved(hwnd: HWND) {
    let mut abd = data(hwnd);
    unsafe {
        SHAppBarMessage(ABM_WINDOWPOSCHANGED, &mut abd);
    }
}
