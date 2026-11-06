// What the dock shows: pinned apps plus apps with open windows, grouped by
// exe like the taskbar does.

use std::fs;
use std::path::{Path, PathBuf};

use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindow, GetWindowLongW, GetWindowTextLengthW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_APPWINDOW,
    WS_EX_TOOLWINDOW,
};

use crate::procinfo;

/// One icon on the dock.
#[derive(Clone)]
pub struct DockItem {
    pub name: String,
    /// What to run to launch it (a shortcut or an exe). None for apps that
    /// are only running, not pinned, and whose exe can't be launched directly.
    pub launch: Option<String>,
    /// Lower-case exe path, used to match windows to pins. Empty if unknown
    /// (e.g. a shortcut to a Store app).
    pub exe: String,
    pub windows: Vec<HWND>,
    pub pinned: bool,
}

/// A top-level window that would appear on the taskbar.
pub struct AppWindow {
    pub hwnd: HWND,
    pub exe: String,
}

const STORE_APP_HOST: &str = "applicationframehost.exe";

pub fn is_store_host(exe: &str) -> bool {
    exe.ends_with(STORE_APP_HOST)
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

/// The same rule the taskbar uses for which windows get a button.
fn is_app_window(hwnd: HWND, own_pid: u32) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || is_cloaked(hwnd) {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == own_pid {
            return false;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let owned = GetWindow(hwnd, GW_OWNER).map(|h| !h.is_invalid()).unwrap_or(false);
        if owned && ex & WS_EX_APPWINDOW.0 == 0 {
            return false;
        }
        if GetWindowTextLengthW(hwnd) == 0 {
            return false;
        }
        !matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
    }
}

/// Taskbar-style windows, front-most first.
pub fn app_windows() -> Vec<AppWindow> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<HWND>);
        list.push(hwnd);
        TRUE
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut Vec<HWND> as isize));
    }
    let own_pid = unsafe { GetCurrentProcessId() };
    all.into_iter()
        .filter(|h| is_app_window(*h, own_pid))
        .map(|hwnd| {
            let mut pid = 0u32;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            let exe = procinfo::process_path(pid).unwrap_or_default().to_lowercase();
            AppWindow { hwnd, exe }
        })
        .collect()
}

/// On-screen rectangle of a window (without the invisible resize border).
pub fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return None;
        }
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
    }
    Some(r)
}

// ---- Pins -------------------------------------------------------------------

#[derive(Clone)]
pub struct Pin {
    pub path: String,
    pub name: String,
    pub exe: String,
}

fn pins_file() -> PathBuf {
    crate::log::data_dir().join("dock_pins.txt")
}

/// Exe a shortcut points to, lower-case ("" for Store apps and other
/// shortcuts without a file target).
fn shortcut_target(lnk: &str) -> String {
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else {
            return String::new();
        };
        let Ok(file) = link.cast::<IPersistFile>() else { return String::new() };
        if file.Load(&HSTRING::from(lnk), STGM_READ).is_err() {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut fd = WIN32_FIND_DATAW::default();
        if link.GetPath(&mut buf, &mut fd, 0).is_err() {
            return String::new();
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len]).to_lowercase()
    }
}

fn pin_from_path(path: &str) -> Pin {
    let lower = path.to_lowercase();
    let (name, exe) = if lower.ends_with(".lnk") {
        let name = Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        (name, shortcut_target(path))
    } else {
        (procinfo::app_name(path), lower)
    };
    Pin { path: path.to_string(), name, exe }
}

/// The user's taskbar pins, used to fill the dock the first time.
fn taskbar_pins() -> Vec<String> {
    let Some(appdata) = std::env::var_os("APPDATA") else { return Vec::new() };
    let dir = PathBuf::from(appdata).join("Microsoft\\Internet Explorer\\Quick Launch\\User Pinned\\TaskBar");
    let mut out: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("lnk")).unwrap_or(false))
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|p| p.to_lowercase());
    out
}

pub fn load_pins() -> Vec<Pin> {
    let file = pins_file();
    let paths: Vec<String> = match fs::read_to_string(&file) {
        Ok(text) => text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect(),
        Err(_) => {
            let imported = taskbar_pins();
            save_pin_paths(&imported);
            imported
        }
    };
    paths.iter().map(|p| pin_from_path(p)).collect()
}

fn save_pin_paths(paths: &[String]) {
    let mut text = String::from("# Apps pinned to the betterUI dock, one shortcut or exe path per line.\n");
    for p in paths {
        text.push_str(p);
        text.push('\n');
    }
    if let Err(e) = fs::write(pins_file(), text) {
        crate::log::write(&format!("could not save dock pins: {e}"));
    }
}

pub fn save_pins(pins: &[Pin]) {
    let paths: Vec<String> = pins.iter().map(|p| p.path.clone()).collect();
    save_pin_paths(&paths);
}

pub fn pin_for_exe(exe: &str) -> Pin {
    pin_from_path(exe)
}

/// Combine pins and open windows into dock items: pins first (in pin
/// order), then running apps that aren't pinned (in first-seen order).
pub fn dock_items(pins: &[Pin], windows: &[AppWindow]) -> Vec<DockItem> {
    let mut items: Vec<DockItem> = pins
        .iter()
        .map(|p| DockItem {
            name: p.name.clone(),
            launch: Some(p.path.clone()),
            exe: p.exe.clone(),
            windows: Vec::new(),
            pinned: true,
        })
        .collect();
    for w in windows {
        // Store apps all share one host exe, so group them per window.
        let key_exe = if is_store_host(&w.exe) { String::new() } else { w.exe.clone() };
        if !key_exe.is_empty() {
            if let Some(item) = items.iter_mut().find(|i| i.exe == key_exe) {
                item.windows.push(w.hwnd);
                continue;
            }
        }
        let name = if key_exe.is_empty() {
            window_title(w.hwnd)
        } else {
            procinfo::app_name(&w.exe)
        };
        items.push(DockItem {
            name,
            launch: if key_exe.is_empty() { None } else { Some(w.exe.clone()) },
            exe: key_exe,
            windows: vec![w.hwnd],
            pinned: false,
        });
    }
    items
}

fn window_title(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Is a fullscreen app (game, video) in front on the monitor `m`?
pub fn fullscreen_on(m: &RECT) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return false;
        }
        if matches!(class_name(fg).as_str(), "Progman" | "WorkerW") {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

/// Does any app window overlap `area`?
pub fn any_window_overlaps(area: &RECT) -> bool {
    app_windows().iter().filter_map(|w| window_rect(w.hwnd)).any(|r| {
        r.left < area.right && area.left < r.right && r.top < area.bottom && area.top < r.bottom
    })
}

/// Is one of betterUI's own windows (a flyout or menu) in front?
pub fn own_window_in_front() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe {
        let fg = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}
