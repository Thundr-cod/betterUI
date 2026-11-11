// The dock: pinned and running apps at the bottom of the screen.
//
// Visibility (right-click the dock to change):
//   smart     shown while nothing overlaps it; hides when a window covers
//             its spot (e.g. a maximized window); comes back when the mouse
//             touches the bottom edge of the screen
//   autohide  hidden until the mouse touches the bottom edge
//   always    always shown
// It always stays hidden while a fullscreen app (game, video) is in front.
//
// Clicking an icon opens the app, focuses its window, or minimizes it if it
// is already in front. Middle-click opens a new window.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{w, Result, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, GetCursorPos,
    GetForegroundWindow, GetWindowThreadProcessId, IsIconic, KillTimer, LoadCursorW, PostMessageW, RegisterClassW,
    SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, TrackPopupMenu, HWND_TOPMOST,
    IDC_ARROW, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, SWP_NOACTIVATE, SW_HIDE,
    SW_MINIMIZE, SW_RESTORE, SW_SHOWNA, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_APP, WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONUP, WM_MBUTTONUP,
    WM_MOUSEACTIVATE, WM_PAINT, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::apps::{self, DockItem, Pin};
use crate::config::{Config, DockMode};
use crate::icons::Icons;
use crate::render::{Color, Gfx, TEXT_FONT};
use crate::ui::{self, R, DIM, FAINT, FG};
use crate::{log, popup};

const TIMER_POLL: usize = 1; // 100 ms: mouse, hover, visibility
const TIMER_ANIM: usize = 2; // ~60 fps while sliding in or out
const WM_ICON_READY: u32 = WM_APP + 0x31; // the icon loader finished one
const POLL_MS: u32 = 100;
const HIDE_DELAY_TICKS: u32 = 4; // wait 400 ms before hiding
const MA_NOACTIVATE: isize = 3;

const ICON: f32 = 30.0; // logical pixels
const SLOT: f32 = 42.0;
const START_OVERHANG: f32 = 14.0;
const PAD: f32 = 6.0;
const SEP: f32 = 17.0; // room for the divider between pinned and other apps
const MARGIN: f32 = 8.0; // gap between dock and screen bottom
/// The line between pinned apps and other running apps.
const DIVIDER: Color = Color(1.0, 1.0, 1.0, 0.35);
/// The Start button's logo colour (the Windows 11 taskbar's blue).
const START_BLUE: Color = Color(0.0, 0.62, 1.0, 1.0);

const MENU_NEW: usize = 1;
const MENU_PIN: usize = 2;
const MENU_CLOSE: usize = 3;
const MENU_MODE_SMART: usize = 11;
const MENU_MODE_AUTOHIDE: usize = 12;
const MENU_MODE_ALWAYS: usize = 13;
const MENU_START_BUTTON: usize = 21;

thread_local! {
    static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
}

struct Dock {
    hwnd: HWND,
    gfx: Gfx,
    letter_font: IDWriteTextFormat,
    icons: Option<Icons>,
    bitmaps: HashMap<String, Option<ID2D1Bitmap>>,
    /// When an icon lookup last came back empty, so it's retried later
    /// instead of never (a Store app's content attaches after its window
    /// opens, so the first lookup can miss).
    icon_misses: HashMap<String, std::time::Instant>,
    /// Loads icons off this thread (icon_loader.rs).
    loader: Option<crate::icon_loader::Loader>,
    /// When each kind of slow step was last logged (see `note_slow`).
    slow_logged: HashMap<&'static str, std::time::Instant>,
    /// Icons asked for and not back yet.
    icons_pending: std::collections::HashSet<String>,
    /// Loaded icons' pixels, kept so bitmaps can be remade cheaply when the
    /// render target is recreated.
    icon_pixels: HashMap<String, crate::icon_loader::Pixels>,
    bitmap_generation: u64,
    pins: Vec<Pin>,
    items: Vec<DockItem>,
    signature: String,
    mode: DockMode,
    forced: bool,
    opacity: f32,
    accent: Color,
    dpi: u32,
    monitor: RECT,
    width: i32,
    height: i32,
    base_x: i32,
    base_y: i32,
    offset: f32,
    target_hidden: bool,
    hide_ticks: u32,
    covered: bool,
    ticks: u64,
    hovered: Option<usize>,
    /// The Start button at the left end (setting `dock_start`), where it
    /// was last drawn, and whether the mouse is over it.
    start_button: bool,
    start_slot: Option<R>,
    start_hovered: bool,
    slots: Vec<R>,
    animating: bool,
    /// Running (unpinned) apps in the order they first appeared, so icons
    /// don't swap places when you switch between windows.
    running_order: Vec<String>,
}

