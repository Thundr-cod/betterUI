// The system tray dropdown: a grid of the tray icons collected by
// shell/tray.rs. Left-click and right-click are passed on to the app exactly
// as the Windows taskbar would, so each app opens its own window or menu.
// Hovering an icon shows its tooltip text along the bottom.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, WM_ACTIVATE, WM_APP, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP,
};

use super::popup::{self, Popup};
use crate::log;
use crate::shell::tray::{self, TrayIcon};
use crate::ui::icons::Icons;
use crate::ui::render::TEXT_FONT;
use crate::ui::{window, R, DIM, FAINT, FG};

const COLUMNS: usize = 5;
const CELL: f32 = 44.0; // logical pixels
const ICON: f32 = 20.0;
const WM_TRAY_CHANGED: u32 = WM_APP + 20;

struct TrayFlyout {
    popup: Popup,
    font: IDWriteTextFormat,
    icons_wic: Option<Icons>,
    bitmaps: HashMap<isize, Option<ID2D1Bitmap>>,
    bitmap_generation: u64,
    items: Vec<TrayIcon>,
    cells: Vec<R>,
    hovered: Option<usize>,
}

thread_local! {
    static FLYOUT: RefCell<Option<TrayFlyout>> = const { RefCell::new(None) };
}

pub fn toggle(anchor: RECT, bar_opacity: u32) {
    if let Err(e) = ensure_created() {
        log::write(&format!("tray flyout could not be created: {e:?}"));
        return;
    }
    window::with_state(&FLYOUT, |f| {
        if f.popup.visible() {
            f.popup.hide();
        } else {
            f.popup.set_opacity(bar_opacity);
            f.show(anchor);
        }
    });
}

pub fn hide() {
    window::with_state(&FLYOUT, |f| f.popup.hide());
}

/// Called by the tray host when icons are added, changed or removed.
pub fn changed() {
    FLYOUT.with(|cell| {
        if let Ok(guard) = cell.try_borrow() {
            if let Some(f) = guard.as_ref() {
                // Handle it from the message loop, not from inside the tray
                // message we're currently answering.
                unsafe {
                    let _ = PostMessageW(f.popup.hwnd, WM_TRAY_CHANGED, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

fn ensure_created() -> Result<()> {
    if FLYOUT.with(|c| c.borrow().is_some()) {
        return Ok(());
    }
    let popup = Popup::new(w!("betterUI.TrayFlyout"), w!("betterUI tray"), Some(wndproc), None)?;
    let font = popup.gfx.text_format(TEXT_FONT, 12.0, 400)?;
    let f = TrayFlyout {
        popup,
        font,
        icons_wic: Icons::new().ok(),
        bitmaps: HashMap::new(),
        bitmap_generation: 0,
        items: Vec::new(),
        cells: Vec::new(),
        hovered: None,
    };
    FLYOUT.with(|c| *c.borrow_mut() = Some(f));
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    window::dispatch(&FLYOUT, hwnd, msg, wp, lp, TrayFlyout::handle)
}

impl TrayFlyout {
    fn scale(&self) -> f32 {
        self.popup.scale()
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

    /// Re-read the icons. Keyed by icon handle, which Windows reuses once an
    /// app replaces its icon: start fresh so a reused handle can't show the
    /// old picture.
    fn reload(&mut self) {
        self.bitmaps.clear();
        self.items = tray::icons();
    }

    fn show(&mut self, anchor: RECT) {
        let (work, dpi_changed) = self.popup.locate(&anchor);
        if dpi_changed {
            if let Ok(f) = self.popup.gfx.text_format(TEXT_FONT, 12.0 * self.scale(), 400) {
                self.font = f;
            }
        }
        self.reload();
        self.hovered = None;
        let (w, h) = self.size_for(self.items.len());
        self.popup.show(&anchor, &work, w, h);
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                let hwnd = self.popup.hwnd;
                window::paint(hwnd, "tray", || self.paint());
                Some(LRESULT(0))
            }
            m if popup::wants_close(m, wp) => {
                self.popup.hide();
                if m == WM_ACTIVATE { None } else { Some(LRESULT(0)) }
            }
            WM_TRAY_CHANGED => {
                if self.popup.visible() {
                    self.reload();
                    let (w, h) = self.size_for(self.items.len());
                    self.popup.resize(w, h);
                    self.popup.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let (x, y) = window::client_point(lp);
                let hovered = self.cells.iter().position(|r| r.contains(x, y));
                if hovered != self.hovered {
                    self.hovered = hovered;
                    self.popup.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                let (x, y) = window::client_point(lp);
                if let Some(i) = self.cells.iter().position(|r| r.contains(x, y)) {
                    if let Some(icon) = self.items.get(i).cloned() {
                        let r = self.cells[i];
                        let mut pt = POINT { x: (r.x + r.w / 2.0) as i32, y: (r.y + r.h / 2.0) as i32 };
                        unsafe {
                            let _ = ClientToScreen(self.popup.hwnd, &mut pt);
                        }
                        // Close first so the app's own menu or window gets focus.
                        self.popup.hide();
                        tray::click(&icon, msg == WM_RBUTTONUP, pt.x, pt.y);
                    }
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn paint(&mut self) -> Result<()> {
        let s = self.scale();
        let pad = self.pad();
        let Some(frame) = self.popup.begin() else { return Ok(()) };
        let frame = frame?;
        if self.popup.gfx.generation != self.bitmap_generation {
            self.bitmaps.clear();
            self.bitmap_generation = self.popup.gfx.generation;
        }

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
                        .and_then(|h| self.icons_wic.as_ref().and_then(|w| w.for_hicon(h, icon as i32)))
                        .and_then(|src| frame.bitmap(&src).ok());
                    self.bitmaps.insert(handle, made.clone());
                    made
                }
            };
            if let Some(b) = bmp {
                frame.draw_icon(&b, r.x + (r.w - icon) / 2.0, r.y + (r.h - icon) / 2.0, icon, 1.0);
            }
            cells.push(r);
        }

        // Tooltip line
        let (width, height) = (self.popup.width as f32, self.popup.height as f32);
        let tip_y = height - pad - self.tip_height();
        let text = match self.hovered.and_then(|i| self.items.get(i)) {
            Some(icon) if !icon.tip.is_empty() => (icon.tip.lines().next().unwrap_or("").to_string(), FG),
            Some(_) => (String::new(), DIM),
            None if self.items.is_empty() => ("No tray icons".to_string(), DIM),
            None => ("Click: open · Right-click: menu".to_string(), DIM),
        };
        if !text.0.is_empty() {
            let t = self.popup.gfx.text_fit(&text.0, &self.font, width - 2.0 * pad - 8.0 * s, self.tip_height())?;
            frame.text(&t, pad + 4.0 * s, tip_y + 4.0 * s, text.1);
        }

        let result = self.popup.gfx.end(frame);
        self.cells = cells;
        result
    }
}
