// Shared plumbing for the flyout windows (audio, Bluetooth, control center):
// window creation with the dark blurred look, placement under a bar button,
// rounded corners, and opening Settings pages.

use windows::core::{Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, LoadCursorW, RegisterClassW, IDC_ARROW, SW_SHOWNORMAL, WNDCLASSW, WNDPROC,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

pub fn create(class: PCWSTR, title: PCWSTR, wndproc: WNDPROC) -> Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?.into();
        let wc = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: hinstance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            title,
            WS_POPUP,
            0,
            0,
            100,
            100,
            None,
            None,
            hinstance,
            None,
        )?;
        let dark: BOOL = TRUE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        round_corners(hwnd);
        crate::backdrop::set_blur(hwnd, true);
        Ok(hwnd)
    }
}

/// Work area and DPI of the monitor containing `anchor`.
pub fn monitor_of(anchor: &RECT) -> (RECT, u32) {
    unsafe {
        let hmon = MonitorFromRect(anchor, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(hmon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (mi.rcWork, if dx == 0 { 96 } else { dx })
    }
}

/// Top-left position for a `w` x `h` flyout under `anchor`, right-aligned to
/// it and kept inside the work area.
pub fn place(anchor: &RECT, work: &RECT, w: i32, gap: i32) -> (i32, i32) {
    let x = (anchor.right - w).clamp(work.left + gap, (work.right - w - gap).max(work.left));
    let y = work.top.max(anchor.bottom) + gap;
    (x, y)
}

/// Ask Windows 11 to round the window's corners. Unlike clipping the window
/// to a region, this also rounds the blur behind it. (Windows turns rounding
/// off on machines without a graphics driver, such as CI virtual machines.)
pub fn round_corners(hwnd: HWND) {
    let corners = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const _,
            std::mem::size_of_val(&corners) as u32,
        );
    }
}

/// Open a Settings page or other URI, e.g. "ms-settings:bluetooth".
pub fn open_uri(uri: &str) {
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(uri), None, None, SW_SHOWNORMAL);
    }
}