/// Remove the dock right away when betterUI quits, instead of leaving it on
/// screen while the taskbar restore settles (about 3 s).
pub fn destroy() {
    crate::start_menu::uninstall();
    // Taken out first: DestroyWindow sends it messages, which must not find it borrowed.
    if let Some(dock) = DOCK.with(|d| d.borrow_mut().take()) {
        unsafe {
            let _ = DestroyWindow(dock.hwnd);
        }
    }
}

/// Create the dock (if enabled in settings, or `forced` for the CI test).
pub fn create(forced: bool) {
    let config = Config::load();
    if !config.dock_enabled && !forced {
        return;
    }
    if let Err(e) = create_inner(&config, forced) {
        log::write(&format!("dock could not be created: {e:?}"));
    }
}

fn create_inner(config: &Config, forced: bool) -> Result<()> {
    unsafe {
        let hinstance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?.into();
        let class = w!("betterUI.Dock");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("betterUI dock"),
            WS_POPUP,
            0,
            0,
            100,
            60,
            None,
            None,
            hinstance,
            None,
        )?;
        let margins = windows::Win32::UI::Controls::MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea(hwnd, &margins);
        popup::round_corners(hwnd);
        crate::backdrop::set_blur(hwnd, config.bar_blur);

        let gfx = Gfx::new()?;
        let letter_font = gfx.text_format(TEXT_FONT, 18.0, 600)?;
        let mut dock = Dock {
            hwnd,
            gfx,
            letter_font,
            icons: Icons::new().map_err(|e| log::write(&format!("icons unavailable: {e:?}"))).ok(),
            bitmaps: HashMap::new(),
            icon_misses: HashMap::new(),
            loader: None,
            slow_logged: HashMap::new(),
            icons_pending: std::collections::HashSet::new(),
            icon_pixels: HashMap::new(),
            bitmap_generation: 0,
            pins: apps::load_pins(),
            items: Vec::new(),
            signature: String::new(),
            mode: if forced { DockMode::Always } else { config.dock_mode },
            forced,
            opacity: config.bar_opacity.max(40) as f32 / 100.0,
            accent: ui::accent(),
            dpi: 0,
            monitor: RECT::default(),
            width: 0,
            height: 0,
            base_x: 0,
            base_y: 0,
            offset: 0.0,
            target_hidden: false,
            hide_ticks: 0,
            covered: false,
            ticks: 0,
            hovered: None,
            start_button: config.dock_start,
            start_slot: None,
            start_hovered: false,
            slots: Vec::new(),
            animating: false,
            running_order: Vec::new(),
        };
        dock.refresh_items();
        dock.reposition();
        // Start hidden in auto-hide mode, shown otherwise.
        if dock.mode == DockMode::AutoHide {
            dock.offset = dock.hidden_offset();
            dock.target_hidden = true;
        } else {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
        }
        dock.apply_offset();
        DOCK.with(|d| *d.borrow_mut() = Some(dock));
        crate::start_menu::install();
        SetTimer(hwnd, TIMER_POLL, POLL_MS, None);
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE),
        _ => {}
    }
    let handled = DOCK.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|d| d.handle(msg, wp, lp)),
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

