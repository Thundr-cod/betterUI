// The top bar window.
//
// Left:   RAM usage, network connection
// Centre: date and time
// Right:  Bluetooth, control center, audio (later: system tray)
//
// The bar only repaints when something it shows has changed (at most once a
// second), so it uses almost no CPU while idle.

use std::cell::RefCell;

use windows::core::{w, Error, Result};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ScreenToClient,
    MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow,
    TrackPopupMenu, TranslateMessage, HWND_TOPMOST, IDC_ARROW, MF_CHECKED, MF_POPUP,
    MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, SWP_NOACTIVATE,
    SW_HIDE, SW_SHOWNA, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_ACTIVATE, WM_APP,
    WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_ERASEBKGND,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_TIMER,
    WM_WINDOWPOSCHANGED, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::appbar::{self, ABN_FULLSCREENAPP, ABN_POSCHANGED};
use crate::audio::{Audio, Flow, Level};
use crate::audio_flyout::{self, Tab, WM_AUDIO_CHANGED};
use crate::panel::{self, Kind};
use crate::ui::{self, R};
use crate::backdrop;
use crate::config::Config;
use crate::render::{Color, Gfx, Text, ICON_FONT, TEXT_FONT};
use crate::sysinfo::{self, Memory, Network};
use crate::{log, taskbar};

// ---- Look -----------------------------------------------------------------
const BAR_HEIGHT: f32 = 32.0; // logical pixels (scaled by DPI)
const BG_RGB: (f32, f32, f32) = (0.06, 0.06, 0.08);
const PILL: Color = Color(1.0, 1.0, 1.0, 0.07);
const FG: Color = Color(1.0, 1.0, 1.0, 0.92);
const DIM: Color = Color(1.0, 1.0, 1.0, 0.55);

// Segoe Fluent Icons glyphs
const ICON_WIFI: &str = "\u{E701}";
const ICON_ETHERNET: &str = "\u{E839}";
const ICON_BLUETOOTH: &str = "\u{E702}";
const ICON_CONTROL_CENTER: &str = "\u{E9E9}";

// ---- Messages / timers ----------------------------------------------------
const WM_APPBAR: u32 = WM_APP + 1;
const TIMER_TICK: usize = 1; // 1 s: clock + system info
const TIMER_TASKBAR: usize = 2; // keep the native taskbar hidden
const MENU_QUIT: usize = 1;
const MENU_BLUR: usize = 2;
const MENU_OPACITY_BASE: usize = 1000; // 1000 + percent
const OPACITY_CHOICES: [u32; 9] = [100, 90, 80, 70, 60, 50, 40, 30, 20];
const MA_NOACTIVATE: isize = 3;
// Used by the CI screenshot test: open a flyout right after start-up.
const WM_TEST_OPEN: u32 = WM_APP + 3;

thread_local! {
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
}

struct Formats {
    text: IDWriteTextFormat,
    bold: IDWriteTextFormat,
    icon: IDWriteTextFormat,
}

struct Bar {
    hwnd: HWND,
    config: Config,
    gfx: Gfx,
    fonts: Formats,
    dpi: u32,
    width: i32,
    height: i32,
    taskbar_created: u32,
    ticks: u64,
    memory: Memory,
    network: Network,
    clock: String,
    audio: Option<Audio>,
    volume: Option<Level>,
    audio_btn: R,
    bt_btn: R,
    cc_btn: R,
    origin: (i32, i32),
}

pub fn run() -> Result<()> {
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("betterUI.TopBar");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_win32());
        }
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("betterUI"),
            WS_POPUP,
            0,
            0,
            100,
            32,
            None,
            None,
            hinstance,
            None,
        )?;
        style_window(hwnd);

        let mut bar = Bar::new(hwnd)?;
        backdrop::set_blur(hwnd, bar.config.bar_blur);
        taskbar::hide();
        appbar::register(hwnd, WM_APPBAR);
        bar.reposition();
        BAR.with(|b| *b.borrow_mut() = Some(bar));

        let _ = ShowWindow(hwnd, SW_SHOWNA);
        SetTimer(hwnd, TIMER_TICK, 1000, None);
        SetTimer(hwnd, TIMER_TASKBAR, 300, None);

        // `--test-open=audio-devices` / `--test-open=audio-mixer`
        for arg in std::env::args() {
            let tab = match arg.as_str() {
                "--test-open=audio-devices" => Some(0),
                "--test-open=audio-mixer" => Some(1),
                "--test-open=bluetooth" => Some(2),
                "--test-open=control-center" => Some(3),
                _ => None,
            };
            if let Some(t) = tab {
                let _ = PostMessageW(hwnd, WM_TEST_OPEN, WPARAM(t), LPARAM(0));
            }
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// Dark mode, square corners, and per-pixel transparency (the blur itself
/// is set up in backdrop.rs).
fn style_window(hwnd: HWND) {
    unsafe {
        let dark: BOOL = TRUE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        let corners = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const _,
            std::mem::size_of_val(&corners) as u32,
        );
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // These must work even if the bar state is busy, so they don't touch it.
    match msg {
        WM_DESTROY => {
            appbar::unregister(hwnd);
            taskbar::restore();
            PostQuitMessage(0);
            return LRESULT(0);
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                appbar::unregister(hwnd);
                taskbar::restore();
            }
            return LRESULT(0);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE),
        WM_ERASEBKGND => return LRESULT(1),
        _ => {}
    }
    let handled = BAR.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|bar| bar.handle(msg, wp, lp)),
        Err(_) => None, // re-entrant call while we're busy: use the default
    });
    handled.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
}

