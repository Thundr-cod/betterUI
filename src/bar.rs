// The top bar window.
//
// Left:   RAM usage, network connection
// Centre: date and time
// Right:  system tray, Bluetooth, control center, audio
//
// There is one bar per monitor; they're rebuilt when monitors are added,
// removed or rearranged. The dock (dock.rs) is a separate window started
// alongside the bars, on the main monitor.
//
// The bar only repaints when something it shows has changed (at most once a
// second), so it uses almost no CPU while idle.

use std::cell::{Cell, RefCell};

use windows::core::{w, Error, Result};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, EnumDisplayMonitors, GetMonitorInfoW, InvalidateRect, MonitorFromRect,
    ScreenToClient, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, KillTimer,
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
use crate::{tray, tray_flyout};
use crate::ui::{self, R};
use crate::backdrop;
use crate::apps;
use crate::config::{Config, DockMode};
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
const ICON_TRAY: &str = "\u{E70D}";

// ---- Messages / timers ----------------------------------------------------
const WM_APPBAR: u32 = WM_APP + 1;
const TIMER_TICK: usize = 1; // 1 s: clock + system info
const TIMER_TASKBAR: usize = 2; // keep the native taskbar hidden
const TIMER_REBUILD: usize = 3; // monitors changed (debounced)
const TIMER_POLL: usize = 4; // 100 ms: auto-hide (mouse at top edge, windows covering)
const TIMER_ANIM: usize = 5; // ~60 fps while sliding in or out
const HIDE_DELAY_TICKS: u32 = 4; // wait 400 ms before hiding
const MENU_QUIT: usize = 1;
const MENU_BLUR: usize = 2;
const MENU_AUTOSTART: usize = 3;
const MENU_BAR_ALWAYS: usize = 21;
const MENU_BAR_AUTOHIDE: usize = 22;
const MENU_BAR_SMART: usize = 23;
const MENU_OPACITY_BASE: usize = 1000; // 1000 + percent
const OPACITY_CHOICES: [u32; 9] = [100, 90, 80, 70, 60, 50, 40, 30, 20];
const MA_NOACTIVATE: isize = 3;
// Used by the CI screenshot test: open a flyout right after start-up.
const WM_TEST_OPEN: u32 = WM_APP + 3;
/// Settings changed from one bar's menu: every bar reloads them.
const WM_CONFIG_CHANGED: u32 = WM_APP + 4;
const MONITORINFOF_PRIMARY: u32 = 1;

thread_local! {
    /// One bar per monitor; the main monitor's bar is first.
    static BARS: RefCell<Vec<Bar>> = const { RefCell::new(Vec::new()) };
    /// Window handles of all bars, readable while BARS is busy.
    static BAR_HWNDS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    /// Set when monitors changed; the bars are rebuilt from the message loop.
    static REBUILD: Cell<bool> = const { Cell::new(false) };
    /// Replace the bars even if the monitors look the same (CI test).
    static FORCE_REBUILD: Cell<bool> = const { Cell::new(false) };
}

/// A monitor: its full rectangle and whether it's the main one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Monitor {
    rect: RECT,
    primary: bool,
}

fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(hmon: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<Monitor>);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            list.push(Monitor { rect: mi.rcMonitor, primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0 });
        }
        TRUE
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut list as *mut Vec<Monitor> as isize));
    }
    // Main monitor first, then left to right.
    list.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
    list
}

struct Formats {
    text: IDWriteTextFormat,
    bold: IDWriteTextFormat,
    icon: IDWriteTextFormat,
}

struct Bar {
    hwnd: HWND,
    monitor: Monitor,
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
    tray_btn: R,
    origin: (i32, i32),
    // Auto-hide (bar_mode autohide / smart). offset is 0 when shown and
    // negative (slid up above the screen edge) when hidden.
    offset: f32,
    target_hidden: bool,
    hide_ticks: u32,
    covered: bool,
    animating: bool,
    polls: u64,
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
        rebuild_bars(true);

        if Config::load().tray_enabled {
            tray::start(tray_flyout::changed);
        }
        crate::dock::create(std::env::args().any(|a| a == "--test-open=dock"));

        // `--test-open=...`: open a flyout from the main bar (CI screenshots).
        let main_bar = BAR_HWNDS.with(|h| h.borrow().first().copied());
        for arg in std::env::args() {
            let tab = match arg.as_str() {
                "--test-open=audio-devices" => Some(0),
                "--test-open=audio-mixer" => Some(1),
                "--test-open=bluetooth" => Some(2),
                "--test-open=control-center" => Some(3),
                "--test-open=tray" => Some(4),
                "--test-rebuild" => Some(9),
                _ => None,
            };
            if let (Some(t), Some(h)) = (tab, main_bar) {
                let _ = PostMessageW(h, WM_TEST_OPEN, WPARAM(t), LPARAM(0));
            }
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            // Rebuild here, outside any window's message handler.
            if REBUILD.with(|r| r.replace(false)) {
                rebuild_bars(FORCE_REBUILD.with(|f| f.replace(false)));
            }
        }

