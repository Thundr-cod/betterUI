// The Bluetooth dropdown and the control center. They share one window
// (only one flyout is open at a time) and differ only in what they lay out.
//
// Bluetooth:      on/off switch, paired devices with connection status,
//                 link to Bluetooth settings.
// Control center: Wi-Fi / Bluetooth / Settings tiles, brightness slider
//                 (external monitors with DDC/CI), volume slider.
//
// Radio and Bluetooth queries can take up to a second, so they run on a
// background thread and the result is posted back to the window; the UI
// never waits on them.

use std::cell::RefCell;
use std::sync::mpsc::{channel, Receiver, Sender};

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, IsWindowVisible, KillTimer, PostMessageW, SetForegroundWindow, SetTimer,
    SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SW_HIDE, SW_SHOW,
    WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_TIMER,
};

use crate::audio::{Audio, Flow, Level};
use crate::audio_flyout::WM_AUDIO_CHANGED;
use crate::bluetooth::{self, BtDevice};
use crate::radios::{self, RadioStates};
use crate::render::{Color, Gfx, Text, ICON_FONT, TEXT_FONT};
use crate::ui::{self, R, DIM, FAINT, FG};
use crate::{brightness, log, popup};

const WM_PANEL_DATA: u32 = WM_APP + 10;
const TIMER_REFRESH: usize = 1;
const MAX_DEVICES: usize = 8;

// Segoe Fluent Icons
const ICON_WIFI: &str = "\u{E701}";
const ICON_BLUETOOTH: &str = "\u{E702}";
const ICON_SETTINGS: &str = "\u{E713}";
const ICON_BRIGHTNESS: &str = "\u{E706}";
const ICON_CHEVRON: &str = "\u{E76C}";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bluetooth,
    ControlCenter,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SliderId {
    Brightness,
    Volume,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    WifiTile,
    BluetoothTile,
    SettingsTile,
    BluetoothSwitch,
    Link(&'static str),
    Slider(SliderId),
    Mute,
}

enum Item {
    Rect(R, f32, Color),
    Circle(f32, f32, f32, Color),
    Text(Text, f32, f32, Color),
}

/// Result of a background query.
struct Fetched {
    radios: RadioStates,
    devices: Option<Vec<BtDevice>>,
    brightness: Option<Option<f32>>,
}

struct Fonts {
    text: IDWriteTextFormat,
    bold: IDWriteTextFormat,
    small: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    icon_small: IDWriteTextFormat,
}

struct Panel {
    hwnd: HWND,
    notify: HWND,
    gfx: Gfx,
    fonts: Fonts,
    dpi: u32,
    kind: Kind,
    audio: Option<Audio>,
    tx: Sender<Fetched>,
    rx: Receiver<Fetched>,
    fetching: bool,
    loaded: bool,
    radios: RadioStates,
    devices: Vec<BtDevice>,
    brightness: Option<f32>,
    brightness_known: bool,
    volume: Option<Level>,
    accent: Color,
    opacity: f32,
    width: i32,
    height: i32,
    hits: Vec<(R, Hit)>,
    tracks: Vec<(SliderId, f32, f32)>,
    dragging: Option<SliderId>,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

/// Open `kind` under `anchor`, or close it if it's already showing.
pub fn toggle(kind: Kind, anchor: RECT, notify: HWND, bar_opacity: u32, force_open: bool) {
    if let Err(e) = ensure_created(notify) {
        log::write(&format!("panel could not be created: {e:?}"));
        return;
    }
    PANEL.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else { return };
        let Some(p) = guard.as_mut() else { return };
        let visible = unsafe { IsWindowVisible(p.hwnd).as_bool() };
        if visible && p.kind == kind && !force_open {
            p.hide();
        } else {
            p.kind = kind;
            p.opacity = (bar_opacity.max(70) as f32) / 100.0;
            p.show(anchor);
        }
    });
}

pub fn hide() {
    PANEL.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(p) = guard.as_mut() {
                p.hide();
            }
        }
    });
}

