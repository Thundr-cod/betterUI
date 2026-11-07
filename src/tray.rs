// System tray host.
//
// Apps add their tray icons by sending a WM_COPYDATA message to the first
// window of class "Shell_TrayWnd" (normally Explorer's taskbar). Windows 11
// gives other programs no supported way to read those icons back, so, like
// RetroBar and other taskbar replacements, betterUI registers its own
// (invisible) "Shell_TrayWnd" window above Explorer's so that it receives
// those messages first.
//
// Every message is passed straight on to Explorer's real taskbar as well, so
// Explorer stays in sync: if betterUI quits or crashes, the icons are still
// there and nothing is lost. Appbar messages (which also go to
// "Shell_TrayWnd") are passed through untouched.
//
// At start-up the standard "TaskbarCreated" message is broadcast, which
// makes every running app re-send its icons (the same thing apps do when
// Explorer restarts).

use std::cell::RefCell;
use std::time::Instant;

use windows::core::{w, Result, GUID};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CopyIcon, CreateWindowExW, DefWindowProcW, DestroyIcon, FindWindowExW, FindWindowW,
    GetWindowThreadProcessId,
    IsWindow, KillTimer, PostMessageW, RegisterClassW, RegisterWindowMessageW, SendMessageTimeoutW,
    SetTimer, SetWindowPos, HICON, HWND_BROADCAST, HWND_TOPMOST, SMTO_ABORTIFHUNG, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, WM_COPYDATA, WM_TIMER, WM_USER, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::log;

// Shell_NotifyIcon messages and flags (shellapi.h)
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_STATE: u32 = 0x08;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x01;
const COPYDATA_TRAY: usize = 1;

const TIMER_TOPMOST: usize = 1; // keep our window ahead of Explorer's (every 1 s)
const TIMER_REBROADCAST: usize = 2; // ask apps to re-send their icons again
const TIMER_EXPLORER_LIST: usize = 3; // compare with the icons Explorer holds
const TIMER_EXPLORER_FALLBACK: usize = 4; // show Explorer's copy of icons that never arrived
const WM_EXPLORER_ICONS: u32 = WM_USER + 0x7F0; // the list is ready (from a worker thread)

