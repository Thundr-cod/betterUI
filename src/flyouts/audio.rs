// The audio flyout, opened from the speaker button on the bar.
//
// Two modes (tabs):
//   Devices: output volume + choose the output device,
//            input volume  + choose the input device
//   Mixer:   volume and mute for each app that has audio open
//
// While open it re-reads the audio state once a second (skipped while a
// slider is being dragged), so changes made elsewhere show up.

use std::cell::RefCell;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, WM_ACTIVATE, WM_APP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT,
    WM_TIMER,
};

use super::popup::{self, Popup};
use super::widgets::{self, Item, SliderStyle};
use crate::log;
use crate::system::audio::{App, Audio, Device, Flow, Level};
use crate::ui::render::{Color, Gfx, ICON_FONT, TEXT_FONT};
use crate::ui::{self, window, R, DIM, FAINT, FG, HOVER, ICON_CHECK, ICON_MIC, ICON_MUTE};

const WIDTH: f32 = 340.0; // logical pixels
const MAX_ROWS: usize = 10; // per device list / mixer, keeps the flyout on screen
const TIMER_REFRESH: usize = 1;

/// Posted to the bar after a change so its speaker icon updates at once.
pub const WM_AUDIO_CHANGED: u32 = WM_APP + 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Devices,
    Mixer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Master(Flow),
    App(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Tab(Tab),
    Device(Flow, usize),
    Slider(Target),
    Mute(Target),
}

struct Fonts {
    text: IDWriteTextFormat,
    bold: IDWriteTextFormat,
    small: IDWriteTextFormat,
    icon: IDWriteTextFormat,
}

struct Flyout {
    popup: Popup,
    notify: HWND,
    fonts: Fonts,
    audio: Audio,
    tab: Tab,
    outputs: Vec<Device>,
    inputs: Vec<Device>,
    out_level: Option<Level>,
    in_level: Option<Level>,
    apps: Vec<App>,
    accent: Color,
    hits: Vec<(R, Hit)>,
    tracks: Vec<(Target, f32, f32)>, // slider target, track x, track width
    dragging: Option<Target>,
}

thread_local! {
    static FLYOUT: RefCell<Option<Flyout>> = const { RefCell::new(None) };
}

/// Show the flyout under `anchor` (screen rectangle of the bar button), or
/// hide it if it is already open. `notify` is told about volume changes.
pub fn toggle(anchor: RECT, notify: HWND, bar_opacity: u32) {
    if let Err(e) = ensure_created(notify) {
        log::write(&format!("audio flyout could not be created: {e:?}"));
        return;
    }
    window::with_state(&FLYOUT, |f| {
        if f.popup.visible() {
            f.hide();
        } else {
            f.popup.set_opacity(bar_opacity);
            f.show(anchor);
        }
    });
}

pub fn hide() {
    window::with_state(&FLYOUT, |f| f.hide());
}

fn ensure_created(notify: HWND) -> Result<()> {
    if FLYOUT.with(|c| c.borrow().is_some()) {
        return Ok(());
    }
    let popup = Popup::new(w!("betterUI.AudioFlyout"), w!("betterUI audio"), Some(wndproc), Some((TIMER_REFRESH, 1000)))?;
    let fonts = make_fonts(&popup.gfx, 1.0)?;
    let flyout = Flyout {
        popup,
        notify,
        fonts,
        audio: Audio::new()?,
        tab: Tab::Devices,
        outputs: Vec::new(),
        inputs: Vec::new(),
        out_level: None,
        in_level: None,
        apps: Vec::new(),
        accent: ui::accent(),
        hits: Vec::new(),
        tracks: Vec::new(),
        dragging: None,
    };
    FLYOUT.with(|c| *c.borrow_mut() = Some(flyout));
    Ok(())
}