fn ensure_created(notify: HWND) -> Result<()> {
    if PANEL.with(|c| c.borrow().is_some()) {
        return Ok(());
    }
    let hwnd = popup::create(w!("betterUI.Panel"), w!("betterUI panel"), Some(wndproc))?;
    let gfx = Gfx::new()?;
    let fonts = make_fonts(&gfx, 1.0)?;
    let (tx, rx) = channel();
    let panel = Panel {
        hwnd,
        notify,
        gfx,
        fonts,
        dpi: 96,
        kind: Kind::ControlCenter,
        audio: Audio::new().ok(),
        tx,
        rx,
        fetching: false,
        loaded: false,
        radios: RadioStates::default(),
        devices: Vec::new(),
        brightness: None,
        brightness_known: false,
        volume: None,
        accent: ui::accent(),
        opacity: 0.85,
        width: 0,
        height: 0,
        hits: Vec::new(),
        tracks: Vec::new(),
        dragging: None,
    };
    PANEL.with(|c| *c.borrow_mut() = Some(panel));
    Ok(())
}

fn make_fonts(gfx: &Gfx, s: f32) -> Result<Fonts> {
    Ok(Fonts {
        text: gfx.text_format(TEXT_FONT, 13.0 * s, 400)?,
        bold: gfx.text_format(TEXT_FONT, 14.0 * s, 600)?,
        small: gfx.text_format(TEXT_FONT, 12.0 * s, 400)?,
        icon: gfx.text_format(ICON_FONT, 16.0 * s, 400)?,
        icon_small: gfx.text_format(ICON_FONT, 11.0 * s, 400)?,
    })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    let handled = PANEL.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|p| p.handle(msg, wp, lp)),
        Err(_) => None,
    });
    handled.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
}

fn point_from(lp: LPARAM) -> (i32, i32) {
    (
        (lp.0 & 0xFFFF) as u16 as i16 as i32,
        ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
}

/// Run `work` on a background thread (with COM ready for the WinRT calls)
/// and post the result back to the panel window.
fn spawn_fetch(hwnd: HWND, tx: Sender<Fetched>, work: impl FnOnce() -> Fetched + Send + 'static) {
    let hwnd_raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let result = work();
        let _ = tx.send(result);
        unsafe {
            let _ = PostMessageW(HWND(hwnd_raw as *mut _), WM_PANEL_DATA, WPARAM(0), LPARAM(0));
        }
    });
}

impl Panel {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn width_for(&self) -> f32 {
        match self.kind {
            Kind::Bluetooth => 320.0,
            Kind::ControlCenter => 360.0,
        }
    }

    fn fetch(&mut self, with_brightness: bool) {
        if self.fetching {
            return;
        }
        self.fetching = true;
        let want_devices = self.kind == Kind::Bluetooth;
        spawn_fetch(self.hwnd, self.tx.clone(), move || Fetched {
            radios: radios::states(),
            devices: if want_devices { Some(bluetooth::paired()) } else { None },
            brightness: if with_brightness { Some(brightness::read()) } else { None },
        });
    }

