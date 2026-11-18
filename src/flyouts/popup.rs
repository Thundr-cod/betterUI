// What every flyout window has in common: a dark, blurred, rounded popup
// that opens under a bar button, follows that monitor's DPI, closes when the
// user clicks elsewhere or presses Esc, and optionally refreshes on a timer
// while open.

use windows::core::{Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DWMWCP_ROUND;
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindowVisible, KillTimer, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SW_HIDE, SW_SHOW, SW_SHOWNORMAL, WA_INACTIVE, WM_ACTIVATE, WM_KEYDOWN, WNDPROC,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

use crate::ui::render::{Frame, Gfx};
use crate::ui::{backdrop, panel_bg, window};

pub struct Popup {
    pub hwnd: HWND,
    pub gfx: Gfx,
    pub dpi: u32,
    pub opacity: f32,
    pub width: i32,
    pub height: i32,
    /// Timer id and interval to run while open.
    refresh: Option<(usize, u32)>,
}

impl Popup {
    pub fn new(class: PCWSTR, title: PCWSTR, wndproc: WNDPROC, refresh: Option<(usize, u32)>) -> Result<Self> {
        window::register_class(class, wndproc)?;
        let hwnd = window::create(class, title, WS_EX_TOOLWINDOW | WS_EX_TOPMOST, 0, 0, 100, 100)?;
        window::style(hwnd, DWMWCP_ROUND);
        backdrop::set_blur(hwnd, true);
        Ok(Self { hwnd, gfx: Gfx::new()?, dpi: 96, opacity: 0.85, width: 0, height: 0, refresh })
    }

    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    /// Flyouts follow the bar's opacity, but never get too see-through to read.
    pub fn set_opacity(&mut self, bar_opacity: u32) {
        self.opacity = bar_opacity.max(70) as f32 / 100.0;
    }

    /// The work area of the monitor under `anchor`, and whether its DPI
    /// differs from last time (the caller then remakes its fonts).
    pub fn locate(&mut self, anchor: &RECT) -> (RECT, bool) {
        let m = window::monitor_info(unsafe { MonitorFromRect(anchor, MONITOR_DEFAULTTOPRIMARY) }).unwrap_or_default();
        let dpi = if m.dpi == 0 { 96 } else { m.dpi };
        let changed = dpi != self.dpi;
        self.dpi = dpi;
        (m.work, changed)
    }

    /// Open at size `w` x `h` under `anchor`, right-aligned to it and kept
    /// inside `work`.
    pub fn show(&mut self, anchor: &RECT, work: &RECT, w: i32, h: i32) {
        self.width = w;
        self.height = h;
        let (x, y) = place(anchor, work, w, (6.0 * self.scale()).round() as i32);
        // Something may have minimized it while closed; SW_SHOW would keep it so.
        window::unminimize(self.hwnd);
        unsafe {
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
            self.gfx.resize(w as u32, h as u32);
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            if let Some((id, ms)) = self.refresh {
                SetTimer(self.hwnd, id, ms, None);
            }
        }
        self.invalidate();
    }

    pub fn hide(&mut self) {
        unsafe {
            if let Some((id, _)) = self.refresh {
                let _ = KillTimer(self.hwnd, id);
            }
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Change size in place (the content grew or shrank).
    pub fn resize(&mut self, w: i32, h: i32) {
        if (w, h) == (self.width, self.height) {
            return;
        }
        self.width = w;
        self.height = h;
        unsafe {
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, w, h, SWP_NOACTIVATE | SWP_NOMOVE);
        }
        self.gfx.resize(w as u32, h as u32);
    }

    pub fn invalidate(&self) {
        window::invalidate(self.hwnd);
    }

    /// Start a frame with the flyout's background drawn. None while the
    /// window has no size yet.
    pub fn begin(&mut self) -> Option<Result<Frame>> {
        if self.width <= 0 || self.height <= 0 {
            return None;
        }
        let frame = match self.gfx.begin(self.hwnd, self.width as u32, self.height as u32) {
            Ok(f) => f,
            Err(e) => return Some(Err(e)),
        };
        frame.round_rect(0.0, 0.0, self.width as f32, self.height as f32, 0.0, panel_bg(self.opacity));
        Some(Ok(frame))
    }
}

/// Did the user click elsewhere or press Esc? Then the flyout closes.
pub fn wants_close(msg: u32, wp: WPARAM) -> bool {
    match msg {
        WM_ACTIVATE => (wp.0 & 0xFFFF) as u32 == WA_INACTIVE,
        WM_KEYDOWN => wp.0 as u16 == VK_ESCAPE.0,
        _ => false,
    }
}

/// Top-left position for a `w`-wide flyout under `anchor`, right-aligned to
/// it and kept inside the work area.
pub fn place(anchor: &RECT, work: &RECT, w: i32, gap: i32) -> (i32, i32) {
    let x = (anchor.right - w).clamp(work.left + gap, (work.right - w - gap).max(work.left));
    let y = work.top.max(anchor.bottom) + gap;
    (x, y)
}

/// Open a Settings page or other URI, e.g. "ms-settings:bluetooth".
pub fn open_uri(uri: &str) {
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(uri), None, None, SW_SHOWNORMAL);
    }
}