        // Quitting: remove every bar (each unregisters its screen space).
        let bars: Vec<Bar> = BARS.with(|b| std::mem::take(&mut *b.borrow_mut()));
        for bar in bars {
            let _ = DestroyWindow(bar.hwnd);
        }
    }
    Ok(())
}

/// Make sure there's exactly one bar per monitor. If the set of monitors is
/// unchanged, existing bars are just re-measured (resolution or scaling
/// changes); otherwise all bars are replaced.
fn rebuild_bars(first: bool) {
    let wanted = monitors();
    let current: Vec<Monitor> = BARS.with(|b| b.borrow().iter().map(|bar| bar.monitor).collect());
    if !first && current == wanted {
        BARS.with(|b| {
            for bar in b.borrow_mut().iter_mut() {
                bar.reposition();
            }
        });
        return;
    }
    log::write(&format!("monitors: {} -> {}", current.len(), wanted.len()));

    // Take the old bars out first: destroying a window sends it messages,
    // which must not find the list borrowed.
    let old: Vec<Bar> = BARS.with(|b| std::mem::take(&mut *b.borrow_mut()));
    for bar in old {
        unsafe {
            let _ = DestroyWindow(bar.hwnd);
        }
    }
    let mut new_bars = Vec::new();
    for m in wanted {
        match create_bar(m) {
            Ok(bar) => new_bars.push(bar),
            Err(e) => log::write(&format!("could not create bar for {:?}: {e:?}", m.rect)),
        }
    }
    let hwnds: Vec<HWND> = new_bars.iter().map(|b| b.hwnd).collect();
    BARS.with(|b| *b.borrow_mut() = new_bars);
    BAR_HWNDS.with(|h| *h.borrow_mut() = hwnds.clone());
    for (i, h) in hwnds.iter().enumerate() {
        unsafe {
            let _ = ShowWindow(*h, SW_SHOWNA);
            SetTimer(*h, TIMER_TICK, 1000, None);
            if i == 0 {
                SetTimer(*h, TIMER_TASKBAR, 300, None);
            }
        }
    }
    if first {
        taskbar::hide();
    }
}

/// Tell every bar to reload the settings (they're changed from one bar).
fn notify_config_changed() {
    BAR_HWNDS.with(|h| {
        for hwnd in h.borrow().iter() {
            unsafe {
                let _ = PostMessageW(*hwnd, WM_CONFIG_CHANGED, WPARAM(0), LPARAM(0));
            }
        }
    });
}

fn create_bar(monitor: Monitor) -> Result<Bar> {
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None)?.into();
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("betterUI.TopBar"),
            w!("betterUI"),
            WS_POPUP,
            monitor.rect.left,
            monitor.rect.top,
            100,
            32,
            None,
            None,
            hinstance,
            None,
        )?;
        style_window(hwnd);
        let mut bar = Bar::new(hwnd, monitor)?;
        backdrop::set_blur(hwnd, bar.config.bar_blur);
        if bar.config.bar_mode == DockMode::Always {
            appbar::register(hwnd, WM_APPBAR);
        } else {
            SetTimer(hwnd, TIMER_POLL, 100, None);
        }
        bar.reposition();
        Ok(bar)
    }
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
            // One bar going away (monitor removed, or quitting): give its
            // screen space back. Quitting itself is PostQuitMessage below.
            appbar::unregister(hwnd);
            return LRESULT(0);
        }
        WM_CLOSE => return LRESULT(0), // bars only close when betterUI quits
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
    let handled = BARS.with(|cell| match cell.try_borrow_mut() {
        Ok(mut bars) => bars.iter_mut().find(|b| b.hwnd == hwnd).and_then(|bar| bar.handle(msg, wp, lp)),
        Err(_) => None, // re-entrant call while we're busy: use the default
    });
    handled.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
}

