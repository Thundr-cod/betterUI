// Win32 window plumbing shared by the bar, the dock, the flyouts and the
// tray host: creating and styling windows, monitor and DPI lookups, reading
// mouse messages, painting, right-click menus, and routing messages to a
// window's state.

use std::cell::RefCell;
use std::thread::LocalKey;

use windows::core::{Error, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, ScreenToClient, HMONITOR, MONITORINFO, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetCursorPos, LoadCursorW,
    RegisterClassW, SetForegroundWindow, TrackPopupMenu, HMENU, IDC_ARROW, MF_CHECKED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, MF_UNCHECKED, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_ERASEBKGND,
    WNDCLASSW, WNDPROC, WS_POPUP,
};

/// WM_MOUSEACTIVATE answer: clicking the window doesn't take focus.
pub const MA_NOACTIVATE: isize = 3;
const MONITORINFOF_PRIMARY: u32 = 1;

pub fn register_class(class: PCWSTR, wndproc: WNDPROC) -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_win32());
        }
    }
    Ok(())
}

/// A borderless popup window of a class made with `register_class`.
pub fn create(class: PCWSTR, title: PCWSTR, ex_style: WINDOW_EX_STYLE, x: i32, y: i32, w: i32, h: i32) -> Result<HWND> {
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None)?.into();
        CreateWindowExW(ex_style, class, title, WS_POPUP, x, y, w, h, None, None, hinstance, None)
    }
}

/// Dark mode, the given corner style, and per-pixel transparency (the blur
/// itself is set up in backdrop.rs). Rounded corners also round the blur
/// behind the window; Windows turns rounding off on machines without a
/// graphics driver.
pub fn style(hwnd: HWND, corners: DWM_WINDOW_CORNER_PREFERENCE) {
    unsafe {
        let dark: BOOL = TRUE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const _,
            std::mem::size_of_val(&corners) as u32,
        );
        let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Monitor {
    /// The whole monitor, and the part not taken by app bars.
    pub rect: RECT,
    pub work: RECT,
    pub dpi: u32,
    pub primary: bool,
}

pub fn monitor_info(hmon: HMONITOR) -> Option<Monitor> {
    unsafe {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return None;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        Some(Monitor {
            rect: mi.rcMonitor,
            work: mi.rcWork,
            dpi: if dx == 0 { 96 } else { dx },
            primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
        })
    }
}

pub fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// Answer WM_PAINT: run `draw` between BeginPaint and EndPaint and log a
/// failure (the next repaint recreates the render target if the GPU reset).
pub fn paint(hwnd: HWND, what: &str, draw: impl FnOnce() -> Result<()>) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        BeginPaint(hwnd, &mut ps);
        if let Err(e) = draw() {
            crate::log::write(&format!("{what} paint failed: {e:?}"));
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

/// The (signed) x, y packed into a mouse message's lParam.
pub fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as u16 as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
}

/// The client-area point of a mouse message, as drawing units.
pub fn client_point(lp: LPARAM) -> (f32, f32) {
    let (x, y) = point(lp);
    (x as f32, y as f32)
}

/// WM_MOUSEWHEEL carries screen coordinates: the point in `hwnd`'s client area.
pub fn wheel_point(hwnd: HWND, lp: LPARAM) -> (f32, f32) {
    let (x, y) = point(lp);
    let mut pt = POINT { x, y };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut pt);
    }
    (pt.x as f32, pt.y as f32)
}

/// Wheel notches turned (positive = away from the user).
pub fn wheel_notches(wp: WPARAM) -> f32 {
    ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as f32 / 120.0
}

/// Window procedure body for a window whose state lives in a thread-local:
/// hand the message to `handle`, or to Windows if `handle` passes on it or
/// the state is busy (a re-entrant message while it's being handled).
pub fn dispatch<T: 'static>(
    state: &'static LocalKey<RefCell<Option<T>>>,
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    handle: impl FnOnce(&mut T, u32, WPARAM, LPARAM) -> Option<LRESULT>,
) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    let handled = state.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|s| handle(s, msg, wp, lp)),
        Err(_) => None,
    });
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

/// Run `f` on a window's thread-local state, if it exists and isn't busy.
pub fn with_state<T: 'static, R>(state: &'static LocalKey<RefCell<Option<T>>>, f: impl FnOnce(&mut T) -> R) -> Option<R> {
    state.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        let result = guard.as_mut().map(f);
        result
    })
}

/// A right-click menu. Destroyed (with its submenus) when dropped.
pub struct Menu(HMENU);

impl Menu {
    pub fn new() -> Option<Menu> {
        unsafe { CreatePopupMenu().ok().map(Menu) }
    }

    pub fn item(&self, id: usize, label: &str) {
        unsafe {
            let _ = AppendMenuW(self.0, MF_STRING, id, &HSTRING::from(label));
        }
    }

    pub fn check(&self, id: usize, label: &str, checked: bool) {
        let check = if checked { MF_CHECKED } else { MF_UNCHECKED };
        unsafe {
            let _ = AppendMenuW(self.0, MF_STRING | check, id, &HSTRING::from(label));
        }
    }

    pub fn separator(&self) {
        unsafe {
            let _ = AppendMenuW(self.0, MF_SEPARATOR, 0, PCWSTR::null());
        }
    }

    pub fn submenu(&self, label: &str, sub: Menu) {
        unsafe {
            let _ = AppendMenuW(self.0, MF_POPUP, sub.0 .0 as usize, &HSTRING::from(label));
        }
        // Now owned by this menu, which destroys it.
        std::mem::forget(sub);
    }

    /// Show the menu at the mouse and wait for a choice: the chosen item's
    /// id, or 0 if the menu was dismissed.
    pub fn track(self, owner: HWND) -> usize {
        unsafe {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(owner);
            TrackPopupMenu(self.0, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, owner, None).0 as usize
        }
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}
