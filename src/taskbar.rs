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
    restore_state();
}

/// Restore, then keep checking for a few seconds and put it back again if
/// Explorer hides it. Quitting with the Start menu open left the taskbar
/// hidden: Start closing a moment later made Explorer re-apply the hidden
/// state betterUI had set. Used when betterUI quits and by the watchdog.
pub fn restore_and_settle() {
    let original = restore_state();
    let mut actions: Vec<&str> = Vec::new();
    for step in 0..15 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if hidden_by_us() {
            // A new betterUI has started and hidden the taskbar again.
            return;
        }
        let state = get_state();
        for hwnd in taskbar_windows() {
            let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
            let parked = parked_off_screen(hwnd);
            if step == 0 || (!visible || parked) {
                crate::log::write(&format!(
                    "taskbar check {step}: visible={visible} off-screen={parked} cloaked={} auto-hide state={state:#x} (original {original:?})",
                    crate::apps::is_cloaked(hwnd)
                ));
            }
            if !visible {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNA);
                }
                actions.push("shown");
            }
            // Left parked below the screen (where auto-hide slides it) although
            // auto-hide is off: switch auto-hide on and off again so Explorer
            // slides it back into place.
            if parked && original.map(|o| o & ABS_AUTOHIDE == 0).unwrap_or(false) {
                let o = original.unwrap_or(0);
                set_state(o | ABS_AUTOHIDE);
                std::thread::sleep(std::time::Duration::from_millis(50));
                set_state(o);
                actions.push("slid back");
            }
        }
        if let Some(original) = original {
            if get_state() != original {
                set_state(original);
                actions.push("setting restored");
            }
        }
    }
    if !actions.is_empty() {
        actions.dedup();
        crate::log::write(&format!("Explorer hid the taskbar again after it was restored; fixed it ({})", actions.join(", ")));
    }
}

/// Is the taskbar window sitting (almost) entirely outside its monitor,
/// where Explorer slides it while auto-hidden?
fn parked_off_screen(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        let w = (r.right.min(m.right) - r.left.max(m.left)).max(0);
        let h = (r.bottom.min(m.bottom) - r.top.max(m.top)).max(0);
        let full = (r.right - r.left).min(r.bottom - r.top).max(1);
        // Auto-hide leaves a 2 px sliver on screen; a shown taskbar is ~48 px.
        w.min(h) < full / 4
    }
}

/// Returns the original auto-hide setting, if one was saved.
fn restore_state() -> Option<u32> {
    let path = state_file();
    let original = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    if let Some(original) = original {
        set_state(original);
    }
    for hwnd in taskbar_windows() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
        }
    }
    let _ = fs::remove_file(&path);
    original
}