impl Bar {
    fn new(hwnd: HWND, monitor: Monitor) -> Result<Self> {
        let gfx = Gfx::new()?;
        let fonts = make_formats(&gfx, 1.0)?;
        let audio = Audio::new()
            .map_err(|e| log::write(&format!("audio unavailable: {e:?}")))
            .ok();
        let volume = audio.as_ref().and_then(|a| a.level(Flow::Output));
        Ok(Self {
            hwnd,
            monitor,
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
            tray_btn: R::default(),
            origin: (0, 0),
            offset: 0.0,
            target_hidden: false,
            hide_ticks: 0,
            covered: false,
            animating: false,
            polls: 0,
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
                    TIMER_POLL => self.poll(),
                    TIMER_ANIM => self.animate(),
                    TIMER_REBUILD => unsafe {
                        let _ = KillTimer(self.hwnd, TIMER_REBUILD);
                        REBUILD.with(|r| r.set(true));
                    },
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
            WM_DPICHANGED => {
                self.reposition();
                Some(LRESULT(0))
            }
            WM_DISPLAYCHANGE => {
                // Monitors were added, removed or changed. Wait until things
                // settle (several of these arrive at once), then rebuild.
                unsafe {
                    SetTimer(self.hwnd, TIMER_REBUILD, 800, None);
                }
                Some(LRESULT(0))
            }
            WM_CONFIG_CHANGED => {
                let old_mode = self.config.bar_mode;
                self.config = Config::load();
                if self.config.bar_mode != old_mode {
                    self.apply_mode(old_mode);
                }
                backdrop::set_blur(self.hwnd, self.config.bar_blur);
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
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
                } else if self.tray_btn.contains(x, y) {
                    self.open_tray(false);
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
                    3 => self.open_panel(Kind::ControlCenter, true),
                    9 => unsafe {
                        // Same path as a monitor being plugged in/out, after 2 s.
                        FORCE_REBUILD.with(|f| f.set(true));
                        SetTimer(self.hwnd, TIMER_REBUILD, 2000, None);
                    },
                    _ => self.open_tray(true),
                }
                Some(LRESULT(0))
            }
            m if m != 0 && m == self.taskbar_created => {
                // Explorer restarted: it re-created the taskbar and forgot us.
                if self.monitor.primary {
                    taskbar::hide();
                }
                if self.config.bar_mode == DockMode::Always {
                    appbar::register(self.hwnd, WM_APPBAR);
                }
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
        tray_flyout::hide();
        audio_flyout::toggle(self.anchor(self.audio_btn), self.hwnd, self.config.bar_opacity, tab);
    }

    fn open_panel(&mut self, kind: Kind, force_open: bool) {
        audio_flyout::hide();
        tray_flyout::hide();
        let b = if kind == Kind::Bluetooth { self.bt_btn } else { self.cc_btn };
        panel::toggle(kind, self.anchor(b), self.hwnd, self.config.bar_opacity, force_open);
    }

    fn open_tray(&mut self, force_open: bool) {
        audio_flyout::hide();
        panel::hide();
        tray_flyout::toggle(self.anchor(self.tray_btn), self.config.bar_opacity, force_open);
    }

    // ---- Auto-hide ------------------------------------------------------------

    /// Switch between always-shown (reserves its strip) and auto-hide.
    fn apply_mode(&mut self, old: DockMode) {
        unsafe {
            if self.config.bar_mode == DockMode::Always {
                let _ = KillTimer(self.hwnd, TIMER_POLL);
                self.target_hidden = false;
                self.offset = 0.0;
                appbar::register(self.hwnd, WM_APPBAR);
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            } else {
                if old == DockMode::Always {
                    appbar::unregister(self.hwnd);
                }
                SetTimer(self.hwnd, TIMER_POLL, 100, None);
            }
        }
        self.reposition();
    }

    fn hidden_offset(&self) -> f32 {
        -(self.height as f32) - 2.0
    }

    fn poll(&mut self) {
        if self.config.bar_mode == DockMode::Always {
            return;
        }
        self.polls += 1;
        let bar = RECT {
            left: self.monitor.rect.left,
            top: self.monitor.rect.top,
            right: self.monitor.rect.right,
            bottom: self.monitor.rect.top + self.height,
        };
        if self.config.bar_mode == DockMode::Smart && self.polls % 3 == 0 {
            self.covered = apps::any_window_overlaps(&bar);
        }
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        let m = self.monitor.rect;
        let in_x = cursor.x >= m.left && cursor.x < m.right;
        let at_edge = in_x && cursor.y <= m.top + 1 && cursor.y >= m.top - 1;
        let visible = self.offset > self.hidden_offset() + 1.0;
        let over = visible && in_x && cursor.y >= m.top && cursor.y <= bar.bottom + 8;

        let want_visible = if apps::fullscreen_on(&m) {
            false
        } else if apps::own_flyout_in_front() {
            // One of the bar's flyouts is open: keep the bar with it.
            visible || at_edge
        } else {
            match self.config.bar_mode {
                DockMode::AutoHide => at_edge || over,
                _ => !self.covered || at_edge || over,
            }
        };
        if want_visible {
            self.hide_ticks = 0;
            self.set_target_hidden(false);
        } else {
            self.hide_ticks += 1;
            if self.hide_ticks >= HIDE_DELAY_TICKS {
                self.set_target_hidden(true);
            }
        }
    }

    fn set_target_hidden(&mut self, hidden: bool) {
        if hidden == self.target_hidden {
            return;
        }
        self.target_hidden = hidden;
        unsafe {
            if !hidden {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            }
            if !self.animating {
                self.animating = true;
                SetTimer(self.hwnd, TIMER_ANIM, 15, None);
            }
        }
    }

    fn animate(&mut self) {
        let target = if self.target_hidden { self.hidden_offset() } else { 0.0 };
        let diff = target - self.offset;
        if diff.abs() < 1.0 {
            self.offset = target;
            self.animating = false;
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_ANIM);
                if self.target_hidden {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
        } else {
            self.offset += diff * 0.35;
        }
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                self.origin.0,
                self.origin.1 + self.offset.round() as i32,
                self.width,
                self.height,
                SWP_NOACTIVATE,
            );
        }
    }