/// The 32-bit-compatible NOTIFYICONDATAW layout used on the wire, so 32-bit
/// and 64-bit apps send the same thing. Handles are 32-bit values.
#[repr(C)]
#[derive(Clone, Copy)]
struct NotifyIconData32 {
    cb_size: u32,
    hwnd: u32,
    uid: u32,
    flags: u32,
    callback_message: u32,
    hicon: u32,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    timeout_or_version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: GUID,
    balloon_icon: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TrayMessage {
    magic: u32,
    message: u32,
    nid: NotifyIconData32,
}

/// One tray icon as the dropdown shows it.
#[derive(Clone)]
pub struct TrayIcon {
    pub owner: HWND,
    pub uid: u32,
    pub guid: Option<GUID>,
    pub callback_message: u32,
    pub version: u32,
    /// Our own copy of the icon (destroyed when replaced or removed).
    pub icon: Option<HICON>,
    pub tip: String,
    pub hidden: bool,
    /// For an icon only Explorer had (the app never sent it to betterUI):
    /// the app's exe, opened on click since there's no way to pass the
    /// click on to the app.
    pub launch: Option<String>,
}

impl TrayIcon {
    fn matches(&self, nid: &NotifyIconData32) -> bool {
        if nid.flags & NIF_GUID != 0 {
            self.guid == Some(nid.guid)
        } else {
            self.owner.0 as usize == nid.hwnd as usize && self.uid == nid.uid
        }
    }
}

struct Host {
    icons: Vec<TrayIcon>,
    taskbar_created: u32,
    last_broadcast: Option<Instant>,
    /// Icons Explorer has that betterUI was missing, asked for directly.
    explorer_pending: Vec<crate::tray_explorer::ExplorerIcon>,
    /// Told when the icon list changes (the dropdown).
    on_change: Option<fn()>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn handle(v: u32) -> *mut core::ffi::c_void {
    v as usize as *mut core::ffi::c_void
}

/// Explorer's real taskbar window (not ours).
fn explorer_tray(ours: HWND) -> Option<HWND> {
    let mut after: Option<HWND> = None;
    unsafe {
        loop {
            let found = FindWindowExW(None, after.unwrap_or_default(), w!("Shell_TrayWnd"), None).ok()?;
            if found.is_invalid() {
                return None;
            }
            if found != ours {
                return Some(found);
            }
            after = Some(found);
        }
    }
}

pub fn start(on_change: fn()) {
    if let Err(e) = start_inner(on_change) {
        log::write(&format!("tray host could not start: {e:?}"));
    }
}

fn start_inner(on_change: fn()) -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?.into();
        let class = w!("Shell_TrayWnd");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_win32());
        }
        // Never shown; it only receives messages.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            hinstance,
            None,
        )?;
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        HOST.with(|h| {
            *h.borrow_mut() = Some(Host { icons: Vec::new(), taskbar_created, last_broadcast: None, explorer_pending: Vec::new(), on_change: Some(on_change) })
        });
        bring_to_front(hwnd);
        SetTimer(hwnd, TIMER_TOPMOST, 1000, None);
        debug(&format!("tray host started; front window is ours: {}", is_front(hwnd)));
        broadcast_taskbar_created(taskbar_created);
        // Some apps answer slowly or miss the first request: ask once more.
        SetTimer(hwnd, TIMER_REBROADCAST, 4000, None);
        // Then compare with Explorer's own list, once apps have answered.
        SetTimer(hwnd, TIMER_EXPLORER_LIST, 7000, None);
    }
    Ok(())
}

fn bring_to_front(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
    }
}

/// Is ours the "Shell_TrayWnd" that apps will find (the one in front)?
fn is_front(ours: HWND) -> bool {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), None).map(|h| h == ours).unwrap_or(false) }
}

// ---- Diagnostics ------------------------------------------------------------
// tray_debug.txt: every tray message received (most recent 400).
// tray_icons.txt: the current icon list.
// Both in %APPDATA%\betterUI, to diagnose apps whose icons don't show up.

thread_local! {
    static DEBUG_LINES: RefCell<std::collections::VecDeque<String>> = const { RefCell::new(std::collections::VecDeque::new()) };
    static STARTED: Instant = Instant::now();
}

fn debug(line: &str) {
    let secs = STARTED.with(|s| s.elapsed().as_secs_f32());
    let text = DEBUG_LINES.with(|d| {
        let mut d = d.borrow_mut();
        d.push_back(format!("[{secs:7.2}s] {line}"));
        while d.len() > 400 {
            d.pop_front();
        }
        d.iter().cloned().collect::<Vec<_>>().join("\n")
    });
    let _ = std::fs::write(crate::log::data_dir().join("tray_debug.txt"), text + "\n");
}

fn exe_of(hwnd: HWND) -> String {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    crate::procinfo::process_path(pid)
        .and_then(|p| std::path::Path::new(&p).file_name().map(|f| f.to_string_lossy().to_string()))
        .unwrap_or_else(|| format!("pid {pid}"))
}

fn dump_icons(icons: &[TrayIcon]) {
    let mut text = String::from("# Tray icons betterUI knows about (shown = appears in the dropdown)\n");
    for i in icons {
        text.push_str(&format!(
            "{} | uid {} | {} | shown={} hidden={} icon={} version={} | {}\n",
            exe_of(i.owner),
            i.uid,
            if i.guid.is_some() { "guid" } else { "id" },
            !i.hidden && i.icon.is_some(),
            i.hidden,
            i.icon.is_some(),
            i.version,
            i.tip.lines().next().unwrap_or("")
        ));
    }
    let _ = std::fs::write(crate::log::data_dir().join("tray_icons.txt"), text);
}

