// The system tray dropdown: a grid of the tray icons collected by tray.rs.
// Left-click and right-click are passed on to the app exactly as the
// Windows taskbar would, so each app opens its own window or menu. Hovering
// an icon shows its tooltip text along the bottom.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{BeginPaint, ClientToScreen, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, IsWindowVisible, PostMessageW, SetForegroundWindow, SetWindowPos, ShowWindow,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SW_HIDE, SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_APP,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP,
};

use crate::icons::Icons;
use crate::render::{Color, Gfx, TEXT_FONT};
use crate::tray::{self, TrayIcon};
use crate::ui::{R, DIM, FAINT, FG};
use crate::{log, popup};

const COLUMNS: usize = 5;
const CELL: f32 = 44.0; // logical pixels
const ICON: f32 = 20.0;
const WM_TRAY_CHANGED: u32 = WM_APP + 20;

struct TrayFlyout {
    hwnd: HWND,
    gfx: Gfx,
    font: IDWriteTextFormat,
    dpi: u32,
    icons_wic: Option<Icons>,
    bitmaps: HashMap<isize, Option<ID2D1Bitmap>>,
    bitmap_generation: u64,
    items: Vec<TrayIcon>,
    cells: Vec<R>,
    hovered: Option<usize>,
    opacity: f32,
    width: i32,
    height: i32,
}

thread_local! {
    static FLYOUT: RefCell<Option<TrayFlyout>> = const { RefCell::new(None) };
}

pub fn toggle(anchor: RECT, bar_opacity: u32, force_open: bool) {
    if let Err(e) = ensure_created() {
        log::write(&format!("tray flyout could not be created: {e:?}"));
        return;
    }
    FLYOUT.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else { return };
        let Some(f) = guard.as_mut() else { return };
        if unsafe { IsWindowVisible(f.hwnd).as_bool() } && !force_open {
            f.hide();
        } else {
            f.opacity = (bar_opacity.max(70) as f32) / 100.0;
            f.show(anchor);
        }
    });
}

pub fn hide() {
    FLYOUT.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(f) = guard.as_mut() {
                f.hide();
            }
        }
    });
}