    fn show(&mut self, anchor: RECT) {
        let (work, dpi) = popup::monitor_of(&anchor);
        if dpi != self.dpi {
            self.dpi = dpi;
            match make_fonts(&self.gfx, self.scale()) {
                Ok(f) => self.fonts = f,
                Err(e) => log::write(&format!("panel fonts failed: {e:?}")),
            }
        }
        self.accent = ui::accent();
        self.volume = self.audio.as_ref().and_then(|a| a.level(Flow::Output));
        self.fetch(!self.brightness_known);

        let s = self.scale();
        self.width = (self.width_for() * s).round() as i32;
        self.height = self.measure_height();
        let (x, y) = popup::place(&anchor, &work, self.width, (6.0 * s).round() as i32);
        unsafe {
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, self.width, self.height, SWP_NOACTIVATE);
            popup::round_corners(self.hwnd, self.width, self.height, (8.0 * s).round() as i32);
            self.gfx.resize(self.width as u32, self.height as u32);
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            SetTimer(self.hwnd, TIMER_REFRESH, 2000, None);
        }
        self.invalidate();
    }

    fn hide(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_REFRESH);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        if self.dragging.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    fn fit_height(&mut self) {
        let h = self.measure_height();
        if h != self.height {
            self.height = h;
            unsafe {
                let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, self.width, self.height, SWP_NOACTIVATE | SWP_NOMOVE);
            }
            popup::round_corners(self.hwnd, self.width, self.height, (8.0 * self.scale()).round() as i32);
            self.gfx.resize(self.width as u32, self.height as u32);
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
                        log::write(&format!("panel paint failed: {e:?}"));
                    }
                    let _ = EndPaint(self.hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_PANEL_DATA => {
                while let Ok(f) = self.rx.try_recv() {
                    self.fetching = false;
                    self.loaded = true;
                    self.radios = f.radios;
                    if let Some(d) = f.devices {
                        self.devices = d;
                    }
                    if let Some(b) = f.brightness {
                        self.brightness = b;
                        self.brightness_known = true;
                    }
                }
                if self.dragging.is_none() {
                    self.fit_height();
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_TIMER => {
                if self.dragging.is_none() {
                    self.volume = self.audio.as_ref().and_then(|a| a.level(Flow::Output));
                    self.fetch(false);
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
            WM_LBUTTONDOWN => {
                let (x, y) = point_from(lp);
                self.mouse_down(x as f32, y as f32);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if let Some(id) = self.dragging {
                    let (x, _) = point_from(lp);
                    self.drag_to(id, x as f32);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.dragging.take().is_some() {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let notches = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as f32 / 120.0;
                let (sx, sy) = point_from(lp);
                let mut pt = POINT { x: sx, y: sy };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let id = self.hits.iter().find_map(|(r, h)| match h {
                    Hit::Slider(id) if r.contains(pt.x as f32, pt.y as f32) => Some(*id),
                    Hit::Mute if r.contains(pt.x as f32, pt.y as f32) => Some(SliderId::Volume),
                    _ => None,
                });
                if let Some(id) = id {
                    let v = self.value_of(id) + notches * 0.02;
                    self.set_value(id, v);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn value_of(&self, id: SliderId) -> f32 {
        match id {
            SliderId::Brightness => self.brightness.unwrap_or(0.0),
            SliderId::Volume => self.volume.map(|l| l.volume).unwrap_or(0.0),
        }
    }

    fn set_value(&mut self, id: SliderId, v: f32) {
        let v = v.clamp(0.0, 1.0);
        match id {
            SliderId::Brightness => {
                self.brightness = Some(v);
                brightness::set(v);
            }
            SliderId::Volume => {
                if let Some(a) = &self.audio {
                    a.set_volume(Flow::Output, v);
                }
                if let Some(l) = self.volume.as_mut() {
                    l.volume = v;
                    if v > 0.0 {
                        l.muted = false;
                    }
                }
                unsafe {
                    let _ = PostMessageW(self.notify, WM_AUDIO_CHANGED, WPARAM(0), LPARAM(0));
                }
            }
        }
        self.invalidate();
    }

    fn drag_to(&mut self, id: SliderId, x: f32) {
        if let Some(&(_, tx, tw)) = self.tracks.iter().find(|(t, _, _)| *t == id) {
            if tw > 0.0 {
                self.set_value(id, (x - tx) / tw);
            }
        }
    }

    fn toggle_radio(&mut self, kind: radios::Kind) {
        let current = match kind {
            radios::Kind::Wifi => self.radios.wifi,
            radios::Kind::Bluetooth => self.radios.bluetooth,
        };
        let Some(on) = current else { return };
        // Show the new state right away; the background result confirms it.
        match kind {
            radios::Kind::Wifi => self.radios.wifi = Some(!on),
            radios::Kind::Bluetooth => self.radios.bluetooth = Some(!on),
        }
        self.invalidate();
        let want_devices = self.kind == Kind::Bluetooth;
        self.fetching = true;
        spawn_fetch(self.hwnd, self.tx.clone(), move || {
            radios::set(kind, !on);
            Fetched {
                radios: radios::states(),
                devices: if want_devices { Some(bluetooth::paired()) } else { None },
                brightness: None,
            }
        });
    }

    fn mouse_down(&mut self, x: f32, y: f32) {
        let Some(hit) = self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h) else {
            return;
        };
        match hit {
            Hit::WifiTile => {
                if self.radios.wifi.is_some() {
                    self.toggle_radio(radios::Kind::Wifi);
                } else {
                    self.hide();
                    popup::open_uri("ms-settings:network");
                }
            }
            Hit::BluetoothTile | Hit::BluetoothSwitch => {
                if self.radios.bluetooth.is_some() {
                    self.toggle_radio(radios::Kind::Bluetooth);
                } else {
                    self.hide();
                    popup::open_uri("ms-settings:bluetooth");
                }
            }
            Hit::SettingsTile => {
                self.hide();
                popup::open_uri("ms-settings:");
            }
            Hit::Link(uri) => {
                self.hide();
                popup::open_uri(uri);
            }
            Hit::Mute => {
                if let (Some(a), Some(l)) = (&self.audio, self.volume.as_mut()) {
                    l.muted = !l.muted;
                    a.set_muted(Flow::Output, l.muted);
                }
                unsafe {
                    let _ = PostMessageW(self.notify, WM_AUDIO_CHANGED, WPARAM(0), LPARAM(0));
                }
                self.invalidate();
            }
            Hit::Slider(id) => {
                self.dragging = Some(id);
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.drag_to(id, x);
            }
        }
    }

    // ---- Layout ------------------------------------------------------------

    fn measure_height(&mut self) -> i32 {
        match self.layout() {
            Ok((_, h)) => h.ceil() as i32,
            Err(_) => (200.0 * self.scale()) as i32,
        }
    }

    fn layout(&mut self) -> Result<(Vec<Item>, f32)> {
        let mut items = Vec::new();
        let mut hits = Vec::new();
        let mut tracks = Vec::new();
        let h = match self.kind {
            Kind::Bluetooth => self.layout_bluetooth(&mut items, &mut hits)?,
            Kind::ControlCenter => self.layout_control_center(&mut items, &mut hits, &mut tracks)?,
        };
        self.hits = hits;
        self.tracks = tracks;
        Ok((items, h))
    }

    fn state_text(&self, state: Option<bool>) -> &'static str {
        if !self.loaded {
            "…"
        } else {
            match state {
                Some(true) => "On",
                Some(false) => "Off",
                None => "Unavailable",
            }
        }
    }

    fn switch(&self, items: &mut Vec<Item>, x: f32, y: f32, on: bool) {
        let s = self.scale();
        let (w, h) = (40.0 * s, 20.0 * s);
        if on {
            items.push(Item::Rect(R::new(x, y, w, h), h / 2.0, self.accent));
            items.push(Item::Circle(x + w - h / 2.0, y + h / 2.0, 6.0 * s, Color(0.05, 0.05, 0.07, 1.0)));
        } else {
            items.push(Item::Rect(R::new(x, y, w, h), h / 2.0, Color(1.0, 1.0, 1.0, 0.16)));
            items.push(Item::Circle(x + h / 2.0, y + h / 2.0, 6.0 * s, DIM));
        }
    }

    fn layout_bluetooth(&self, items: &mut Vec<Item>, hits: &mut Vec<(R, Hit)>) -> Result<f32> {
        let s = self.scale();
        let w = self.width_for() * s;
        let pad = 14.0 * s;
        let inner = w - 2.0 * pad;
        let mut y = pad;

        // Header: title + switch
        let title = self.gfx.text("Bluetooth", &self.fonts.bold, 28.0 * s)?;
        items.push(Item::Text(title, pad, y, FG));
        if self.radios.bluetooth.is_some() {
            let sx = w - pad - 40.0 * s;
            self.switch(items, sx, y + 4.0 * s, self.radios.bluetooth == Some(true));
            hits.push((R::new(sx - 6.0 * s, y, 52.0 * s, 28.0 * s), Hit::BluetoothSwitch));
        }
        y += 36.0 * s;

        let message = if !self.loaded {
            Some("Loading…")
        } else if self.radios.bluetooth.is_none() {
            Some("Bluetooth isn't available on this PC")
        } else if self.radios.bluetooth == Some(false) {
            Some("Turn on Bluetooth to use your devices")
        } else if self.devices.is_empty() {
            Some("No paired devices")
        } else {
            None
        };
        if let Some(m) = message {
            let t = self.gfx.text(m, &self.fonts.text, 32.0 * s)?;
            items.push(Item::Text(t, pad, y, DIM));
            y += 36.0 * s;
        } else {
            for dev in self.devices.iter().take(MAX_DEVICES) {
                let row = R::new(pad, y, inner, 40.0 * s);
                if dev.connected {
                    items.push(Item::Rect(row, 6.0 * s, FAINT));
                }
                let icon = self.gfx.text(ICON_BLUETOOTH, &self.fonts.icon, row.h)?;
                items.push(Item::Text(icon, row.x + 10.0 * s, row.y, if dev.connected { self.accent } else { DIM }));
                let status = self.gfx.text(if dev.connected { "Connected" } else { "Not connected" }, &self.fonts.small, row.h)?;
                let status_x = row.x + row.w - 10.0 * s - status.width;
                let name_x = row.x + 40.0 * s;
                let name = self.gfx.text_fit(&dev.name, &self.fonts.text, status_x - name_x - 10.0 * s, row.h)?;
                items.push(Item::Text(name, name_x, row.y, if dev.connected { FG } else { DIM }));
                items.push(Item::Text(status, status_x, row.y, DIM));
                y += 42.0 * s;
            }
        }

        // Footer link
        y += 4.0 * s;
        items.push(Item::Rect(R::new(pad, y, inner, 1.0 * s), 0.0, FAINT));
        y += 6.0 * s;
        let row = R::new(pad, y, inner, 34.0 * s);
        let t = self.gfx.text("Bluetooth settings", &self.fonts.text, row.h)?;
        items.push(Item::Text(t, row.x + 10.0 * s, row.y, FG));
        let chev = self.gfx.text(ICON_CHEVRON, &self.fonts.icon_small, row.h)?;
        let cx = row.x + row.w - 10.0 * s - chev.width;
        items.push(Item::Text(chev, cx, row.y, DIM));
        hits.push((row, Hit::Link("ms-settings:bluetooth")));
        y += row.h + pad - 4.0 * s;
        Ok(y)
    }

    fn layout_control_center(
        &self,
        items: &mut Vec<Item>,
        hits: &mut Vec<(R, Hit)>,
        tracks: &mut Vec<(SliderId, f32, f32)>,
    ) -> Result<f32> {
        let s = self.scale();
        let w = self.width_for() * s;
        let pad = 14.0 * s;
        let inner = w - 2.0 * pad;
        let mut y = pad;

        // Tiles
        let gap = 8.0 * s;
        let tile_w = (inner - 2.0 * gap) / 3.0;
        let tile_h = 76.0 * s;
        let tiles: [(&str, &str, Option<bool>, Hit, bool); 3] = [
            (ICON_WIFI, "Wi-Fi", self.radios.wifi, Hit::WifiTile, true),
            (ICON_BLUETOOTH, "Bluetooth", self.radios.bluetooth, Hit::BluetoothTile, true),
            (ICON_SETTINGS, "Settings", None, Hit::SettingsTile, false),
        ];
        for (i, (icon, label, state, hit, is_radio)) in tiles.iter().enumerate() {
            let r = R::new(pad + i as f32 * (tile_w + gap), y, tile_w, tile_h);
            let on = *is_radio && *state == Some(true);
            let (bg, fg, sub) = if on {
                (self.accent, Color(0.05, 0.05, 0.07, 1.0), Color(0.05, 0.05, 0.07, 0.75))
            } else {
                (FAINT, FG, DIM)
            };
            items.push(Item::Rect(r, 8.0 * s, bg));
            let ic = self.gfx.text(icon, &self.fonts.icon, 24.0 * s)?;
            items.push(Item::Text(ic, r.x + 12.0 * s, r.y + 10.0 * s, fg));
            let lt = self.gfx.text_fit(label, &self.fonts.text, r.w - 20.0 * s, 18.0 * s)?;
            items.push(Item::Text(lt, r.x + 12.0 * s, r.y + 38.0 * s, fg));
            let st = if *is_radio { self.state_text(*state) } else { "Open" };
            let stt = self.gfx.text_fit(st, &self.fonts.small, r.w - 20.0 * s, 16.0 * s)?;
            items.push(Item::Text(stt, r.x + 12.0 * s, r.y + 55.0 * s, sub));
            hits.push((r, *hit));
        }
        y += tile_h + 14.0 * s;

        // Sliders
        let slider = |items: &mut Vec<Item>,
                          hits: &mut Vec<(R, Hit)>,
                          tracks: &mut Vec<(SliderId, f32, f32)>,
                          id: SliderId,
                          title: &str,
                          glyph: &str,
                          value: f32,
                          muted: bool,
                          y: &mut f32|
         -> Result<()> {
            let t = self.gfx.text(title, &self.fonts.small, 18.0 * s)?;
            items.push(Item::Text(t, pad, *y, DIM));
            *y += 20.0 * s;
            let row_h = 32.0 * s;
            let btn = R::new(pad, *y, row_h, row_h);
            items.push(Item::Rect(btn, 6.0 * s, FAINT));
            let icon = self.gfx.text(glyph, &self.fonts.icon, row_h)?;
            let ix = btn.x + (btn.w - icon.width) / 2.0;
            items.push(Item::Text(icon, ix, *y, if muted { DIM } else { FG }));
            if id == SliderId::Volume {
                hits.push((btn, Hit::Mute));
            }
            let pct = self.gfx.text(&format!("{}", (value * 100.0).round() as i32), &self.fonts.text, row_h)?;
            let tx = btn.x + btn.w + 14.0 * s;
            let tw = w - pad - 34.0 * s - 10.0 * s - tx;
            let cy = *y + row_h / 2.0;
            let v = value.clamp(0.0, 1.0);
            let fill = if muted { DIM } else { self.accent };
            items.push(Item::Rect(R::new(tx, cy - 2.0 * s, tw, 4.0 * s), 2.0 * s, Color(1.0, 1.0, 1.0, 0.18)));
            if v > 0.0 {
                items.push(Item::Rect(R::new(tx, cy - 2.0 * s, tw * v, 4.0 * s), 2.0 * s, fill));
            }
            items.push(Item::Circle(tx + tw * v, cy, 8.0 * s, Color(0.27, 0.27, 0.30, 1.0)));
            items.push(Item::Circle(tx + tw * v, cy, 4.5 * s, fill));
            hits.push((R::new(tx - 10.0 * s, *y, tw + 20.0 * s, row_h), Hit::Slider(id)));
            tracks.push((id, tx, tw));
            let px = w - pad - pct.width;
            items.push(Item::Text(pct, px, *y, DIM));
            *y += row_h + 12.0 * s;
            Ok(())
        };

        if let Some(b) = self.brightness {
            slider(items, hits, tracks, SliderId::Brightness, "Brightness", ICON_BRIGHTNESS, b, false, &mut y)?;
        }
        match self.volume {
            Some(l) => slider(
                items,
                hits,
                tracks,
                SliderId::Volume,
                "Volume",
                ui::volume_glyph(l.volume, l.muted),
                l.volume,
                l.muted,
                &mut y,
            )?,
            None => {
                let t = self.gfx.text("No audio output device", &self.fonts.text, 28.0 * s)?;
                items.push(Item::Text(t, pad, y, DIM));
                y += 34.0 * s;
            }
        }
        y += pad - 12.0 * s;
        Ok(y)
    }

    fn paint(&mut self) -> Result<()> {
        if self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        let (items, _) = self.layout()?;
        let frame = self.gfx.begin(self.hwnd, self.width as u32, self.height as u32)?;
        frame.round_rect(0.0, 0.0, self.width as f32, self.height as f32, 0.0, Color(0.10, 0.10, 0.12, self.opacity));
        for item in &items {
            match item {
                Item::Rect(r, radius, c) => frame.round_rect(r.x, r.y, r.w, r.h, *radius, *c),
                Item::Circle(x, y, radius, c) => frame.circle(*x, *y, *radius, *c),
                Item::Text(t, x, y, c) => frame.text(t, *x, *y, *c),
            }
        }
        self.gfx.end(frame)
    }
}
