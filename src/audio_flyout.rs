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
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromRect, ScreenToClient,
    MONITORINFO, MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, IsWindowVisible, KillTimer, LoadCursorW, PostMessageW,
    RegisterClassW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST,
    IDC_ARROW, SWP_NOACTIVATE, SW_HIDE, SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_TIMER,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::audio::{App, Audio, Device, Flow, Level};
use crate::backdrop;
use crate::render::{Color, Gfx, Text, ICON_FONT, TEXT_FONT};
use crate::ui::{self, R, DIM, FAINT, FG, HOVER, ICON_CHECK, ICON_MIC, ICON_MUTE};
use crate::log;

const WIDTH: f32 = 340.0; // logical pixels
const MAX_ROWS: usize = 10; // per device list / mixer, keeps the flyout on screen
const TIMER_REFRESH: usize = 1;

/// Posted to the bar after a change so its speaker icon updates at once.
pub const WM_AUDIO_CHANGED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
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

enum Item {
    Rect(R, f32, Color),
    Circle(f32, f32, f32, Color),
    Text(Text, f32, f32, Color),
}

struct Fonts {
    text: IDWriteTextFormat,
    bold: IDWriteTextFormat,
    small: IDWriteTextFormat,
    icon: IDWriteTextFormat,
}

struct Flyout {
    hwnd: HWND,
    notify: HWND,
    gfx: Gfx,
    fonts: Fonts,
    dpi: u32,
    audio: Audio,
    tab: Tab,
    outputs: Vec<Device>,
    inputs: Vec<Device>,
    out_level: Option<Level>,
    in_level: Option<Level>,
    apps: Vec<App>,
    accent: Color,
    opacity: f32,
    width: i32,
    height: i32,
    hits: Vec<(R, Hit)>,
    tracks: Vec<(Target, f32, f32)>, // slider target, track x, track width
    dragging: Option<Target>,
}

thread_local! {
    static FLYOUT: RefCell<Option<Flyout>> = const { RefCell::new(None) };
}

/// Show the flyout under `anchor` (screen rectangle of the bar button), or
/// hide it if it is already open. `notify` is told about volume changes.
pub fn toggle(anchor: RECT, notify: HWND, bar_opacity: u32, tab: Option<Tab>) {
    if let Err(e) = ensure_created(notify) {
        log::write(&format!("audio flyout could not be created: {e:?}"));
        return;
    }
    FLYOUT.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else { return };
        let Some(f) = guard.as_mut() else { return };
        let visible = unsafe { IsWindowVisible(f.hwnd).as_bool() };
        if visible && tab.is_none() {
            f.hide();
        } else {
            if let Some(t) = tab {
                f.tab = t;
            }
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

fn ensure_created(notify: HWND) -> Result<()> {
    let exists = FLYOUT.with(|c| c.borrow().is_some());
    if exists {
        return Ok(());
    }
    unsafe {
        let hinstance = GetModuleHandleW(None)?.into();
        let class = w!("betterUI.AudioFlyout");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("betterUI audio"),
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
        let corners = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const _,
            std::mem::size_of_val(&corners) as u32,
        );
        let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        backdrop::set_blur(hwnd, true);

        let gfx = Gfx::new()?;
        let fonts = make_fonts(&gfx, 1.0)?;
        let flyout = Flyout {
            hwnd,
            notify,
            gfx,
            fonts,
            dpi: 96,
            audio: Audio::new()?,
            tab: Tab::Devices,
            outputs: Vec::new(),
            inputs: Vec::new(),
            out_level: None,
            in_level: None,
            apps: Vec::new(),
            accent: ui::accent(),
            opacity: 0.85,
            width: 0,
            height: 0,
            hits: Vec::new(),
            tracks: Vec::new(),
            dragging: None,
        };
        FLYOUT.with(|c| *c.borrow_mut() = Some(flyout));
    }
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
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    let handled = FLYOUT.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|f| f.handle(msg, wp, lp)),
        Err(_) => None,
    });
    handled.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
}

fn point_from(lp: LPARAM) -> (i32, i32) {
    let x = (lp.0 & 0xFFFF) as u16 as i16 as i32;
    let y = ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    (x, y)
}

impl Flyout {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn refresh(&mut self) {
        self.outputs = self.audio.devices(Flow::Output);
        self.inputs = self.audio.devices(Flow::Input);
        self.out_level = self.audio.level(Flow::Output);
        self.in_level = self.audio.level(Flow::Input);
        self.apps = self.audio.apps();
    }

