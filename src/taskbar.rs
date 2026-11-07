// Hiding and restoring the native Windows taskbar.
//
// Two things are needed to fully replace it:
//   1. Hide the taskbar windows (Shell_TrayWnd on the primary monitor,
//      Shell_SecondaryTrayWnd on the others).
//   2. Switch the taskbar to auto-hide, otherwise Windows keeps reserving
//      the strip at the bottom and maximized windows won't fill the screen.
//
// The user's original auto-hide setting is saved to a file *before* we change
// anything, so it can be restored even after a crash or power loss
// (`betterui.exe --restore-taskbar`).

use std::fs;
use std::path::PathBuf;

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, ShowWindow, SW_HIDE,
    SW_SHOWNA,
};
use windows::Win32::System::Threading::GetCurrentProcessId;

use crate::appbar::{ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE};

fn state_file() -> PathBuf {
    crate::log::data_dir().join("taskbar_state.txt")
}

/// All of Explorer's taskbar windows (one per monitor).
fn taskbar_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<HWND>);
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf);
        if n > 0 {
            let name = String::from_utf16_lossy(&buf[..n as usize]);
            // Skip betterUI's own tray host window (same class name, see tray.rs).
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if (name == "Shell_TrayWnd" || name == "Shell_SecondaryTrayWnd")
                && pid != GetCurrentProcessId()
            {
                list.push(hwnd);
            }
        }
        TRUE
    }
    let mut list: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut list as *mut Vec<HWND> as isize));
    }
    list
}

fn get_state() -> u32 {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32 }
}

fn set_state(state: u32) {
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        lParam: LPARAM(state as isize),
        ..Default::default()
    };
    if let Some(primary) = taskbar_windows().into_iter().next() {
        abd.hWnd = primary;
    }
    unsafe {
        SHAppBarMessage(ABM_SETSTATE, &mut abd);
    }
}

/// Hide the native taskbar. Safe to call repeatedly.
pub fn hide() {
    let path = state_file();
    let current = get_state();
    if !path.exists() {
        // First hide since the last clean restore: remember the user's setting.
        let _ = fs::write(&path, current.to_string());
    }
    if current & ABS_AUTOHIDE == 0 {
        set_state(current | ABS_AUTOHIDE);
    }
    for hwnd in taskbar_windows() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Explorer sometimes re-shows the taskbar (for example when the mouse
/// touches the bottom edge). Called on a short timer to undo that.
pub fn rehide_if_visible() {
    for hwnd in taskbar_windows() {
        unsafe {
            if IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

/// Is the taskbar currently hidden by betterUI (its original setting saved
/// and not yet restored)?
pub fn hidden_by_us() -> bool {
    state_file().exists()
}

/// Put the taskbar back exactly as it was. Safe to call repeatedly.
pub fn restore() {
    let path = state_file();
    if let Some(original) = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        set_state(original);
    }
    for hwnd in taskbar_windows() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
        }
    }
    let _ = fs::remove_file(&path);
}
