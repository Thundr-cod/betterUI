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

use windows::core::{w, Result};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::DWMWCP_DONOTROUND;
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, MonitorFromRect, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, PostMessageW,
    PostQuitMessage, RegisterWindowMessageW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, HWND_TOPMOST,
    MSG, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNA, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_ENDSESSION, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_TIMER,
    WM_WINDOWPOSCHANGED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

use super::appbar::{self, ABN_FULLSCREENAPP, ABN_POSCHANGED};
use super::slide::Slide;
use super::{dock, taskbar, tray};
use crate::config::{Config, DockMode};
use crate::flyouts::audio::{self as audio_flyout, WM_AUDIO_CHANGED};
use crate::flyouts::panel::{self, Kind};
use crate::flyouts::tray as tray_flyout;
use crate::log;
use crate::system::apps;
use crate::system::audio::{Audio, Flow, Level};
use crate::system::sysinfo::{self, Memory, Network};
use crate::ui::render::{Color, Gfx, Text, ICON_FONT, TEXT_FONT};
use crate::ui::window::{self, Menu, MA_NOACTIVATE};
use crate::ui::{self, backdrop, R, DIM, FAINT, FG, ICON_BLUETOOTH, ICON_WIFI};

// ---- Look -----------------------------------------------------------------
const BAR_HEIGHT: f32 = 32.0; // logical pixels (scaled by DPI)
const BG_RGB: (f32, f32, f32) = (0.06, 0.06, 0.08);

// Segoe Fluent Icons glyphs
const ICON_RAM: &str = "\u{EEA0}"; // a memory module
const ICON_ETHERNET: &str = "\u{E839}";
const ICON_CONTROL_CENTER: &str = "\u{E9E9}";
const ICON_TRAY: &str = "\u{E70D}";

// ---- Messages / timers ----------------------------------------------------
const WM_APPBAR: u32 = WM_APP + 1;
/// Settings changed from one bar's menu: every bar reloads them.
const WM_CONFIG_CHANGED: u32 = WM_APP + 4;
const TIMER_TICK: usize = 1; // 1 s: clock + system info
const TIMER_TASKBAR: usize = 2; // keep the native taskbar hidden
const TIMER_REBUILD: usize = 3; // monitors changed (debounced)
const TIMER_POLL: usize = 4; // 100 ms: auto-hide (mouse at top edge, windows covering)
const TIMER_ANIM: usize = 5; // ~60 fps while sliding in or out
const MENU_QUIT: usize = 1;
const MENU_BLUR: usize = 2;
const MENU_AUTOSTART: usize = 3;
const MENU_BAR_ALWAYS: usize = 21;
const MENU_BAR_AUTOHIDE: usize = 22;
const MENU_BAR_SMART: usize = 23;
const MENU_OPACITY_BASE: usize = 1000; // 1000 + percent
const OPACITY_CHOICES: [u32; 9] = [100, 90, 80, 70, 60, 50, 40, 30, 20];

