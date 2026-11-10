//! Keep the Start menu at one fixed spot above the dock.
//!
//! Windows places Start above its own (hidden) taskbar, over the dock, and
//! puts it back there on its own each time it opens. A WinEvent hook (no code
//! runs inside Explorer) hears when Start's window is shown or moved and sets
//! it straight back to the fixed spot, so there's no polling.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindowRect, GetWindowThreadProcessId, SetWindowPos, GA_ROOT, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
    OBJID_WINDOW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, WINEVENT_OUTOFCONTEXT,
};

use crate::{log, procinfo};

thread_local! {
    /// (bottom edge Start's window must end at, monitor left, monitor right).
    static TARGET: Cell<Option<(i32, i32, i32)>> = const { Cell::new(None) };
    static HOOKS: RefCell<Vec<HWINEVENTHOOK>> = const { RefCell::new(Vec::new()) };
    /// window -> does it belong to the process that draws Start.
    static HOSTS: RefCell<HashMap<isize, bool>> = RefCell::new(HashMap::new());
    /// Moves since the foreground last changed, so a tug-of-war with Windows can't run forever.
    static MOVES: Cell<u32> = const { Cell::new(0) };
}
const MAX_MOVES: u32 = 10;

/// Start listening (on the UI thread, which pumps messages).
pub fn install() {
    HOOKS.with(|h| {
        let mut h = h.borrow_mut();
        if !h.is_empty() {
            return;
        }
        for event in [EVENT_SYSTEM_FOREGROUND, EVENT_OBJECT_LOCATIONCHANGE] {
            let hook = unsafe { SetWinEventHook(event, event, HMODULE::default(), Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT) };
            if hook.is_invalid() {
                log::write("Start menu: could not listen for window events");
            } else {
                h.push(hook);
            }
        }
    });
}

pub fn uninstall() {
    HOOKS.with(|h| {
        for hook in h.borrow_mut().drain(..) {
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
    });
    TARGET.with(|t| t.set(None));
}

/// Where Start's window must end: `bottom` (screen y) on the monitor spanning
/// `left..right`. Called whenever the dock is placed.
pub fn set_target(bottom: i32, left: i32, right: i32) {
    TARGET.with(|t| t.set(Some((bottom, left, right))));
}

/// Newer Windows 11 builds draw Start in SearchHost.exe (seen on the real
/// PC), older ones in StartMenuExperienceHost.exe.
fn is_start_host(exe: &str) -> bool {
    let exe = exe.to_lowercase();
    exe.ends_with("\\searchhost.exe") || exe.ends_with("\\startmenuexperiencehost.exe")
}

unsafe extern "system" fn on_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    if event == EVENT_SYSTEM_FOREGROUND {
        MOVES.with(|m| m.set(0));
    }
    // Top-level windows only: a child's position is relative to its parent,
    // so moving it with screen coordinates would push it out of Start.
    if hwnd.is_invalid() || object != OBJID_WINDOW.0 || child != 0 || GetAncestor(hwnd, GA_ROOT) != hwnd {
        return;
    }
    let Some((bottom, left, right)) = TARGET.with(|t| t.get()) else { return };
    // Keyed by window, not process id: ids get reused, so a restarted
    // SearchHost could inherit a cached "not Start".
    let host = HOSTS.with(|h| {
        let mut h = h.borrow_mut();
        if h.len() > 512 {
            h.clear(); // window handles get reused too; a fresh look now and then is cheap
        }
        *h.entry(hwnd.0 as isize).or_insert_with(|| {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            procinfo::process_path(pid).map(|p| is_start_host(&p)).unwrap_or(false)
        })
    });
    if !host {
        return;
    }
    let mut r = RECT::default();
    if GetWindowRect(hwnd, &mut r).is_err() || r.bottom == bottom {
        return;
    }
    // Only the Start panel: not SearchHost's full-screen windows, nor one on another monitor.
    let width = r.right - r.left;
    if width <= 0 || width * 2 > right - left || r.left < left || r.right > right {
        return;
    }
    let tries = MOVES.with(|m| {
        m.set(m.get() + 1);
        m.get()
    });
    if tries > MAX_MOVES {
        if tries == MAX_MOVES + 1 {
            log::write("Start menu: Windows keeps moving it back; leaving it until Start opens again");
        }
        return;
    }
    let moved = SetWindowPos(hwnd, HWND::default(), r.left, bottom - (r.bottom - r.top), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    log::write(&format!(
        "Start menu: {},{}-{},{} set to end at {}: {}",
        r.left,
        r.top,
        r.right,
        r.bottom,
        bottom,
        if moved.is_ok() { "ok".to_string() } else { format!("{moved:?}") }
    ));
}

#[cfg(test)]
mod tests {
    use super::is_start_host;

    #[test]
    fn start_hosts() {
        assert!(is_start_host("C:\\Windows\\SystemApps\\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\\SearchHost.exe"));
        assert!(is_start_host("c:\\windows\\systemapps\\x\\startmenuexperiencehost.exe"));
        assert!(!is_start_host("c:\\windows\\explorer.exe"));
    }
}