impl Dock {
    fn scale(&self) -> f32 {
        self.dpi.max(96) as f32 / 96.0
    }

    fn hidden_offset(&self) -> f32 {
        self.height as f32 + MARGIN * self.scale() + 2.0
    }

    fn shown_rect(&self) -> RECT {
        RECT {
            left: self.base_x,
            top: self.base_y,
            right: self.base_x + self.width,
            bottom: self.base_y + self.height,
        }
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(self.hwnd, &mut ps);
                    let started = std::time::Instant::now();
                    let painted = self.paint();
                    self.note_slow("paint", started);
                    if let Err(e) = painted {
                        log::write(&format!("dock paint failed: {e:?}"));
                    }
                    let _ = EndPaint(self.hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_TIMER => {
                match wp.0 {
                    TIMER_POLL => self.poll(),
                    TIMER_ANIM => self.animate(),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_ICON_READY => {
                self.icons_ready();
                Some(LRESULT(0))
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                self.reposition();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let (x, y) = point_from(lp);
                if self.start_slot.map(|r| r.contains(x, y)).unwrap_or(false) {
                    open_start(false);
                } else if let Some(i) = self.slot_at(x, y) {
                    self.click(i);
                }
                Some(LRESULT(0))
            }
            WM_MBUTTONUP => {
                let (x, y) = point_from(lp);
                if let Some(i) = self.slot_at(x, y) {
                    if let Some(path) = self.items[i].launch.clone() {
                        popup::open_uri(&path);
                    }
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                let (x, y) = point_from(lp);
                if self.start_slot.map(|r| r.contains(x, y)).unwrap_or(false) {
                    // Like the Windows Start button: the Win+X menu.
                    open_start(true);
                    return Some(LRESULT(0));
                }
                let i = self.slot_at(x, y);
                self.context_menu(i);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn slot_at(&self, x: f32, y: f32) -> Option<usize> {
        self.slots.iter().position(|r| r.contains(x, y)).filter(|&i| i < self.items.len())
    }

    // ---- Contents -----------------------------------------------------------

    /// Log a dock step that blocked the dock's thread for 30 ms or more (at
    /// most once per 10 s per step), to find what makes the dock feel slow.
    fn note_slow(&mut self, step: &'static str, started: std::time::Instant) {
        let ms = started.elapsed().as_millis();
        if ms < 30 {
            return;
        }
        let now = std::time::Instant::now();
        if self.slow_logged.get(step).map(|t| now.duration_since(*t).as_secs() < 10).unwrap_or(false) {
            return;
        }
        self.slow_logged.insert(step, now);
        log::write(&format!("dock slow: {step} took {ms} ms ({} items)", self.items.len()));
    }

    /// Pins saved as a Store app's versioned exe path (older betterUI) break
    /// when the app updates. Once the app is running, re-save them by its
    /// AppUserModelID. `items` starts with the pins, in pin order.
    fn upgrade_store_pins(&mut self, items: &[DockItem]) {
        let mut changed = false;
        for (pin, item) in self.pins.iter_mut().zip(items) {
            if pin.path.to_lowercase().starts_with("shell:") || apps::package_family(&pin.path).is_none() {
                continue;
            }
            let Some(&hwnd) = item.windows.first() else { continue };
            let mut pid = 0u32;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            if let Some(id) = apps::process_app_id(pid) {
                log::write(&format!("dock pin moved to the Store app id {id}"));
                *pin = apps::pin_for_app_id(&id);
                changed = true;
            }
        }
        if changed {
            apps::save_pins(&self.pins);
        }
    }

    fn refresh_items(&mut self) {
        let started = std::time::Instant::now();
        let windows = apps::app_windows();
        self.note_slow("listing windows", started);
        let started = std::time::Instant::now();
        let items = apps::dock_items(&self.pins, &windows);
        self.upgrade_store_pins(&items);
        let items = self.stable_order(items);
        self.note_slow("matching windows to pins", started);
        let fg = unsafe { GetForegroundWindow() };
        let signature: String = items
            .iter()
            .map(|i| format!("{}:{}:{};", i.name, i.windows.len(), i.windows.contains(&fg)))
            .collect();
        let count_changed = items.len() != self.items.len()
            || items.iter().zip(&self.items).any(|(a, b)| a.pinned != b.pinned);
        self.items = items;
        if signature != self.signature {
            self.signature = signature;
            if count_changed && self.dpi != 0 {
                self.reposition();
            }
            self.invalidate();
        }
    }

    /// Windows are listed front-most first, which changes every time focus
    /// moves. Keep pinned apps in pin order and running apps in the order
    /// they were first seen, like the taskbar.
    fn stable_order(&mut self, items: Vec<DockItem>) -> Vec<DockItem> {
        fn key(i: &DockItem) -> String {
            if !i.exe.is_empty() {
                i.exe.clone()
            } else {
                format!("hwnd:{}", i.windows.first().map(|h| h.0 as isize).unwrap_or(0))
            }
        }
        let (pinned, mut running): (Vec<DockItem>, Vec<DockItem>) = items.into_iter().partition(|i| i.pinned);
        let keys: Vec<String> = running.iter().map(key).collect();
        self.running_order.retain(|k| keys.contains(k));
        for k in keys {
            if !self.running_order.contains(&k) {
                self.running_order.push(k);
            }
        }
        let order = &self.running_order;
        running.sort_by_key(|i| order.iter().position(|k| *k == key(i)).unwrap_or(usize::MAX));
        pinned.into_iter().chain(running).collect()
    }

    fn layout_width(&self) -> f32 {
        let s = self.scale();
        let n = self.items.len().max(1) as f32;
        let has_sep = self.items.iter().any(|i| i.pinned) && self.items.iter().any(|i| !i.pinned);
        let start = if self.start_button { SLOT * s } else { 0.0 };
        2.0 * PAD * s + start + n * SLOT * s + if has_sep { SEP * s } else { 0.0 }
    }

    /// Centre the dock at the bottom of the primary monitor.
    fn reposition(&mut self) {
        unsafe {
            let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(hmon, &mut mi);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            if dx != self.dpi && dx > 0 {
                self.dpi = dx;
                if let Ok(f) = self.gfx.text_format(TEXT_FONT, 13.0 * self.scale(), 600) {
                    self.letter_font = f;
                }
                self.bitmaps.clear(); // icon size changed
            }
            self.monitor = mi.rcMonitor;
        }
        let s = self.scale();
        self.width = self.layout_width().round() as i32;
        self.height = ((SLOT + 2.0 * PAD) * s).round() as i32;
        let mw = self.monitor.right - self.monitor.left;
        self.base_x = self.monitor.left + (mw - self.width) / 2;
        self.base_y = self.monitor.bottom - self.height - (MARGIN * s).round() as i32;
        // SearchHost draws Start's visible panel 14 px below its window's
        // bottom edge (measured on the real PC at 100%), so leave room for that.
        crate::start_menu::set_target(
            self.base_y - ((MARGIN + START_OVERHANG) * s).round() as i32,
            self.monitor.left,
            self.monitor.right,
        );
        self.gfx.resize(self.width as u32, self.height as u32);
        self.apply_offset();
        self.invalidate();
    }

    fn apply_offset(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                self.base_x,
                self.base_y + self.offset.round() as i32,
                self.width,
                self.height,
                SWP_NOACTIVATE,
            );
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    // ---- Show / hide ------------------------------------------------------

    fn poll(&mut self) {
        self.ticks += 1;
        if self.ticks % 10 == 0 {
            self.refresh_items();
        }
        if self.mode == DockMode::Smart && self.ticks % 3 == 0 {
            let started = std::time::Instant::now();
            let dock = self.shown_rect();
            self.covered = apps::any_window_overlaps(&dock);
            self.note_slow("checking for covering windows", started);
        }

        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        let dock = self.shown_rect();
        let visible = self.offset < self.hidden_offset() - 1.0;
        let in_x = cursor.x >= self.monitor.left && cursor.x < self.monitor.right;
        let at_edge = in_x
            && cursor.y >= self.monitor.bottom - 2
            && cursor.y < self.monitor.bottom + 1
            && cursor.x >= dock.left - 40
            && cursor.x <= dock.right + 40;
        let over = visible
            && cursor.x >= dock.left - 8
            && cursor.x <= dock.right + 8
            && cursor.y >= dock.top - 8
            && cursor.y <= self.monitor.bottom;

        let want_visible = if self.forced {
            true
        } else if self.fullscreen_in_front() {
            false
        } else {
            match self.mode {
                DockMode::Always => true,
                DockMode::AutoHide => at_edge || over,
                DockMode::Smart => !self.covered || at_edge || over,
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

        // Hover highlight
        let lx = (cursor.x - self.base_x) as f32;
        let ly = (cursor.y - (self.base_y + self.offset.round() as i32)) as f32;
        let hovered = if visible { self.slot_at(lx, ly) } else { None };
        let start_hovered = visible && self.start_slot.map(|r| r.contains(lx, ly)).unwrap_or(false);
        if hovered != self.hovered || start_hovered != self.start_hovered {
            self.hovered = hovered;
            self.start_hovered = start_hovered;
            self.invalidate();
        }
    }

    fn fullscreen_in_front(&self) -> bool {
        apps::fullscreen_on(&self.monitor)
    }

    fn set_target_hidden(&mut self, hidden: bool) {
        if hidden == self.target_hidden {
            return;
        }
        self.target_hidden = hidden;
        if !hidden {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            }
        }
        if !self.animating {
            self.animating = true;
            unsafe {
                SetTimer(self.hwnd, TIMER_ANIM, 15, None);
            }
        }
    }

    fn animate(&mut self) {
        let started = std::time::Instant::now();
        self.animate_step();
        self.note_slow("slide animation step", started);
    }

    fn animate_step(&mut self) {
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
        self.apply_offset();
    }

    // ---- Actions ------------------------------------------------------------

    fn click(&mut self, i: usize) {
        let item = self.items[i].clone();
        unsafe {
            if item.windows.is_empty() {
                if let Some(path) = &item.launch {
                    popup::open_uri(path);
                }
                return;
            }
            let fg = GetForegroundWindow();
            if item.windows.contains(&fg) {
                if item.windows.len() > 1 {
                    // Several windows: cycle to the one furthest back.
                    activate(*item.windows.last().unwrap());
                } else {
                    let _ = ShowWindow(fg, SW_MINIMIZE);
                }
            } else {
                activate(item.windows[0]);
            }
        }
        self.refresh_items();
    }

    fn context_menu(&mut self, index: Option<usize>) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let item = index.map(|i| self.items[i].clone());
            if let Some(item) = &item {
                if item.launch.is_some() {
                    let _ = AppendMenuW(menu, MF_STRING, MENU_NEW, w!("New window"));
                }
                if item.pinned {
                    let _ = AppendMenuW(menu, MF_STRING, MENU_PIN, w!("Unpin from dock"));
                } else if item.launch.is_some() {
                    let _ = AppendMenuW(menu, MF_STRING, MENU_PIN, w!("Pin to dock"));
                }
                if !item.windows.is_empty() {
                    let label = if item.windows.len() > 1 { "Close all windows" } else { "Close window" };
                    let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, &HSTRING::from(label));
                }
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
            }
            let check = |m: DockMode| if self.mode == m { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING | check(DockMode::Smart), MENU_MODE_SMART, w!("Hide dock when a window covers it"));
            let _ = AppendMenuW(menu, MF_STRING | check(DockMode::AutoHide), MENU_MODE_AUTOHIDE, w!("Auto-hide dock"));
            let _ = AppendMenuW(menu, MF_STRING | check(DockMode::Always), MENU_MODE_ALWAYS, w!("Always show dock"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
            let start_check = if self.start_button { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING | start_check, MENU_START_BUTTON, w!("Show Start button"));

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, self.hwnd, None);
            let _ = DestroyMenu(menu);

            match cmd.0 as usize {
                MENU_NEW => {
                    if let Some(path) = item.as_ref().and_then(|i| i.launch.clone()) {
                        popup::open_uri(&path);
                    }
                }
                MENU_PIN => {
                    if let Some(item) = &item {
                        if item.pinned {
                            if let Some(path) = &item.launch {
                                self.pins.retain(|p| &p.path != path);
                            }
                        } else if let Some(path) = &item.launch {
                            // Pin the exe that starts the app, not a helper
                            // that only draws its windows (Steam).
                            let launcher = apps::launcher_for(&item.windows);
                            let exe = launcher.as_deref().unwrap_or(path);
                            // Store apps by their AppUserModelID: their exe
                            // path changes with every update.
                            let app_id = apps::package_family(exe).and_then(|_| {
                                let mut pid = 0u32;
                                GetWindowThreadProcessId(*item.windows.first()?, Some(&mut pid));
                                apps::process_app_id(pid)
                            });
                            self.pins.push(match app_id {
                                Some(id) => apps::pin_for_app_id(&id),
                                None => apps::pin_for_exe(exe),
                            });
                        }
                        apps::save_pins(&self.pins);
                        self.refresh_items();
                        self.reposition();
                    }
                }
                MENU_CLOSE => {
                    if let Some(item) = &item {
                        for h in &item.windows {
                            let _ = PostMessageW(*h, WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                }
                MENU_START_BUTTON => {
                    self.start_button = !self.start_button;
                    let on = self.start_button;
                    Config::update(|c| c.dock_start = on);
                    if !on {
                        self.start_slot = None;
                    }
                    self.reposition();
                    self.invalidate();
                }
                id @ (MENU_MODE_SMART | MENU_MODE_AUTOHIDE | MENU_MODE_ALWAYS) => {
                    let mode = match id {
                        MENU_MODE_AUTOHIDE => DockMode::AutoHide,
                        MENU_MODE_ALWAYS => DockMode::Always,
                        _ => DockMode::Smart,
                    };
                    self.mode = mode;
                    self.forced = false;
                    Config::update(|c| c.dock_mode = mode);
                }
                _ => {}
            }
        }
    }

    // ---- Drawing ------------------------------------------------------------

    fn paint(&mut self) -> Result<()> {
        if self.width <= 0 || self.height <= 0 {
            return Ok(());
        }
        let s = self.scale();
        let frame = self.gfx.begin(self.hwnd, self.width as u32, self.height as u32)?;
        if self.gfx.generation != self.bitmap_generation {
            self.bitmaps.clear();
            self.bitmap_generation = self.gfx.generation;
        }
        frame.round_rect(0.0, 0.0, self.width as f32, self.height as f32, 0.0, Color(0.10, 0.10, 0.12, self.opacity));

        let fg = unsafe { GetForegroundWindow() };
        let icon_px = (ICON * s).round();
        let mut slots = Vec::with_capacity(self.items.len());
        let mut x = PAD * s;
        let y = PAD * s;
        self.start_slot = None;
        if self.start_button {
            let slot = R::new(x, y, SLOT * s, SLOT * s);
            if self.start_hovered {
                frame.round_rect(slot.x + 2.0 * s, slot.y, slot.w - 4.0 * s, slot.h, 10.0 * s, FAINT);
            }
            draw_start_icon(&frame, slot.x + (slot.w - icon_px) / 2.0, slot.y + 3.0 * s, icon_px);
            self.start_slot = Some(slot);
            x += SLOT * s;
        }
        let mut prev_pinned = true;
        for i in 0..self.items.len() {
            let item = self.items[i].clone();
            if !item.pinned && prev_pinned && i > 0 {
                // separator between pinned and other running apps
                // Snapped to whole device pixels so it's one crisp line of
                // even thickness at any display scale.
                let dw = (2.0 * s).round().max(1.0);
                let dx = (x + SEP * s / 2.0 - dw / 2.0).round();
                let dy = (y + 8.0 * s).round();
                let dh = ((SLOT - 16.0) * s).round();
                frame.round_rect(dx, dy, dw, dh, 0.0, DIVIDER);
                x += SEP * s;
            }
            prev_pinned = item.pinned;
            let slot = R::new(x, y, SLOT * s, SLOT * s);
            if self.hovered == Some(i) {
                frame.round_rect(slot.x + 2.0 * s, slot.y, slot.w - 4.0 * s, slot.h, 10.0 * s, FAINT);
            }
            // Whole pixels, so the icon (already sized to icon_px) is drawn 1:1.
            let ix = (slot.x + (slot.w - icon_px) / 2.0).round();
            let iy = (slot.y + 3.0 * s).round();
            let dim = if item.windows.is_empty() { 0.9 } else { 1.0 };
            match self.icon_for(&item, icon_px as i32, &frame) {
                IconState::Ready(bmp) => frame.draw_icon(&bmp, ix, iy, icon_px, dim),
                // Still loading (usually a few milliseconds): leave the slot
                // empty rather than flash a letter.
                IconState::Loading => {}
                IconState::Missing => {
                    // No icon available: a tile with the app's first letter.
                    frame.round_rect(ix, iy, icon_px, icon_px, 9.0 * s, Color(1.0, 1.0, 1.0, 0.14));
                    let letter: String = item.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
                    if let Ok(t) = self.gfx.text(&letter, &self.letter_font, icon_px) {
                        frame.text(&t, ix + (icon_px - t.width) / 2.0, iy, FG);
                    }
                }
            }
            if !item.windows.is_empty() {
                let focused = item.windows.contains(&fg);
                let color = if focused { self.accent } else { DIM };
                let w = if focused { 10.0 * s } else { 4.0 * s };
                frame.round_rect(slot.x + (slot.w - w) / 2.0, slot.y + slot.h - 5.0 * s, w, 3.0 * s, 1.5 * s, color);
            }
            slots.push(slot);
            x += SLOT * s;
        }
        let result = self.gfx.end(frame);
        self.slots = slots;
        result
    }

    fn icon_for(&mut self, item: &DockItem, size: i32, frame: &crate::render::Frame) -> IconState {
        use crate::icon_loader::{Loader, Request, Source};
        let (base, source) = match (&item.launch, item.windows.first()) {
            (Some(path), _) => (path.to_lowercase(), Source::Path(path.clone())),
            (None, Some(h)) => (format!("hwnd:{}", h.0 as isize), Source::Window(h.0 as isize)),
            (None, None) => return IconState::Missing,
        };
        let key = format!("{size}|{base}");
        if let Some(Some(bmp)) = self.bitmaps.get(&key) {
            return IconState::Ready(bmp.clone());
        }
        if let Some(pixels) = self.icon_pixels.get(&key) {
            let bmp = self.icons.as_ref().and_then(|i| i.from_pixels(pixels)).and_then(|src| frame.bitmap(&src).ok());
            if let Some(bmp) = bmp {
                self.bitmaps.insert(key, Some(bmp.clone()));
                return IconState::Ready(bmp);
            }
        }
        if self.icons_pending.contains(&key) {
            return IconState::Loading;
        }
        if let Some(missed) = self.icon_misses.get(&key) {
            if missed.elapsed() < std::time::Duration::from_secs(3) {
                return IconState::Missing;
            }
        }
        if self.loader.is_none() {
            self.loader = Some(Loader::start(self.hwnd, WM_ICON_READY));
        }
        // Packaged desktop apps (Photos) often have no icon in their exe, so the
        // shell gives a generic one; their Apps-folder entry has the real one.
        let packaged = match (&source, item.windows.first()) {
            (Source::Path(p), Some(h)) if p.to_lowercase().contains("\\windowsapps\\") => {
                let mut pid = 0u32;
                unsafe {
                    GetWindowThreadProcessId(*h, Some(&mut pid));
                }
                apps::process_app_id(pid)
            }
            _ => None,
        };
        let source = packaged.map(|id| Source::Path(format!("shell:AppsFolder\\{id}"))).unwrap_or(source);
        if let Some(loader) = &self.loader {
            self.icons_pending.insert(key.clone());
            loader.request(Request { key, source, size });
        }
        IconState::Loading
    }

    /// Icons the loader finished: keep their pixels (or note the miss, to
    /// retry in 3 s) and redraw.
    fn icons_ready(&mut self) {
        let done = match &self.loader {
            Some(loader) => loader.take_done(),
            None => return,
        };
        if done.is_empty() {
            return;
        }
        for loaded in done {
            self.icons_pending.remove(&loaded.key);
            match loaded.pixels {
                Some(pixels) => {
                    self.icon_misses.remove(&loaded.key);
                    self.icon_pixels.insert(loaded.key, pixels);
                }
                None => {
                    self.icon_misses.insert(loaded.key, std::time::Instant::now());
                }
            }
        }
        // Forget icons of windows that have closed.
        if self.icon_pixels.len() > 64 {
            let live: std::collections::HashSet<String> = self
                .items
                .iter()
                .flat_map(|i| i.windows.iter().map(|h| format!("hwnd:{}", h.0 as isize)).chain(i.launch.iter().map(|p| p.to_lowercase())))
                .collect();
            self.icon_pixels.retain(|k, _| k.split_once('|').map(|(_, b)| live.contains(b)).unwrap_or(false));
            self.bitmaps.retain(|k, _| k.split_once('|').map(|(_, b)| live.contains(b)).unwrap_or(false));
        }
        self.invalidate();
    }
}

/// The Start button's icon: the Windows 11 logo, four square panes in a
/// 2 x 2 grid, in the blue the Windows taskbar uses.
fn draw_start_icon(frame: &crate::render::Frame, x: f32, y: f32, size: f32) {
    // App icon artwork fills ~90% of its square (measured on the real PC); a
    // solid logo reads larger, so it stays a touch under that.
    let grid = (size * 0.88).round();
    let gap = (grid / 14.0).round().max(1.0);
    let pane = ((grid - gap) / 2.0).floor().max(2.0);
    let total = 2.0 * pane + gap;
    let left = (x + (size - total) / 2.0).round();
    let top = (y + (size - total) / 2.0).round();
    for row in 0..2 {
        for col in 0..2 {
            frame.round_rect(
                left + col as f32 * (pane + gap),
                top + row as f32 * (pane + gap),
                pane,
                pane,
                pane * 0.08,
                START_BLUE,
            );
        }
    }
}

/// Open the Start menu (or with `power_menu`, the Win+X menu) by pressing
/// the Windows key for the user: the same path as the keyboard, which
/// works while betterUI keeps the Windows taskbar hidden.
fn open_start(power_menu: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LWIN, VK_X,
    };
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() },
        },
    };
    let inputs: Vec<INPUT> = if power_menu {
        vec![key(VK_LWIN, false), key(VK_X, false), key(VK_X, true), key(VK_LWIN, true)]
    } else {
        vec![key(VK_LWIN, false), key(VK_LWIN, true)]
    };
    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent as usize != inputs.len() {
            log::write("could not open Start from the dock (SendInput was blocked)");
        }
    }
}

enum IconState {
    Ready(ID2D1Bitmap),
    Loading,
    Missing,
}

unsafe fn activate(hwnd: HWND) {
    if IsIconic(hwnd).as_bool() {
        let _ = ShowWindow(hwnd, SW_RESTORE);
    }
    let _ = SetForegroundWindow(hwnd);
}