fn make_fonts(gfx: &Gfx, s: f32) -> Result<Fonts> {
    Ok(Fonts {
        text: gfx.text_format(TEXT_FONT, 13.0 * s, 400)?,
        bold: gfx.text_format(TEXT_FONT, 13.0 * s, 600)?,
        small: gfx.text_format(TEXT_FONT, 12.0 * s, 600)?,
        icon: gfx.text_format(ICON_FONT, 16.0 * s, 400)?,
    })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    window::dispatch(&FLYOUT, hwnd, msg, wp, lp, Flyout::handle)
}

impl Flyout {
    fn scale(&self) -> f32 {
        self.popup.scale()
    }

    fn refresh(&mut self) {
        self.outputs = self.audio.devices(Flow::Output);
        self.inputs = self.audio.devices(Flow::Input);
        self.out_level = self.audio.level(Flow::Output);
        self.in_level = self.audio.level(Flow::Input);
        self.apps = self.audio.apps();
    }

    fn show(&mut self, anchor: RECT) {
        let (work, dpi_changed) = self.popup.locate(&anchor);
        if dpi_changed {
            match make_fonts(&self.popup.gfx, self.scale()) {
                Ok(f) => self.fonts = f,
                Err(e) => log::write(&format!("flyout fonts failed: {e:?}")),
            }
        }
        self.accent = ui::accent();
        self.refresh();
        let w = (WIDTH * self.scale()).round() as i32;
        let h = self.measure_height();
        self.popup.show(&anchor, &work, w, h);
    }