fn broadcast_taskbar_created(msg: u32) {
    HOST.with(|h| {
        if let Ok(mut g) = h.try_borrow_mut() {
            if let Some(host) = g.as_mut() {
                host.last_broadcast = Some(Instant::now());
            }
        }
    });
    unsafe {
        let _ = PostMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0));
    }
}

/// Current tray icons (visible ones only), oldest first.
pub fn icons() -> Vec<TrayIcon> {
    if crate::audio::demo() {
        return demo_icons();
    }
    HOST.with(|h| {
        let Ok(mut g) = h.try_borrow_mut() else { return Vec::new() };
        let Some(host) = g.as_mut() else { return Vec::new() };
        // Drop icons whose app has gone away without removing them.
        host.icons.retain(|i| {
            let alive = unsafe { IsWindow(i.owner).as_bool() };
            if !alive {
                if let Some(icon) = i.icon {
                    unsafe {
                        let _ = DestroyIcon(icon);
                    }
                }
            }
            alive
        });
        host.icons.iter().filter(|i| !i.hidden && i.icon.is_some()).cloned().collect()
    })
}

fn demo_icons() -> Vec<TrayIcon> {
    use windows::Win32::UI::WindowsAndMessaging::{LoadIconW, IDI_APPLICATION, IDI_INFORMATION, IDI_SHIELD, IDI_WARNING};
    [(IDI_SHIELD, "Windows Security"), (IDI_INFORMATION, "OneDrive - Up to date"), (IDI_APPLICATION, "Steam"), (IDI_WARNING, "Discord"), (IDI_APPLICATION, "NVIDIA Settings")]
        .iter()
        .enumerate()
        .map(|(i, (id, tip))| TrayIcon {
            owner: HWND::default(),
            uid: i as u32,
            guid: None,
            callback_message: 0,
            version: 4,
            icon: unsafe { LoadIconW(None, *id).ok() },
            tip: tip.to_string(),
            hidden: false,
            launch: None,
        })
        .collect()
}