thread_local! {
    /// One bar per monitor; the main monitor's bar is first.
    static BARS: RefCell<Vec<Bar>> = const { RefCell::new(Vec::new()) };
    /// Window handles of all bars, readable while BARS is busy.
    static BAR_HWNDS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    /// Set when monitors changed; the bars are rebuilt from the message loop.
    static REBUILD: Cell<bool> = const { Cell::new(false) };
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
        if let Some(m) = window::monitor_info(hmon) {
            list.push(Monitor { rect: m.rect, primary: m.primary });
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
    /// Auto-hide (bar_mode autohide / smart): slides up above the screen edge.
    slide: Slide,
    covered: bool,
    polls: u64,
}

pub fn run() -> Result<()> {
    window::register_class(w!("betterUI.TopBar"), Some(wndproc))?;
    rebuild_bars(true);
    if Config::load().tray_enabled {
        tray::start(tray_flyout::changed);
    }
    dock::create();

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            // Rebuild here, outside any window's message handler.
            if REBUILD.with(|r| r.replace(false)) {
                rebuild_bars(false);
            }
        }

        // Quitting: remove the dock and every bar (each unregisters its screen space).
        dock::destroy();
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
    let hwnd = window::create(
        w!("betterUI.TopBar"),
        w!("betterUI"),
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        monitor.rect.left,
        monitor.rect.top,
        100,
        32,
    )?;
    window::style(hwnd, DWMWCP_DONOTROUND);
    let mut bar = Bar::new(hwnd, monitor)?;
    backdrop::set_blur(hwnd, bar.config.bar_blur);
    if bar.config.bar_mode == DockMode::Always {
        appbar::register(hwnd, WM_APPBAR);
    } else {
        unsafe {
            SetTimer(hwnd, TIMER_POLL, 100, None);
        }
    }
    bar.reposition();
    Ok(bar)
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
            slide: Slide::new(hwnd, TIMER_ANIM),
            covered: false,
            polls: 0,
        })
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                let hwnd = self.hwnd;
                window::paint(hwnd, "bar", || self.paint());
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
                // Unreadable file: keep the current settings instead of defaults.
                if let Some(c) = Config::load_checked() {
                    self.config = c;
                }
                if self.config.bar_mode != old_mode {
                    self.apply_mode(old_mode);
                }
                backdrop::set_blur(self.hwnd, self.config.bar_blur);
                window::invalidate(self.hwnd);
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
                let (x, y) = window::client_point(lp);
                if self.audio_btn.contains(x, y) {
                    self.open_audio();
                } else if self.cc_btn.contains(x, y) {
                    self.open_panel(Kind::ControlCenter);
                } else if self.bt_btn.contains(x, y) {
                    self.open_panel(Kind::Bluetooth);
                } else if self.tray_btn.contains(x, y) {
                    self.open_tray();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                // Scroll on the speaker button to change the volume.
                let (x, y) = window::wheel_point(self.hwnd, lp);
                if self.audio_btn.contains(x, y) {
                    if let (Some(audio), Some(level)) = (&self.audio, self.volume) {
                        audio.set_volume(Flow::Output, level.volume + window::wheel_notches(wp) * 0.02);
                    }
                    self.update_volume(true);
                }
                Some(LRESULT(0))
            }
            WM_AUDIO_CHANGED => {
                self.update_volume(true);
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
        if self.ticks.is_multiple_of(2) {
            let (memory, network) = shared_readouts();
            // Only redraw when the shown value (one decimal) changes.
            if (memory.used_gb * 10.0).round() != (self.memory.used_gb * 10.0).round() {
                changed = true;
            }
            self.memory = memory;
            if network != self.network {
                self.network = network;
                changed = true;
            }
        }
        changed |= self.update_volume(false);
        if changed {
            window::invalidate(self.hwnd);
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
            window::invalidate(self.hwnd);
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

    fn open_audio(&mut self) {
        panel::hide();
        tray_flyout::hide();
        audio_flyout::toggle(self.anchor(self.audio_btn), self.hwnd, self.config.bar_opacity);
    }

    fn open_panel(&mut self, kind: Kind) {
        audio_flyout::hide();
        tray_flyout::hide();
        let b = if kind == Kind::Bluetooth { self.bt_btn } else { self.cc_btn };
        panel::toggle(kind, self.anchor(b), self.hwnd, self.config.bar_opacity);
    }

    fn open_tray(&mut self) {
        audio_flyout::hide();
        panel::hide();
        tray_flyout::toggle(self.anchor(self.tray_btn), self.config.bar_opacity);
    }

    // ---- Auto-hide ------------------------------------------------------------

    /// Switch between always-shown (reserves its strip) and auto-hide.
    fn apply_mode(&mut self, old: DockMode) {
        unsafe {
            if self.config.bar_mode == DockMode::Always {
                let _ = KillTimer(self.hwnd, TIMER_POLL);
                self.slide.jump(false, self.hidden_offset());
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
        let m = self.monitor.rect;
        let bar = RECT { left: m.left, top: m.top, right: m.right, bottom: m.top + self.height };
        if self.config.bar_mode == DockMode::Smart && self.polls.is_multiple_of(3) {
            self.covered = apps::any_window_overlaps(&bar);
        }
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        let in_x = cursor.x >= m.left && cursor.x < m.right;
        let at_edge = in_x && cursor.y <= m.top + 1 && cursor.y >= m.top - 1;
        let visible = self.slide.shown(self.hidden_offset());
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
        self.slide.want_visible(want_visible);
    }

    fn animate(&mut self) {
        self.slide.step(self.hidden_offset());
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                self.origin.0,
                self.origin.1 + self.slide.offset.round() as i32,
                self.width,
                self.height,
                SWP_NOACTIVATE,
            );
        }
    }

    /// Size and place the bar across the top of its monitor.
    fn reposition(&mut self) {
        let hmon = unsafe { MonitorFromRect(&self.monitor.rect, MONITOR_DEFAULTTONEAREST) };
        let Some(m) = window::monitor_info(hmon) else { return };
        self.monitor.rect = m.rect;
        if m.dpi != self.dpi {
            self.dpi = m.dpi;
            match make_formats(&self.gfx, self.scale()) {
                Ok(f) => self.fonts = f,
                Err(e) => log::write(&format!("font setup failed: {e:?}")),
            }
        }

        let height = (BAR_HEIGHT * self.scale()).round() as i32;
        let rc = if self.config.bar_mode == DockMode::Always {
            appbar::reserve_top(self.hwnd, m.rect, height)
        } else {
            // Auto-hide: float over the top edge without reserving space.
            RECT { left: m.rect.left, top: m.rect.top, right: m.rect.right, bottom: m.rect.top + height }
        };
        self.origin = (rc.left, rc.top);
        self.width = rc.right - rc.left;
        self.height = rc.bottom - rc.top;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                rc.left,
                rc.top + self.slide.offset.round() as i32,
                self.width,
                self.height,
                SWP_NOACTIVATE,
            );
        }
        self.gfx.resize(self.width as u32, self.height as u32);
        window::invalidate(self.hwnd);
    }

    fn context_menu(&mut self) {
        let Some(menu) = Menu::new() else { return };
        if let Some(sub) = Menu::new() {
            for pct in OPACITY_CHOICES {
                sub.check(MENU_OPACITY_BASE + pct as usize, &format!("{pct}%"), pct == self.config.bar_opacity);
            }
            menu.submenu("Bar opacity", sub);
        }
        if let Some(sub) = Menu::new() {
            let mode = self.config.bar_mode;
            sub.check(MENU_BAR_ALWAYS, "Always show", mode == DockMode::Always);
            sub.check(MENU_BAR_AUTOHIDE, "Auto-hide (show at top edge)", mode == DockMode::AutoHide);
            sub.check(MENU_BAR_SMART, "Hide when a window covers it", mode == DockMode::Smart);
            menu.submenu("Bar visibility", sub);
        }
        menu.check(MENU_BLUR, "Blur behind bar", self.config.bar_blur);
        menu.check(MENU_AUTOSTART, "Start with Windows", crate::autostart::is_enabled());
        menu.separator();
        menu.item(MENU_QUIT, "Quit betterUI (restores taskbar)");

        let cmd = menu.track(self.hwnd);
        if cmd == MENU_QUIT {
            taskbar::set_quitting();
            unsafe { PostQuitMessage(0) };
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
            self.config.bar_blur = blur;
            Config::update(|c| c.bar_blur = blur);
            notify_config_changed();
            backdrop::set_blur(self.hwnd, blur);
            window::invalidate(self.hwnd);
        } else if cmd > MENU_OPACITY_BASE && cmd <= MENU_OPACITY_BASE + 100 {
            let pct = (cmd - MENU_OPACITY_BASE) as u32;
            self.config.bar_opacity = pct;
            Config::update(|c| c.bar_opacity = pct);
            notify_config_changed();
            window::invalidate(self.hwnd);
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
        let mem_label = self.gfx.text(ICON_RAM, &self.fonts.icon, pill_h)?;
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
            frame.round_rect(x, pill_y, width, pill_h, radius, FAINT);
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

        // Right side, from the right edge inwards: one-icon buttons.
        let mut right = w - 6.0 * s;
        let mut button = |icon: &Text, color: Color| -> R {
            let bw = icon.width + 2.0 * pad;
            right -= bw;
            pill(right, &[(icon, color)]);
            let r = R::new(right, 0.0, bw, h);
            right -= gap;
            r
        };
        let audio_btn = button(&speaker, if self.volume.is_some() { FG } else { DIM });
        let cc_btn = button(&cc_icon, FG);
        let bt_btn = button(&bt_icon, FG);
        let tray_btn = tray_icon.as_ref().map(|t| button(t, FG)).unwrap_or_default();

        let result = self.gfx.end(frame);
        self.audio_btn = audio_btn;
        self.cc_btn = cc_btn;
        self.bt_btn = bt_btn;
        self.tray_btn = tray_btn;
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

/// Memory and network, read once for all bars: every monitor's bar ticks on
/// this thread within the same second, so the first read is reused.
fn shared_readouts() -> (Memory, Network) {
    use std::time::{Duration, Instant};
    thread_local! {
        static LAST: Cell<Option<(Instant, Memory, Network)>> = const { Cell::new(None) };
    }
    LAST.with(|last| {
        if let Some((at, m, n)) = last.get() {
            if at.elapsed() < Duration::from_millis(900) {
                return (m, n);
            }
        }
        let (m, n) = (sysinfo::memory(), sysinfo::network());
        last.set(Some((Instant::now(), m, n)));
        (m, n)
    })
}

/// "Tue Nov 17   4:09 PM", in the user's language and Windows time format
/// (12- or 24-hour).
fn clock_text() -> String {
    use windows::core::PCWSTR;
    use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, ENUM_DATE_FORMATS_FLAGS, TIME_NOSECONDS};
    let t = unsafe { GetLocalTime() };
    let (mut date, mut time) = ([0u16; 64], [0u16; 64]);
    let (d, h) = unsafe {
        (
            // Null locale = the user's own; the picture keeps the bar's short layout.
            GetDateFormatEx(PCWSTR::null(), ENUM_DATE_FORMATS_FLAGS(0), Some(&t), w!("ddd MMM d"), Some(&mut date), PCWSTR::null()),
            GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(&t), PCWSTR::null(), Some(&mut time)),
        )
    };
    if d <= 0 || h <= 0 {
        return format!("{}:{:02}", t.wHour, t.wMinute);
    }
    // The counts include the closing null.
    let text = |buf: &[u16], n: i32| String::from_utf16_lossy(&buf[..(n as usize).saturating_sub(1)]);
    format!("{}   {}", text(&date, d), text(&time, h))
}