    fn hide(&mut self) {
        self.popup.hide();
        if self.dragging.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    /// Resize the window if the content height changed (devices or apps
    /// came or went).
    fn fit_height(&mut self) {
        let h = self.measure_height();
        self.popup.resize(self.popup.width, h);
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                let hwnd = self.popup.hwnd;
                window::paint(hwnd, "audio flyout", || self.paint());
                Some(LRESULT(0))
            }
            m if popup::wants_close(m, wp) => {
                self.hide();
                if m == WM_ACTIVATE { None } else { Some(LRESULT(0)) }
            }
            WM_TIMER => {
                if self.dragging.is_none() {
                    self.refresh();
                    self.fit_height();
                    self.popup.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = window::client_point(lp);
                self.mouse_down(x, y);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if let Some(t) = self.dragging {
                    self.drag_to(t, window::client_point(lp).0);
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
                let (x, y) = window::wheel_point(self.popup.hwnd, lp);
                let target = self.hits.iter().find_map(|(r, h)| match h {
                    Hit::Slider(t) | Hit::Mute(t) if r.contains(x, y) => Some(*t),
                    _ => None,
                });
                if let Some(t) = target {
                    let v = self.volume_of(t) + window::wheel_notches(wp) * 0.02;
                    self.set_volume(t, v);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn changed(&self) {
        self.popup.invalidate();
        unsafe {
            let _ = PostMessageW(self.notify, WM_AUDIO_CHANGED, WPARAM(0), LPARAM(0));
        }
    }

    fn mouse_down(&mut self, x: f32, y: f32) {
        let Some(hit) = self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h) else {
            return;
        };
        match hit {
            Hit::Tab(t) => {
                if self.tab != t {
                    self.tab = t;
                    self.refresh();
                    self.fit_height();
                    self.popup.invalidate();
                }
            }
            Hit::Device(flow, i) => {
                let list = if flow == Flow::Output { &self.outputs } else { &self.inputs };
                if let Some(dev) = list.get(i) {
                    if !dev.is_default {
                        let id = dev.id.clone();
                        self.audio.set_default(&id);
                        self.refresh();
                        self.fit_height();
                        self.changed();
                    }
                }
            }
            Hit::Mute(t) => {
                let muted = !self.muted_of(t);
                match t {
                    Target::Master(flow) => {
                        self.audio.set_muted(flow, muted);
                        if let Some(l) = self.level_mut(flow) {
                            l.muted = muted;
                        }
                    }
                    Target::App(i) => {
                        if let Some(app) = self.apps.get_mut(i) {
                            app.set_muted(muted);
                        }
                    }
                }
                self.changed();
            }
            Hit::Slider(t) => {
                self.dragging = Some(t);
                unsafe {
                    SetCapture(self.popup.hwnd);
                }
                self.drag_to(t, x);
            }
        }
    }

    fn drag_to(&mut self, t: Target, x: f32) {
        if let Some(v) = widgets::track_value(&self.tracks, t, x) {
            self.set_volume(t, v);
        }
    }

    fn level_mut(&mut self, flow: Flow) -> Option<&mut Level> {
        match flow {
            Flow::Output => self.out_level.as_mut(),
            Flow::Input => self.in_level.as_mut(),
        }
    }

    fn volume_of(&self, t: Target) -> f32 {
        match t {
            Target::Master(Flow::Output) => self.out_level.map(|l| l.volume).unwrap_or(0.0),
            Target::Master(Flow::Input) => self.in_level.map(|l| l.volume).unwrap_or(0.0),
            Target::App(i) => self.apps.get(i).map(|a| a.volume).unwrap_or(0.0),
        }
    }

    fn muted_of(&self, t: Target) -> bool {
        match t {
            Target::Master(Flow::Output) => self.out_level.map(|l| l.muted).unwrap_or(false),
            Target::Master(Flow::Input) => self.in_level.map(|l| l.muted).unwrap_or(false),
            Target::App(i) => self.apps.get(i).map(|a| a.muted).unwrap_or(false),
        }
    }

    fn set_volume(&mut self, t: Target, v: f32) {
        let v = v.clamp(0.0, 1.0);
        match t {
            Target::Master(flow) => {
                self.audio.set_volume(flow, v);
                if let Some(l) = self.level_mut(flow) {
                    l.volume = v;
                    if v > 0.0 {
                        l.muted = false;
                    }
                }
            }
            Target::App(i) => {
                if let Some(app) = self.apps.get_mut(i) {
                    app.set_volume(v);
                }
            }
        }
        self.changed();
    }

    // ---- Layout ------------------------------------------------------------

    fn measure_height(&mut self) -> i32 {
        match self.layout() {
            Ok((_, h)) => h.ceil() as i32,
            Err(_) => (200.0 * self.scale()) as i32,
        }
    }

    /// Work out every shape, text and clickable area. Used both to measure
    /// the window height and to paint, so the two always agree.
    fn layout(&mut self) -> Result<(Vec<Item>, f32)> {
        let s = self.scale();
        let gfx = &self.popup.gfx;
        let w = WIDTH * s;
        let pad = 14.0 * s;
        let inner = w - 2.0 * pad;
        let mut items: Vec<Item> = Vec::new();
        let mut hits: Vec<(R, Hit)> = Vec::new();
        let mut tracks: Vec<(Target, f32, f32)> = Vec::new();
        let mut y = pad;
        let style = SliderStyle {
            gfx,
            icon_font: &self.fonts.icon,
            text_font: &self.fonts.text,
            accent: self.accent,
            scale: s,
            pad,
            width: w,
        };
        let mut slider = |items: &mut Vec<Item>, target: Target, glyph: &str, volume: f32, muted: bool, y: f32| -> Result<()> {
            let sl = widgets::slider(&style, items, y, glyph, volume, muted)?;
            hits.push((sl.button, Hit::Mute(target)));
            hits.push((sl.track_hit, Hit::Slider(target)));
            tracks.push((target, sl.track_x, sl.track_w));
            Ok(())
        };

        // Tabs
        let tab_h = 32.0 * s;
        items.push(Item::Rect(R::new(pad, y, inner, tab_h), 8.0 * s, FAINT));
        let mut tab_hits = Vec::new();
        for (i, (tab, label)) in [(Tab::Devices, "Devices"), (Tab::Mixer, "Mixer")].iter().enumerate() {
            let half = inner / 2.0;
            let r = R::new(pad + half * i as f32, y, half, tab_h);
            let active = self.tab == *tab;
            if active {
                items.push(Item::Rect(R::new(r.x + 3.0 * s, r.y + 3.0 * s, r.w - 6.0 * s, r.h - 6.0 * s), 6.0 * s, HOVER));
            }
            let t = gfx.text(label, &self.fonts.bold, tab_h)?;
            let x = r.x + (r.w - t.width) / 2.0;
            items.push(Item::Text(t, x, y, if active { FG } else { DIM }));
            tab_hits.push((r, Hit::Tab(*tab)));
        }
        y += tab_h + 12.0 * s;

        let mut device_hits = Vec::new();
        match self.tab {
            Tab::Devices => {
                for flow in [Flow::Output, Flow::Input] {
                    let (title, level, devices) = match flow {
                        Flow::Output => ("Output", self.out_level, &self.outputs),
                        Flow::Input => ("Input", self.in_level, &self.inputs),
                    };
                    let t = gfx.text(title, &self.fonts.small, 20.0 * s)?;
                    items.push(Item::Text(t, pad, y, DIM));
                    y += 22.0 * s;
                    if let Some(level) = level {
                        let glyph = match flow {
                            Flow::Output => ui::volume_glyph(level.volume, level.muted),
                            Flow::Input if level.muted => ICON_MUTE,
                            Flow::Input => ICON_MIC,
                        };
                        slider(&mut items, Target::Master(flow), glyph, level.volume, level.muted, y)?;
                        y += 40.0 * s;
                    }
                    if devices.is_empty() {
                        let t = gfx.text("No devices found", &self.fonts.text, 30.0 * s)?;
                        items.push(Item::Text(t, pad + 4.0 * s, y, DIM));
                        y += 30.0 * s;
                    }
                    for (i, dev) in devices.iter().take(MAX_ROWS).enumerate() {
                        let row = R::new(pad, y, inner, 32.0 * s);
                        if dev.is_default {
                            items.push(Item::Rect(row, 6.0 * s, FAINT));
                            let check = gfx.text(ICON_CHECK, &self.fonts.icon, row.h)?;
                            items.push(Item::Text(check, row.x + 10.0 * s, row.y, self.accent));
                        }
                        let name_x = row.x + 36.0 * s;
                        let t = gfx.text_fit(&dev.name, &self.fonts.text, row.w - 36.0 * s - 8.0 * s, row.h)?;
                        items.push(Item::Text(t, name_x, row.y, if dev.is_default { FG } else { DIM }));
                        device_hits.push((row, Hit::Device(flow, i)));
                        y += 34.0 * s;
                    }
                    y += 10.0 * s;
                }
                y -= 10.0 * s;
            }
            Tab::Mixer => {
                if let Some(level) = self.out_level {
                    let t = gfx.text("Master", &self.fonts.small, 20.0 * s)?;
                    items.push(Item::Text(t, pad, y, DIM));
                    y += 22.0 * s;
                    let glyph = ui::volume_glyph(level.volume, level.muted);
                    slider(&mut items, Target::Master(Flow::Output), glyph, level.volume, level.muted, y)?;
                    y += 44.0 * s;
                }
                if self.apps.is_empty() {
                    let t = gfx.text("No apps are using audio", &self.fonts.text, 30.0 * s)?;
                    items.push(Item::Text(t, pad + 4.0 * s, y, DIM));
                    y += 30.0 * s;
                }
                for (i, app) in self.apps.iter().take(MAX_ROWS).enumerate() {
                    let t = gfx.text_fit(&app.name, &self.fonts.small, inner, 20.0 * s)?;
                    items.push(Item::Text(t, pad, y, FG));
                    y += 22.0 * s;
                    let glyph = ui::volume_glyph(app.volume, app.muted);
                    slider(&mut items, Target::App(i), glyph, app.volume, app.muted, y)?;
                    y += 42.0 * s;
                }
                y -= 6.0 * s;
            }
        }
        y += pad;

        hits.extend(tab_hits);
        hits.extend(device_hits);
        self.hits = hits;
        self.tracks = tracks;
        Ok((items, y))
    }

    fn paint(&mut self) -> Result<()> {
        let (items, _) = self.layout()?;
        let Some(frame) = self.popup.begin() else { return Ok(()) };
        let frame = frame?;
        widgets::draw(&frame, &items);
        self.popup.gfx.end(frame)
    }
}