/// Called by the tray host when icons are added, changed or removed.
pub fn changed() {
    FLYOUT.with(|cell| {
        if let Ok(guard) = cell.try_borrow() {
            if let Some(f) = guard.as_ref() {
                // Handle it from the message loop, not from inside the tray
                // message we're currently answering.
                unsafe {
                    let _ = PostMessageW(f.hwnd, WM_TRAY_CHANGED, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

fn ensure_created() -> Result<()> {
    if FLYOUT.with(|c| c.borrow().is_some()) {
        return Ok(());
    }
    let hwnd = popup::create(w!("betterUI.TrayFlyout"), w!("betterUI tray"), Some(wndproc))?;
    let gfx = Gfx::new()?;
    let font = gfx.text_format(TEXT_FONT, 12.0, 400)?;
    let f = TrayFlyout {
        hwnd,
        gfx,
        font,
        dpi: 96,
        icons_wic: Icons::new().ok(),
        bitmaps: HashMap::new(),
        bitmap_generation: 0,
        items: Vec::new(),
        cells: Vec::new(),
        hovered: None,
        opacity: 0.85,
        width: 0,
        height: 0,
    };
    FLYOUT.with(|c| *c.borrow_mut() = Some(f));
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    let handled = FLYOUT.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|f| f.handle(msg, wp, lp)),
        Err(_) => None,
    });
    handled.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
}

fn point_from(lp: LPARAM) -> (f32, f32) {
    (
        (lp.0 & 0xFFFF) as u16 as i16 as f32,
        ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as f32,
    )
}

impl TrayFlyout {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn pad(&self) -> f32 {
        10.0 * self.scale()
    }

    fn tip_height(&self) -> f32 {
        28.0 * self.scale()
    }

    fn size_for(&self, count: usize) -> (i32, i32) {
        let s = self.scale();
        let rows = count.div_ceil(COLUMNS).max(1) as f32;
        let w = 2.0 * self.pad() + COLUMNS as f32 * CELL * s;
        let h = 2.0 * self.pad() + rows * CELL * s + self.tip_height();
        (w.round() as i32, h.round() as i32)
    }

    fn show(&mut self, anchor: RECT) {
        let (work, dpi) = popup::monitor_of(&anchor);
        if dpi != self.dpi {
            self.dpi = dpi;
            if let Ok(f) = self.gfx.text_format(TEXT_FONT, 12.0 * self.scale(), 400) {
                self.font = f;
            }
            self.bitmaps.clear();
        }
        self.items = tray::icons();
        self.hovered = None;
        let (w, h) = self.size_for(self.items.len());
        self.width = w;
        self.height = h;
        let (x, y) = popup::place(&anchor, &work, w, (6.0 * self.scale()).round() as i32);
        unsafe {
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
            self.gfx.resize(w as u32, h as u32);
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.invalidate();
    }

    fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(self.hwnd, &mut ps);
                    if let Err(e) = self.paint() {
                        log::write(&format!("tray paint failed: {e:?}"));
                    }
                    let _ = EndPaint(self.hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_TRAY_CHANGED => {
                if unsafe { IsWindowVisible(self.hwnd).as_bool() } {
                    self.items = tray::icons();
                    let (w, h) = self.size_for(self.items.len());
                    if (w, h) != (self.width, self.height) {
                        self.width = w;
                        self.height = h;
                        unsafe {
                            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, w, h, SWP_NOACTIVATE | SWP_NOMOVE);
                        }
                        self.gfx.resize(w as u32, h as u32);
                    }
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    self.hide();
                }
                None
            }
            WM_KEYDOWN => {
                if wp.0 as u16 == VK_ESCAPE.0 {
                    self.hide();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let (x, y) = point_from(lp);
                let hovered = self.cells.iter().position(|r| r.contains(x, y));
                if hovered != self.hovered {
                    self.hovered = hovered;
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                let (x, y) = point_from(lp);
                if let Some(i) = self.cells.iter().position(|r| r.contains(x, y)) {
                    if let Some(icon) = self.items.get(i).cloned() {
                        let r = self.cells[i];
                        let mut pt = windows::Win32::Foundation::POINT {
                            x: (r.x + r.w / 2.0) as i32,
                            y: (r.y + r.h / 2.0) as i32,
                        };
                        unsafe {
                            let _ = ClientToScreen(self.hwnd, &mut pt);
                        }
                        // Close first so the app's own menu or window gets focus.
                        self.hide();
                        tray::click(&icon, msg == WM_RBUTTONUP, pt.x, pt.y);
                    }
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn paint(&mut self) -> Result<()> {
        if self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        let s = self.scale();
        let pad = self.pad();
        let frame = self.gfx.begin(self.hwnd, self.width as u32, self.height as u32)?;
        if self.gfx.generation != self.bitmap_generation {
            self.bitmaps.clear();
            self.bitmap_generation = self.gfx.generation;
        }
        frame.round_rect(0.0, 0.0, self.width as f32, self.height as f32, 0.0, Color(0.10, 0.10, 0.12, self.opacity));

        let cell = CELL * s;
        let icon = (ICON * s).round();
        let mut cells = Vec::with_capacity(self.items.len());
        for i in 0..self.items.len() {
            let col = (i % COLUMNS) as f32;
            let row = (i / COLUMNS) as f32;
            let r = R::new(pad + col * cell, pad + row * cell, cell, cell);
            if self.hovered == Some(i) {
                frame.round_rect(r.x + 2.0 * s, r.y + 2.0 * s, r.w - 4.0 * s, r.h - 4.0 * s, 6.0 * s, FAINT);
            }
            let handle = self.items[i].icon.map(|h| h.0 as isize).unwrap_or(0);
            let bmp = match self.bitmaps.get(&handle) {
                Some(b) => b.clone(),
                None => {
                    let made = self.items[i]
                        .icon
                        .and_then(|h| self.icons_wic.as_ref().and_then(|w| w.for_hicon(h)))
                        .and_then(|src| frame.bitmap(&src).ok());
                    self.bitmaps.insert(handle, made.clone());
                    made
                }
            };
            if let Some(b) = bmp {
                frame.draw_bitmap(&b, r.x + (r.w - icon) / 2.0, r.y + (r.h - icon) / 2.0, icon, icon, 1.0);
            }
            cells.push(r);
        }

        // Tooltip line
        let tip_y = self.height as f32 - pad - self.tip_height();
        let text = match self.hovered.and_then(|i| self.items.get(i)) {
            Some(icon) if !icon.tip.is_empty() => (icon.tip.lines().next().unwrap_or("").to_string(), FG),
            Some(_) => (String::new(), DIM),
            None if self.items.is_empty() => ("No tray icons".to_string(), DIM),
            None => ("Click: open · Right-click: menu".to_string(), DIM),
        };
        if !text.0.is_empty() {
            let t = self.gfx.text_fit(&text.0, &self.font, self.width as f32 - 2.0 * pad - 8.0 * s, self.tip_height())?;
            frame.text(&t, pad + 4.0 * s, tip_y + 4.0 * s, text.1);
        }

        let result = self.gfx.end(frame);
        crate::testhooks::write(
            "tray",
            self.hwnd,
            &cells.iter().enumerate().map(|(i, r)| (format!("Icon({i})"), *r)).collect::<Vec<_>>(),
        );
        self.cells = cells;
        // Keep the bitmap cache from growing as icons change.
        if self.bitmaps.len() > 200 {
            self.bitmaps.clear();
        }
        result
    }
}