impl Bar {
    fn new(hwnd: HWND) -> Result<Self> {
        let gfx = Gfx::new()?;
        let fonts = make_formats(&gfx, 1.0)?;
        let audio = Audio::new()
            .map_err(|e| log::write(&format!("audio unavailable: {e:?}")))
            .ok();
        let volume = audio.as_ref().and_then(|a| a.level(Flow::Output));
        Ok(Self {
            hwnd,
            config: Config::load(),
            gfx,
            fonts,
            dpi: 96,
            width: 0,
            height: 0,
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            ticks: 0,
            memory: sysinfo::memory(),
            network: sysinfo::network(),
            clock: clock_text(),
            audio,
            volume,
            audio_btn: R::default(),
            bt_btn: R::default(),
            cc_btn: R::default(),
            origin: (0, 0),
        })
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(self.hwnd, &mut ps);
                    if let Err(e) = self.paint() {
                        // The next clock tick repaints (after the render
                        // target has been recreated, if the GPU was reset).
                        log::write(&format!("paint failed: {e:?}"));
                    }
                    let _ = EndPaint(self.hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_TIMER => {
                match wp.0 {
                    TIMER_TICK => self.tick(),
                    TIMER_TASKBAR => taskbar::rehide_if_visible(),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_APPBAR => {
                match wp.0 {
                    ABN_POSCHANGED => self.reposition(),
                    ABN_FULLSCREENAPP => unsafe {
                        // Get out of the way of fullscreen games and videos.
                        let _ = ShowWindow(self.hwnd, if lp.0 != 0 { SW_HIDE } else { SW_SHOWNA });
                    },
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                self.reposition();
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                appbar::activated(self.hwnd);
                None
            }
            WM_WINDOWPOSCHANGED => {
                appbar::moved(self.hwnd);
                None
            }
            WM_RBUTTONUP => {
                self.context_menu();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let x = (lp.0 & 0xFFFF) as u16 as i16 as f32;
                let y = ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
                if self.audio_btn.contains(x, y) {
                    self.open_audio(None);
                } else if self.cc_btn.contains(x, y) {
                    self.open_panel(Kind::ControlCenter, false);
                } else if self.bt_btn.contains(x, y) {
                    self.open_panel(Kind::Bluetooth, false);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                // Scroll on the speaker button to change the volume.
                let mut pt = POINT {
                    x: (lp.0 & 0xFFFF) as u16 as i16 as i32,
                    y: ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
                };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                if self.audio_btn.contains(pt.x as f32, pt.y as f32) {
                    let notches = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as f32 / 120.0;
                    if let (Some(audio), Some(level)) = (&self.audio, self.volume) {
                        audio.set_volume(Flow::Output, level.volume + notches * 0.02);
                    }
                    self.update_volume(true);
                }
                Some(LRESULT(0))
            }
            WM_AUDIO_CHANGED => {
                self.update_volume(true);
                Some(LRESULT(0))
            }
            WM_TEST_OPEN => {
                match wp.0 {
                    0 => self.open_audio(Some(Tab::Devices)),
                    1 => self.open_audio(Some(Tab::Mixer)),
                    2 => self.open_panel(Kind::Bluetooth, true),
                    _ => self.open_panel(Kind::ControlCenter, true),
                }
                Some(LRESULT(0))
            }
            m if m != 0 && m == self.taskbar_created => {
                // Explorer restarted: it re-created the taskbar and forgot us.
                taskbar::hide();
                appbar::register(self.hwnd, WM_APPBAR);
                self.reposition();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn tick(&mut self) {
        self.ticks += 1;
        let mut changed = false;
        let clock = clock_text();
        if clock != self.clock {
            self.clock = clock;
            changed = true;
        }
        if self.ticks % 2 == 0 {
            let memory = sysinfo::memory();
            // Only redraw when the shown value (one decimal) changes.
            if (memory.used_gb * 10.0).round() != (self.memory.used_gb * 10.0).round() {
                changed = true;
            }
            self.memory = memory;
            let network = sysinfo::network();
            if network != self.network {
                self.network = network;
                changed = true;
            }
        }
        changed |= self.update_volume(false);
        if changed {
            unsafe {
                let _ = InvalidateRect(self.hwnd, None, false);
            }
        }
    }

    /// Re-read the output volume. Returns true if the icon needs redrawing
    /// (and redraws right away when `redraw` is set).
    fn update_volume(&mut self, redraw: bool) -> bool {
        let level = self.audio.as_ref().and_then(|a| a.level(Flow::Output));
        let glyph_of = |l: Option<Level>| l.map(|l| ui::volume_glyph(l.volume, l.muted));
        let changed = glyph_of(level) != glyph_of(self.volume);
        self.volume = level;
        if changed && redraw {
            unsafe {
                let _ = InvalidateRect(self.hwnd, None, false);
            }
        }
        changed
    }

    /// Screen rectangle of a bar button, for placing its flyout. Before the
    /// first paint (only possible right after start-up) the buttons have no
    /// position yet, so fall back to the right end of the bar.
    fn anchor(&self, b: R) -> RECT {
        let b = if b.w > 0.0 {
            b
        } else {
            let s = self.scale();
            R::new(self.width as f32 - 50.0 * s, 0.0, 44.0 * s, self.height as f32)
        };
        RECT {
            left: self.origin.0 + b.x as i32,
            top: self.origin.1 + b.y as i32,
            right: self.origin.0 + (b.x + b.w) as i32,
            bottom: self.origin.1 + self.height,
        }
    }

    fn open_audio(&mut self, tab: Option<Tab>) {
        panel::hide();
        audio_flyout::toggle(self.anchor(self.audio_btn), self.hwnd, self.config.bar_opacity, tab);
    }

    fn open_panel(&mut self, kind: Kind, force_open: bool) {
        audio_flyout::hide();
        let b = if kind == Kind::Bluetooth { self.bt_btn } else { self.cc_btn };
        panel::toggle(kind, self.anchor(b), self.hwnd, self.config.bar_opacity, force_open);
    }

    /// Size and place the bar across the top of the primary monitor.
    fn reposition(&mut self) {
        unsafe {
            let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let _ = GetMonitorInfoW(hmon, &mut mi);

            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            if dx != self.dpi && dx > 0 {
                self.dpi = dx;
                match make_formats(&self.gfx, self.scale()) {
                    Ok(f) => self.fonts = f,
                    Err(e) => log::write(&format!("font setup failed: {e:?}")),
                }
            }

            let height = (BAR_HEIGHT * self.scale()).round() as i32;
            let rc = appbar::reserve_top(self.hwnd, mi.rcMonitor, height);
            self.origin = (rc.left, rc.top);
            self.width = rc.right - rc.left;
            self.height = rc.bottom - rc.top;
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                rc.left,
                rc.top,
                self.width,
                self.height,
                SWP_NOACTIVATE,
            );
            self.gfx.resize(self.width as u32, self.height as u32);
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn context_menu(&mut self) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            if let Ok(sub) = CreatePopupMenu() {
                for pct in OPACITY_CHOICES {
                    let check = if pct == self.config.bar_opacity { MF_CHECKED } else { MF_UNCHECKED };
                    let label = windows::core::HSTRING::from(format!("{pct}%"));
                    let _ = AppendMenuW(sub, MF_STRING | check, MENU_OPACITY_BASE + pct as usize, &label);
                }
                let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Bar opacity"));
            }
            let blur_check = if self.config.bar_blur { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING | blur_check, MENU_BLUR, w!("Blur behind bar"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit betterUI (restores taskbar)"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu); // also destroys the submenu
            let cmd = cmd.0 as usize;
            if cmd == MENU_QUIT {
                let _ = PostMessageW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            } else if cmd == MENU_BLUR {
                self.config.bar_blur = !self.config.bar_blur;
                backdrop::set_blur(self.hwnd, self.config.bar_blur);
                self.config.save();
                let _ = InvalidateRect(self.hwnd, None, false);
            } else if cmd > MENU_OPACITY_BASE && cmd <= MENU_OPACITY_BASE + 100 {
                self.config.bar_opacity = (cmd - MENU_OPACITY_BASE) as u32;
                self.config.save();
                let _ = InvalidateRect(self.hwnd, None, false);
            }
        }
    }

    fn paint(&mut self) -> Result<()> {
        if self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        let s = self.scale();
        let (w, h) = (self.width as f32, self.height as f32);
        let pill_y = (5.0 * s).round();
        let pill_h = h - 2.0 * pill_y;

        // Build all text first (needs only &self), then draw.
        let mem_label = self.gfx.text("RAM", &self.fonts.bold, pill_h)?;
        let mem_value = self.gfx.text(
            &format!("{:.1} / {:.0} GB", self.memory.used_gb, self.memory.total_gb),
            &self.fonts.text,
            pill_h,
        )?;
        let (net_icon, net_label, net_color) = match self.network {
            Network::Wifi => (ICON_WIFI, "Wi-Fi", FG),
            Network::Ethernet => (ICON_ETHERNET, "Ethernet", FG),
            Network::Other => (ICON_ETHERNET, "Connected", FG),
            Network::Offline => (ICON_WIFI, "Offline", DIM),
        };
        let net_icon = self.gfx.text(net_icon, &self.fonts.icon, pill_h)?;
        let net_label = self.gfx.text(net_label, &self.fonts.text, pill_h)?;
        let clock = self.gfx.text(&self.clock, &self.fonts.bold, pill_h)?;
        let speaker = match self.volume {
            Some(l) => ui::volume_glyph(l.volume, l.muted),
            None => ui::ICON_MUTE,
        };
        let speaker = self.gfx.text(speaker, &self.fonts.icon, pill_h)?;
        let bt_icon = self.gfx.text(ICON_BLUETOOTH, &self.fonts.icon, pill_h)?;
        let cc_icon = self.gfx.text(ICON_CONTROL_CENTER, &self.fonts.icon, pill_h)?;

        let frame = self.gfx.begin(self.hwnd, self.width as u32, self.height as u32)?;
        let bg = Color(BG_RGB.0, BG_RGB.1, BG_RGB.2, self.config.bar_opacity as f32 / 100.0);
        frame.round_rect(0.0, 0.0, w, h, 0.0, bg);

        let pad = 10.0 * s;
        let gap = 6.0 * s;
        let radius = pill_h / 2.0;
        let pill = |x: f32, parts: &[(&Text, Color)]| -> f32 {
            let inner: f32 = parts.iter().map(|(t, _)| t.width).sum::<f32>()
                + gap * (parts.len().saturating_sub(1)) as f32;
            let width = inner + 2.0 * pad;
            frame.round_rect(x, pill_y, width, pill_h, radius, PILL);
            let mut cx = x + pad;
            for (t, c) in parts {
                frame.text(t, cx, pill_y, *c);
                cx += t.width + gap;
            }
            width
        };

        // Left side
        let mut x = 6.0 * s;
        x += pill(x, &[(&mem_label, DIM), (&mem_value, FG)]) + gap;
        pill(x, &[(&net_icon, net_color), (&net_label, net_color)]);

        // Centre
        frame.text(&clock, ((w - clock.width) / 2.0).round(), pill_y, FG);

        // Right side
        let speaker_w = speaker.width + 2.0 * pad;
        let speaker_x = w - 6.0 * s - speaker_w;
        let speaker_color = if self.volume.is_some() { FG } else { DIM };
        pill(speaker_x, &[(&speaker, speaker_color)]);
        let audio_btn = R::new(speaker_x, 0.0, speaker_w, h);
        let cc_w = cc_icon.width + 2.0 * pad;
        let cc_x = speaker_x - gap - cc_w;
        pill(cc_x, &[(&cc_icon, FG)]);
        let cc_btn = R::new(cc_x, 0.0, cc_w, h);
        let bt_w = bt_icon.width + 2.0 * pad;
        let bt_x = cc_x - gap - bt_w;
        pill(bt_x, &[(&bt_icon, FG)]);
        let bt_btn = R::new(bt_x, 0.0, bt_w, h);

        let result = self.gfx.end(frame);
        self.audio_btn = audio_btn;
        self.cc_btn = cc_btn;
        self.bt_btn = bt_btn;
        result
    }
}

fn make_formats(gfx: &Gfx, scale: f32) -> Result<Formats> {
    Ok(Formats {
        text: gfx.text_format(TEXT_FONT, 12.5 * scale, 400)?,
        bold: gfx.text_format(TEXT_FONT, 12.5 * scale, 600)?,
        icon: gfx.text_format(ICON_FONT, 13.0 * scale, 400)?,
    })
}

fn clock_text() -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let t = unsafe { GetLocalTime() };
    let hour12 = match t.wHour % 12 {
        0 => 12,
        h => h,
    };
    let ampm = if t.wHour < 12 { "AM" } else { "PM" };
    format!(
        "{} {} {}   {}:{:02} {}",
        DAYS[t.wDayOfWeek as usize % 7],
        MONTHS[(t.wMonth as usize + 11) % 12],
        t.wDay,
        hour12,
        t.wMinute,
        ampm
    )
}