    /// Size and place the bar across the top of its monitor.
    fn reposition(&mut self) {
        unsafe {
            let hmon = MonitorFromRect(&self.monitor.rect, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let _ = GetMonitorInfoW(hmon, &mut mi);
            self.monitor.rect = mi.rcMonitor;

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
            let rc = if self.config.bar_mode == DockMode::Always {
                appbar::reserve_top(self.hwnd, mi.rcMonitor, height)
            } else {
                // Auto-hide: float over the top edge without reserving space.
                RECT { left: mi.rcMonitor.left, top: mi.rcMonitor.top, right: mi.rcMonitor.right, bottom: mi.rcMonitor.top + height }
            };
            self.origin = (rc.left, rc.top);
            self.width = rc.right - rc.left;
            self.height = rc.bottom - rc.top;
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                rc.left,
                rc.top + self.offset.round() as i32,
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
            if let Ok(sub) = CreatePopupMenu() {
                let mode = self.config.bar_mode;
                let check = |m: DockMode| if mode == m { MF_CHECKED } else { MF_UNCHECKED };
                let _ = AppendMenuW(sub, MF_STRING | check(DockMode::Always), MENU_BAR_ALWAYS, w!("Always show"));
                let _ = AppendMenuW(sub, MF_STRING | check(DockMode::AutoHide), MENU_BAR_AUTOHIDE, w!("Auto-hide (show at top edge)"));
                let _ = AppendMenuW(sub, MF_STRING | check(DockMode::Smart), MENU_BAR_SMART, w!("Hide when a window covers it"));
                let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Bar visibility"));
            }
            let blur_check = if self.config.bar_blur { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING | blur_check, MENU_BLUR, w!("Blur behind bar"));
            let start_check = if crate::autostart::is_enabled() { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING | start_check, MENU_AUTOSTART, w!("Start with Windows"));
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
                PostQuitMessage(0);
            } else if (MENU_BAR_ALWAYS..=MENU_BAR_SMART).contains(&cmd) {
                let mode = match cmd {
                    MENU_BAR_AUTOHIDE => DockMode::AutoHide,
                    MENU_BAR_SMART => DockMode::Smart,
                    _ => DockMode::Always,
                };
                // Every bar (including this one) picks it up from the settings.
                Config::update(|c| c.bar_mode = mode);
                notify_config_changed();
            } else if cmd == MENU_AUTOSTART {
                crate::autostart::set_enabled(!crate::autostart::is_enabled());
            } else if cmd == MENU_BLUR {
                let blur = !self.config.bar_blur;
                self.config = Config::update(|c| c.bar_blur = blur);
                notify_config_changed();
                backdrop::set_blur(self.hwnd, self.config.bar_blur);
                let _ = InvalidateRect(self.hwnd, None, false);
            } else if cmd > MENU_OPACITY_BASE && cmd <= MENU_OPACITY_BASE + 100 {
                let pct = (cmd - MENU_OPACITY_BASE) as u32;
                self.config = Config::update(|c| c.bar_opacity = pct);
                notify_config_changed();
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
        let tray_icon = if self.config.tray_enabled {
            Some(self.gfx.text(ICON_TRAY, &self.fonts.icon, pill_h)?)
        } else {
            None
        };

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
        let tray_btn = match &tray_icon {
            Some(t) => {
                let tw = t.width + 2.0 * pad;
                let tx = bt_x - gap - tw;
                pill(tx, &[(t, FG)]);
                R::new(tx, 0.0, tw, h)
            }
            None => R::default(),
        };

        let result = self.gfx.end(frame);
        self.audio_btn = audio_btn;
        self.cc_btn = cc_btn;
        self.bt_btn = bt_btn;
        self.tray_btn = tray_btn;
        let hook_name = if self.monitor.primary { "bar".to_string() } else { format!("bar_{}", self.monitor.rect.left) };
        crate::testhooks::write(
            &hook_name,
            self.hwnd,
            &[
                ("Tray".to_string(), tray_btn),
                ("Bluetooth".to_string(), bt_btn),
                ("ControlCenter".to_string(), cc_btn),
                ("Audio".to_string(), audio_btn),
            ],
        );
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