/// Simulate a click on a tray icon, the way Explorer does: send the app the
/// mouse messages it asked for, with its icon's position.
pub fn click(icon: &TrayIcon, right: bool, x: i32, y: i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, WM_CONTEXTMENU, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
    };
    const NIN_SELECT: u32 = WM_USER;
    if icon.callback_message == 0 || icon.owner.is_invalid() {
        // An icon only Explorer had: open its app instead.
        if let (Some(exe), false) = (&icon.launch, right) {
            unsafe {
                let _ = windows::Win32::UI::Shell::ShellExecuteW(
                    None,
                    w!("open"),
                    &windows::core::HSTRING::from(exe.as_str()),
                    None,
                    None,
                    windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                );
            }
        }
        return;
    }
    unsafe {
        // Let the app bring its menu/window to the front.
        let mut pid = 0u32;
        GetWindowThreadProcessId(icon.owner, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let messages: &[u32] = if right {
            &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU]
        } else {
            &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT]
        };
        for &m in messages {
            let (wparam, lparam) = if icon.version >= 4 {
                // NOTIFYICON_VERSION_4: click position in wParam,
                // event + icon id in lParam.
                (
                    ((x as u16 as u32) | ((y as u16 as u32) << 16)) as usize,
                    ((m & 0xFFFF) | ((icon.uid & 0xFFFF) << 16)) as isize,
                )
            } else {
                // Older icons: icon id in wParam, event in lParam. Only
                // version 3 understands NIN_SELECT / WM_CONTEXTMENU.
                if icon.version < 3 && (m == WM_CONTEXTMENU || m == NIN_SELECT) {
                    continue;
                }
                (icon.uid as usize, m as isize)
            };
            let _ = PostMessageW(icon.owner, icon.callback_message, WPARAM(wparam), LPARAM(lparam));
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let cds = lp.0 as *const COPYDATASTRUCT;
            if !cds.is_null() && (*cds).dwData == COPYDATA_TRAY {
                record(&*cds);
            }
            // Always let Explorer handle it too (icons and appbar messages).
            return forward(hwnd, msg, wp, lp).unwrap_or(LRESULT(1));
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_TOPMOST => {
                    if !is_front(hwnd) {
                        debug("Explorer's taskbar was in front of ours; taking the front spot back");
                        bring_to_front(hwnd);
                    }
                }
                TIMER_EXPLORER_LIST => {
                    let _ = KillTimer(hwnd, TIMER_EXPLORER_LIST);
                    request_explorer_icons(hwnd);
                }
                TIMER_EXPLORER_FALLBACK => {
                    let _ = KillTimer(hwnd, TIMER_EXPLORER_FALLBACK);
                    add_explorer_fallbacks();
                }
                TIMER_REBROADCAST => {
                    let _ = KillTimer(hwnd, TIMER_REBROADCAST);
                    bring_to_front(hwnd);
                    debug(&format!("asking apps to re-send icons; front window is ours: {}", is_front(hwnd)));
                    let msg = HOST.with(|h| h.try_borrow().ok().and_then(|g| g.as_ref().map(|x| x.taskbar_created)));
                    if let Some(m) = msg {
                        broadcast_taskbar_created(m);
                    }
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_EXPLORER_ICONS => {
            on_explorer_icons(hwnd);
            return LRESULT(0);
        }
        _ => {}
    }
    let taskbar_created = HOST.with(|h| h.try_borrow().ok().and_then(|g| g.as_ref().map(|x| (x.taskbar_created, x.last_broadcast))));
    if let Some((tc, last)) = taskbar_created {
        if msg == tc {
            // Explorer restarted (not our own broadcast): its new taskbar is
            // now in front of ours. Take the front spot back and ask apps to
            // re-send their icons, after Explorer has settled.
            let ours = last.map(|t| t.elapsed().as_secs_f32() < 3.0).unwrap_or(false);
            if !ours {
                debug("TaskbarCreated from Explorer (Explorer restarted?)");
                SetTimer(hwnd, TIMER_REBROADCAST, 2000, None);
                SetTimer(hwnd, TIMER_EXPLORER_LIST, 5000, None);
            }
            return LRESULT(0);
        }
    }
    // Other messages apps send to the taskbar (e.g. WM_COMMAND shortcuts)
    // go to Explorer.
    if msg == windows::Win32::UI::WindowsAndMessaging::WM_COMMAND || (WM_USER..0x8000).contains(&msg) {
        if let Some(r) = forward(hwnd, msg, wp, lp) {
            return r;
        }
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn forward(ours: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let target = explorer_tray(ours)?;
    let mut result = 0usize;
    unsafe {
        let ok = SendMessageTimeoutW(target, msg, wp, lp, SMTO_ABORTIFHUNG, 1000, Some(&mut result));
        if ok.0 == 0 {
            return None;
        }
    }
    Some(LRESULT(result as isize))
}

fn record(cds: &COPYDATASTRUCT) {
    let size = std::mem::size_of::<TrayMessage>();
    let n = (cds.cbData as usize).min(size);
    if cds.lpData.is_null() || n < 8 + 24 {
        return;
    }
    // Copy into a zeroed, aligned struct (older apps send a shorter one).
    let mut data: TrayMessage = unsafe { std::mem::zeroed() };
    unsafe {
        std::ptr::copy_nonoverlapping(cds.lpData as *const u8, &mut data as *mut TrayMessage as *mut u8, n);
    }
    let message = data.message;
    let nid = data.nid;
    let kind = match message {
        NIM_ADD => "ADD",
        NIM_MODIFY => "MODIFY",
        NIM_DELETE => "DELETE",
        3 => "SETFOCUS",
        NIM_SETVERSION => "SETVERSION",
        _ => "?",
    };
    let owner = HWND(handle(nid.hwnd));

    let changed = HOST.with(|h| {
        let Ok(mut g) = h.try_borrow_mut() else { return false };
        let Some(host) = g.as_mut() else { return false };
        let pos = host.icons.iter().position(|i| i.matches(&nid));
        match message {
            NIM_ADD | NIM_MODIFY => {
                // A modify for an icon we never saw added is treated as an add.
                let idx = match pos {
                    Some(i) => i,
                    None => {
                        host.icons.push(TrayIcon {
                            owner: HWND(handle(nid.hwnd)),
                            uid: nid.uid,
                            guid: if nid.flags & NIF_GUID != 0 { Some(nid.guid) } else { None },
                            callback_message: 0,
                            version: 0,
                            icon: None,
                            tip: String::new(),
                            hidden: false,
                            launch: None,
                        });
                        host.icons.len() - 1
                    }
                };
                let icon = &mut host.icons[idx];
                if nid.hwnd != 0 {
                    icon.owner = HWND(handle(nid.hwnd));
                }
                if nid.flags & NIF_MESSAGE != 0 {
                    icon.callback_message = nid.callback_message;
                }
                if nid.flags & NIF_ICON != 0 {
                    if let Some(old) = icon.icon.take() {
                        unsafe {
                            let _ = DestroyIcon(old);
                        }
                    }
                    if nid.hicon != 0 {
                        icon.icon = unsafe { CopyIcon(HICON(handle(nid.hicon))).ok() };
                        if icon.icon.is_none() {
                            debug(&format!("  could not copy icon handle {:#x}", nid.hicon));
                        }
                    }
                }
                if nid.flags & NIF_TIP != 0 {
                    icon.tip = wide_to_string(&nid.tip);
                }
                if nid.flags & NIF_STATE != 0 && nid.state_mask & NIS_HIDDEN != 0 {
                    icon.hidden = nid.state & NIS_HIDDEN != 0;
                }
                true
            }
            NIM_DELETE => {
                if let Some(i) = pos {
                    let removed = host.icons.remove(i);
                    if let Some(icon) = removed.icon {
                        unsafe {
                            let _ = DestroyIcon(icon);
                        }
                    }
                    true
                } else {
                    false
                }
            }
            NIM_SETVERSION => {
                if let Some(i) = pos {
                    host.icons[i].version = nid.timeout_or_version;
                }
                false
            }
            _ => false,
        }
    });
    debug(&format!(
        "{kind:10} {:24} uid={} flags={:#04x} icon={} state={:#x}/{:#x} cb={} size={}",
        exe_of(owner),
        nid.uid,
        nid.flags,
        nid.hicon != 0,
        nid.state,
        nid.state_mask,
        cds.cbData,
        size
    ));
    if changed {
        HOST.with(|h| {
            if let Ok(g) = h.try_borrow() {
                if let Some(host) = g.as_ref() {
                    dump_icons(&host.icons);
                }
            }
        });
        let cb = HOST.with(|h| h.try_borrow().ok().and_then(|g| g.as_ref().and_then(|x| x.on_change)));
        if let Some(cb) = cb {
            cb();
        }
    }
}

// ---- Icons only Explorer has (tray_explorer.rs) ----

struct SendIcons(Vec<crate::tray_explorer::ExplorerIcon>);
// The icon handles are plain USER handles, usable from any thread.
unsafe impl Send for SendIcons {}

// Filled by the worker thread; read (and logged) on the tray window's thread,
// since the debug log lives there.
static EXPLORER_ICONS: std::sync::Mutex<Option<std::result::Result<SendIcons, String>>> = std::sync::Mutex::new(None);

/// Read Explorer's list on a worker thread (it waits on Explorer), then
/// post the result back to the tray window.
fn request_explorer_icons(hwnd: HWND) {
    let target = hwnd.0 as isize;
    std::thread::spawn(move || {
        let result = crate::tray_explorer::list().map(SendIcons);
        if let Ok(mut slot) = EXPLORER_ICONS.lock() {
            *slot = Some(result);
        }
        unsafe {
            let _ = PostMessageW(HWND(target as *mut _), WM_EXPLORER_ICONS, WPARAM(0), LPARAM(0));
        }
    });
}

fn known(host: &Host, e: &crate::tray_explorer::ExplorerIcon) -> bool {
    host.icons.iter().any(|i| match (e.guid, i.guid) {
        (Some(a), Some(b)) => a == b,
        _ => i.owner.0 as isize == e.hwnd && i.uid == e.uid,
    })
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
}

/// Explorer's list arrived: for each icon betterUI is missing, send the
/// app's icon window "TaskbarCreated" directly (a broadcast doesn't reach
/// message-only windows), and give it two seconds to send its icon.
fn on_explorer_icons(hwnd: HWND) {
    let items = match EXPLORER_ICONS.lock().ok().and_then(|mut s| s.take()) {
        Some(Ok(SendIcons(items))) => items,
        Some(Err(e)) => {
            debug(&format!("could not read Explorer's tray icons: {e}"));
            return;
        }
        None => return,
    };
    debug(&format!("Explorer's tray holds {} icons", items.len()));
    let mut missing = Vec::new();
    let tc = HOST.with(|h| {
        let Ok(mut g) = h.try_borrow_mut() else { return None };
        let host = g.as_mut()?;
        for e in items {
            let have = known(host, &e);
            debug(&format!(
                "Explorer has {:24} uid={} guid={} known={have} tip={:?}",
                file_name(&e.exe),
                e.uid,
                e.guid.is_some(),
                e.tip
            ));
            if have {
                if let Some(icon) = e.icon {
                    unsafe {
                        let _ = DestroyIcon(icon);
                    }
                }
            } else {
                missing.push(e.clone());
                host.explorer_pending.push(e);
            }
        }
        Some(host.taskbar_created)
    });
    let Some(tc) = tc else { return };
    if missing.is_empty() {
        return;
    }
    for e in &missing {
        let sent = unsafe { PostMessageW(HWND(e.hwnd as *mut _), tc, WPARAM(0), LPARAM(0)) };
        debug(&format!(
            "asked {} directly to re-send its icon: {}",
            file_name(&e.exe),
            match sent {
                Ok(()) => "sent".to_string(),
                Err(err) => format!("blocked ({err})"),
            }
        ));
    }
    unsafe {
        SetTimer(hwnd, TIMER_EXPLORER_FALLBACK, 2000, None);
    }
}

/// Icons that still haven't arrived: show Explorer's copy (its picture and
/// tooltip); clicking opens the app.
fn add_explorer_fallbacks() {
    let changed = HOST.with(|h| {
        let Ok(mut g) = h.try_borrow_mut() else { return false };
        let Some(host) = g.as_mut() else { return false };
        let pending = std::mem::take(&mut host.explorer_pending);
        let mut changed = false;
        for e in pending {
            if known(host, &e) {
                debug(&format!("{} sent its icon after being asked directly", file_name(&e.exe)));
                if let Some(icon) = e.icon {
                    unsafe {
                        let _ = DestroyIcon(icon);
                    }
                }
                continue;
            }
            debug(&format!("{} never sent its icon; showing Explorer's copy", file_name(&e.exe)));
            host.icons.push(TrayIcon {
                owner: HWND(e.hwnd as *mut _),
                uid: e.uid,
                guid: e.guid,
                callback_message: 0,
                version: 0,
                icon: e.icon,
                tip: e.tip.clone(),
                hidden: false,
                launch: Some(e.exe.clone()).filter(|x| !x.is_empty()),
            });
            changed = true;
        }
        if changed {
            dump_icons(&host.icons);
        }
        changed
    });
    if changed {
        let cb = HOST.with(|h| h.try_borrow().ok().and_then(|g| g.as_ref().and_then(|x| x.on_change)));
        if let Some(cb) = cb {
            cb();
        }
    }
}