    fn show(&mut self, anchor: RECT) {
        unsafe {
            let hmon = MonitorFromRect(&anchor, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(hmon, &mut mi);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            if dx != self.dpi && dx > 0 {
                self.dpi = dx;
                match make_fonts(&self.gfx, self.scale()) {
                    Ok(f) => self.fonts = f,
                    Err(e) => log::write(&format!("flyout fonts failed: {e:?}")),
                }
            }
            self.accent = ui::accent();
            self.refresh();
            let s = self.scale();
            self.width = (WIDTH * s).round() as i32;
            self.height = self.measure_height();

            let work = mi.rcWork;
            let gap = (6.0 * s).round() as i32;
            let mut x = anchor.right - self.width;
            x = x.clamp(work.left + gap, (work.right - self.width - gap).max(work.left));
            let y = work.top.max(anchor.bottom) + gap;

            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, self.width, self.height, SWP_NOACTIVATE);
            self.gfx.resize(self.width as u32, self.height as u32);
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            SetTimer(self.hwnd, TIMER_REFRESH, 1000, None);
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn hide(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_REFRESH);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.dragging = None;
    }

    /// Resize the window if the content height changed (devices or apps
    /// came or went).
    fn fit_height(&mut self) {
        let h = self.measure_height();
        if h != self.height {
            self.height = h;
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    self.width,
                    self.height,
                    SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
                );
            }
            self.gfx.resize(self.width as u32, self.height as u32);
        }
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(self.hwnd, &mut ps);
                    if let Err(e) = self.paint() {
                        log::write(&format!("flyout paint failed: {e:?}"));
                    }
                    let _ = EndPaint(self.hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                // Close when the user clicks anywhere else.
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
            WM_TIMER => {
                if self.dragging.is_none() {
                    self.refresh();
                    self.fit_height();
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = point_from(lp);
                self.mouse_down(x as f32, y as f32);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if let Some(t) = self.dragging {
                    let (x, _) = point_from(lp);
                    self.drag_to(t, x as f32);
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
                let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as f32 / 120.0;
                let (sx, sy) = point_from(lp);
                let mut pt = POINT { x: sx, y: sy };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let (x, y) = (pt.x as f32, pt.y as f32);
                let target = self.hits.iter().find_map(|(r, h)| match h {
                    Hit::Slider(t) | Hit::Mute(t) if r.contains(x, y) => Some(*t),
                    _ => None,
                });
                if let Some(t) = target {
                    let v = self.volume_of(t) + delta * 0.02;
                    self.set_volume(t, v);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn changed(&self) {
        self.invalidate();
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
                    self.invalidate();
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
                    SetCapture(self.hwnd);
                }
                self.drag_to(t, x);
            }
        }
    }

    fn drag_to(&mut self, t: Target, x: f32) {
        if let Some(&(_, tx, tw)) = self.tracks.iter().find(|(tt, _, _)| *tt == t) {
            if tw > 0.0 {
                self.set_volume(t, (x - tx) / tw);
            }
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
        let w = WIDTH * s;
        let pad = 14.0 * s;
        let inner = w - 2.0 * pad;
        let mut items: Vec<Item> = Vec::new();
        let mut hits: Vec<(R, Hit)> = Vec::new();
        let mut tracks: Vec<(Target, f32, f32)> = Vec::new();
        let mut y = pad;

        // Tabs
        let tab_h = 32.0 * s;
        items.push(Item::Rect(R::new(pad, y, inner, tab_h), 8.0 * s, FAINT));
        for (i, (tab, label)) in [(Tab::Devices, "Devices"), (Tab::Mixer, "Mixer")].iter().enumerate() {
            let half = inner / 2.0;
            let r = R::new(pad + half * i as f32, y, half, tab_h);
            let active = self.tab == *tab;
            if active {
                items.push(Item::Rect(R::new(r.x + 3.0 * s, r.y + 3.0 * s, r.w - 6.0 * s, r.h - 6.0 * s), 6.0 * s, HOVER));
            }
            let t = self.gfx.text(label, &self.fonts.bold, tab_h)?;
            let x = r.x + (r.w - t.width) / 2.0;
            items.push(Item::Text(t, x, y, if active { FG } else { DIM }));
            hits.push((r, Hit::Tab(*tab)));
        }
        y += tab_h + 12.0 * s;

        match self.tab {
            Tab::Devices => {
                for flow in [Flow::Output, Flow::Input] {
                    let (title, level, devices) = match flow {
                        Flow::Output => ("Output", self.out_level, &self.outputs),
                        Flow::Input => ("Input", self.in_level, &self.inputs),
                    };
                    let t = self.gfx.text(title, &self.fonts.small, 20.0 * s)?;
                    items.push(Item::Text(t, pad, y, DIM));
                    y += 22.0 * s;
                    if let Some(level) = level {
                        let glyph = match flow {
                            Flow::Output => ui::volume_glyph(level.volume, level.muted),
                            Flow::Input => {
                                if level.muted {
                                    ICON_MUTE
                                } else {
                                    ICON_MIC
                                }
                            }
                        };
                        self.slider_row(&mut items, &mut hits, &mut tracks, Target::Master(flow), glyph, level.volume, level.muted, y)?;
                        y += 40.0 * s;
                    }
                    if devices.is_empty() {
                        let t = self.gfx.text("No devices found", &self.fonts.text, 30.0 * s)?;
                        items.push(Item::Text(t, pad + 4.0 * s, y, DIM));
                        y += 30.0 * s;
                    }
                    for (i, dev) in devices.iter().take(MAX_ROWS).enumerate() {
                        let row = R::new(pad, y, inner, 32.0 * s);
                        if dev.is_default {
                            items.push(Item::Rect(row, 6.0 * s, FAINT));
                            let check = self.gfx.text(ICON_CHECK, &self.fonts.icon, row.h)?;
                            items.push(Item::Text(check, row.x + 10.0 * s, row.y, self.accent));
                        }
                        let name_x = row.x + 36.0 * s;
                        let t = self.gfx.text_fit(&dev.name, &self.fonts.text, row.w - 36.0 * s - 8.0 * s, row.h)?;
                        items.push(Item::Text(t, name_x, row.y, if dev.is_default { FG } else { DIM }));
                        hits.push((row, Hit::Device(flow, i)));
                        y += 34.0 * s;
                    }
                    y += 10.0 * s;
                }
                y -= 10.0 * s;
            }
            Tab::Mixer => {
                if let Some(level) = self.out_level {
                    let t = self.gfx.text("Master", &self.fonts.small, 20.0 * s)?;
                    items.push(Item::Text(t, pad, y, DIM));
                    y += 22.0 * s;
                    let glyph = ui::volume_glyph(level.volume, level.muted);
                    self.slider_row(&mut items, &mut hits, &mut tracks, Target::Master(Flow::Output), glyph, level.volume, level.muted, y)?;
                    y += 44.0 * s;
                }
                if self.apps.is_empty() {
                    let t = self.gfx.text("No apps are using audio", &self.fonts.text, 30.0 * s)?;
                    items.push(Item::Text(t, pad + 4.0 * s, y, DIM));
                    y += 30.0 * s;
                }
                let apps: Vec<(String, f32, bool)> = self
                    .apps
                    .iter()
                    .take(MAX_ROWS)
                    .map(|a| (a.name.clone(), a.volume, a.muted))
                    .collect();
                for (i, (name, volume, muted)) in apps.into_iter().enumerate() {
                    let t = self.gfx.text_fit(&name, &self.fonts.small, inner, 20.0 * s)?;
                    items.push(Item::Text(t, pad, y, FG));
                    y += 22.0 * s;
                    let glyph = ui::volume_glyph(volume, muted);
                    self.slider_row(&mut items, &mut hits, &mut tracks, Target::App(i), glyph, volume, muted, y)?;
                    y += 42.0 * s;
                }
                y -= 6.0 * s;
            }
        }
        y += pad;

        self.hits = hits;
        self.tracks = tracks;
        Ok((items, y))
    }

    /// [mute button] [=====o-----] [ 42 ]
    #[allow(clippy::too_many_arguments)]
    fn slider_row(
        &self,
        items: &mut Vec<Item>,
        hits: &mut Vec<(R, Hit)>,
        tracks: &mut Vec<(Target, f32, f32)>,
        target: Target,
        glyph: &str,
        volume: f32,
        muted: bool,
        y: f32,
    ) -> Result<()> {
        let s = self.scale();
        let pad = 14.0 * s;
        let w = WIDTH * s;
        let row_h = 32.0 * s;

        let btn = R::new(pad, y, row_h, row_h);
        items.push(Item::Rect(btn, 6.0 * s, FAINT));
        let icon = self.gfx.text(glyph, &self.fonts.icon, row_h)?;
        let ix = btn.x + (btn.w - icon.width) / 2.0;
        items.push(Item::Text(icon, ix, y, if muted { DIM } else { FG }));
        hits.push((btn, Hit::Mute(target)));

        let pct = self.gfx.text(&format!("{}", (volume * 100.0).round() as i32), &self.fonts.text, row_h)?;
        let pct_w = 34.0 * s;
        let tx = btn.x + btn.w + 14.0 * s;
        let tw = w - pad - pct_w - 10.0 * s - tx;
        let cy = y + row_h / 2.0;
        let v = volume.clamp(0.0, 1.0);
        let fill = if muted { DIM } else { self.accent };
        items.push(Item::Rect(R::new(tx, cy - 2.0 * s, tw, 4.0 * s), 2.0 * s, Color(1.0, 1.0, 1.0, 0.18)));
        if v > 0.0 {
            items.push(Item::Rect(R::new(tx, cy - 2.0 * s, tw * v, 4.0 * s), 2.0 * s, fill));
        }
        let knob_x = tx + tw * v;
        items.push(Item::Circle(knob_x, cy, 8.0 * s, Color(0.27, 0.27, 0.30, 1.0)));
        items.push(Item::Circle(knob_x, cy, 4.5 * s, fill));
        hits.push((R::new(tx - 10.0 * s, y, tw + 20.0 * s, row_h), Hit::Slider(target)));
        tracks.push((target, tx, tw));

        let px = w - pad - pct.width;
        items.push(Item::Text(pct, px, y, DIM));
        Ok(())
    }

    fn paint(&mut self) -> Result<()> {
        if self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        let (items, _) = self.layout()?;
        let frame = self.gfx.begin(self.hwnd, self.width as u32, self.height as u32)?;
        let bg = Color(0.10, 0.10, 0.12, self.opacity);
        frame.round_rect(0.0, 0.0, self.width as f32, self.height as f32, 0.0, bg);
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
