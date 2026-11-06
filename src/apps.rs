// What the dock shows: pinned apps plus apps with open windows, grouped by
// exe like the taskbar does.

use std::fs;
use std::path::{Path, PathBuf};

use windows::core::{Interface, GUID, HSTRING};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
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
    /// For Squirrel-installed apps (Discord, Slack …): the exe name the
    /// pinned `Update.exe --processStart <name>` starts, lower-case.
    pub squirrel_app: Option<String>,
}

fn pins_file() -> PathBuf {
    crate::log::data_dir().join("dock_pins.txt")
}

/// Exe a shortcut points to, lower-case ("" for Store apps and other
/// shortcuts without a file target). The taskbar's File Explorer shortcut
/// has no file target, so it is recognised by its app ID (or name).
fn shortcut_target(lnk: &str) -> (String, String) {
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else {
            return (String::new(), String::new());
        };
        let Ok(file) = link.cast::<IPersistFile>() else { return (String::new(), String::new()) };
        if file.Load(&HSTRING::from(lnk), STGM_READ).is_err() {
            return (String::new(), String::new());
        }
        let mut buf = [0u16; 1024];
        let mut fd = WIN32_FIND_DATAW::default();
        let target = if link.GetPath(&mut buf, &mut fd, 0).is_ok() {
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..len]).to_lowercase()
        } else {
            String::new()
        };
        if !target.is_empty() {
            let mut args = [0u16; 1024];
            let args = if link.GetArguments(&mut args).is_ok() {
                let len = args.iter().position(|&c| c == 0).unwrap_or(args.len());
                String::from_utf16_lossy(&args[..len])
            } else {
                String::new()
            };
            return (target, args);
        }
        let aumid = shortcut_app_id(&link);
        let stem = Path::new(lnk).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        if aumid.eq_ignore_ascii_case("Microsoft.Windows.Explorer") || stem == "file explorer" {
            return (explorer_exe(), String::new());
        }
        (String::new(), String::new())
    }
}

/// Squirrel installers (Discord, Slack …) pin `Update.exe --processStart
/// Discord.exe`; the app itself runs from a versioned subfolder. Returns
/// the app's exe name, lower-case.
fn squirrel_app(target: &str, args: &str) -> Option<String> {
    if !target.ends_with("\\update.exe") {
        return None;
    }
    let mut parts = args.split_whitespace();
    while let Some(a) = parts.next() {
        if a.eq_ignore_ascii_case("--processStart") {
            return parts.next().map(|n| n.trim_matches('"').to_lowercase()).filter(|n| !n.is_empty());
        }
    }
    None
}

/// A shortcut's AppUserModelID (the ID the taskbar groups windows by).
fn shortcut_app_id(link: &IShellLinkW) -> String {
    unsafe {
        let Ok(store) = link.cast::<IPropertyStore>() else { return String::new() };
        // PKEY_AppUserModel_ID, built from an existing key of the same type.
        let mut key = PKEY_Device_FriendlyName;
        key.fmtid = GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3);
        key.pid = 5;
        store.GetValue(&key).map(|v| v.to_string()).unwrap_or_default()
    }
}

fn explorer_exe() -> String {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{windir}\\explorer.exe").to_lowercase()
}

/// Folders that hold many unrelated apps, so sharing one says nothing.
fn shared_folders() -> Vec<String> {
    let mut out = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "LOCALAPPDATA", "APPDATA", "USERPROFILE", "SystemDrive"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim_end_matches('\\').to_lowercase();
            out.push(format!("{v}\\windowsapps"));
            out.push(format!("{v}\\programs"));
            out.push(v);
        }
    }
    out
}

/// The folder an app is installed in, if it's specific to that app (not
/// Windows itself, Program Files, AppData and the like).
fn app_folder(exe: &str, shared: &[String]) -> Option<String> {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    app_folder_in(exe, shared, &windir)
}

fn app_folder_in(exe: &str, shared: &[String], windir: &str) -> Option<String> {
    let dir = Path::new(exe).parent()?.to_string_lossy().to_lowercase();
    let windir = windir.to_lowercase();
    if dir.is_empty() || dir == windir || dir.starts_with(&format!("{windir}\\")) || shared.contains(&dir) {
        return None;
    }
    Some(dir)
}

/// Is `exe` in a subfolder of `folder` (not in `folder` itself)? Apps that
/// keep several programs side by side in one folder (Office's Word and
/// Excel) are separate apps; a helper lives in its own subfolder.
fn in_subfolder(exe: &str, folder: &str) -> bool {
    Path::new(exe)
        .parent()
        .map(|d| d.to_string_lossy().to_lowercase().starts_with(&format!("{folder}\\")))
        .unwrap_or(false)
}

thread_local! {
    static COMPANIES: std::cell::RefCell<std::collections::HashMap<String, Option<String>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Do two exes come from the same publisher (version-info "Company")? Keeps
/// games installed inside Steam's folder from counting as part of Steam.
fn same_company(a: &str, b: &str) -> bool {
    let company = |exe: &str| {
        COMPANIES.with(|c| {
            c.borrow_mut()
                .entry(exe.to_string())
                .or_insert_with(|| procinfo::company_name(exe).map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()))
                .clone()
        })
    };
    match (company(a), company(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Does a window's exe belong to a pin installed in `folder`?
fn window_matches_pin(window_exe: &str, pin: &Pin, folder: &str) -> bool {
    if !in_subfolder(window_exe, folder) {
        return false;
    }
    match &pin.squirrel_app {
        Some(app) => Path::new(window_exe).file_name().map(|f| f.to_string_lossy().to_lowercase()).as_ref() == Some(app),
        None => same_company(window_exe, &pin.exe),
    }
}

/// The exe that starts an app, given its windows. Apps like Steam draw
/// their windows from a helper (steamwebhelper.exe, several folders down)
/// that does nothing when launched on its own, so walk up each window's
/// parent processes while they belong to the same install and publisher,
/// and keep the result closest to the install's root.
pub fn launcher_for(windows: &[HWND]) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    // pid -> parent pid, from one snapshot of all processes.
    let mut parents = std::collections::HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    let shared = shared_folders();
    windows
        .iter()
        .filter_map(|&hwnd| {
            let mut pid = 0u32;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            let mut exe = procinfo::process_path(pid)?.to_lowercase();
            for _ in 0..8 {
                let Some(&parent) = parents.get(&pid) else { break };
                if parent == 0 || parent == pid {
                    break;
                }
                // If the parent exited and its id was reused, this finds an
                // unrelated exe, which the checks below reject.
                let Some(parent_exe) = procinfo::process_path(parent).map(|p| p.to_lowercase()) else { break };
                let Some(folder) = app_folder(&parent_exe, &shared) else { break };
                let same_dir = Path::new(&exe).parent().map(|d| d.to_string_lossy().to_lowercase()).as_ref() == Some(&folder);
                if !(same_dir || in_subfolder(&exe, &folder)) || !(parent_exe == exe || same_company(&parent_exe, &exe)) {
                    break;
                }
                exe = parent_exe;
                pid = parent;
            }
            Some(exe)
        })
        .min_by_key(|exe| exe.matches('\\').count())
}

fn pin_from_path(path: &str) -> Pin {
    let lower = path.to_lowercase();
    let (name, exe) = if lower.ends_with(".lnk") {
        let name = Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let (exe, args) = shortcut_target(path);
        let squirrel_app = squirrel_app(&exe, &args);
        return Pin { path: path.to_string(), name, exe, squirrel_app };
    } else {
        (procinfo::app_name(path), lower)
    };
    Pin { path: path.to_string(), name, exe, squirrel_app: None }
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
    let pins: Vec<Pin> = paths.iter().map(|p| pin_from_path(p)).collect();
    for p in &pins {
        // File names only, so the log doesn't carry the user name or paths.
        let file = |p: &str| Path::new(p).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        let starts = p.squirrel_app.as_ref().map(|a| format!(" (starts {a})")).unwrap_or_default();
        crate::log::write(&format!("dock pin {} -> {:?}{starts}", file(&p.path), file(&p.exe)));
    }
    pins
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
    let shared = shared_folders();
    let pin_folders: Vec<Option<String>> = pins.iter().map(|p| app_folder(&p.exe, &shared)).collect();
    for w in windows {
        // Store apps all share one host exe, so group them per window.
        let key_exe = if is_store_host(&w.exe) { String::new() } else { w.exe.clone() };
        if !key_exe.is_empty() {
            if let Some(item) = items.iter_mut().find(|i| i.exe == key_exe) {
                item.windows.push(w.hwnd);
                continue;
            }
            // A window from another exe of a pinned app's install, e.g.
            // Discord.exe under a pin to Discord's Update.exe launcher, or
            // steamwebhelper.exe under a pin to steam.exe. The most specific
            // (deepest) matching install folder wins.
            let best = pin_folders
                .iter()
                .enumerate()
                .filter_map(|(i, f)| f.as_ref().filter(|f| window_matches_pin(&key_exe, &pins[i], f)).map(|f| (i, f.len())))
                .max_by_key(|&(_, len)| len)
                .map(|(i, _)| i);
            if let Some(i) = best {
                items[i].windows.push(w.hwnd);
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

/// Is one of betterUI's flyouts (audio, Bluetooth, control center, tray)
/// in front? The bars and the dock themselves don't count: the bar becomes
/// the foreground window when its right-click menu opens, and that must not
/// keep it from hiding.
pub fn own_flyout_in_front() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe {
        let fg = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        pid == GetCurrentProcessId()
            && matches!(
                class_name(fg).as_str(),
                "betterUI.AudioFlyout" | "betterUI.Panel" | "betterUI.TrayFlyout"
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Vec<String> {
        ["c:\\program files", "c:\\program files (x86)", "c:\\users\\me\\appdata\\local", "c:\\users\\me"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn app_folder_skips_shared_and_windows_folders() {
        let sh = shared();
        assert_eq!(app_folder_in("c:\\windows\\explorer.exe", &sh, "C:\\Windows"), None);
        assert_eq!(app_folder_in("c:\\windows\\system32\\notepad.exe", &sh, "C:\\Windows"), None);
        assert_eq!(app_folder_in("c:\\program files\\tool.exe", &sh, "C:\\Windows"), None);
        assert_eq!(
            app_folder_in("c:\\program files (x86)\\steam\\steam.exe", &sh, "C:\\Windows").as_deref(),
            Some("c:\\program files (x86)\\steam")
        );
    }

    #[test]
    fn subfolder_only_not_same_folder() {
        let office = "c:\\program files\\microsoft office\\root\\office16";
        assert!(!in_subfolder("c:\\program files\\microsoft office\\root\\office16\\excel.exe", office));
        let steam = "c:\\program files (x86)\\steam";
        assert!(in_subfolder("c:\\program files (x86)\\steam\\bin\\cef\\cef.win64\\steamwebhelper.exe", steam));
        assert!(!in_subfolder("c:\\program files (x86)\\steamextra\\x.exe", steam));
    }

    #[test]
    fn squirrel_shortcuts() {
        let t = "c:\\users\\me\\appdata\\local\\discord\\update.exe";
        assert_eq!(squirrel_app(t, "--processStart Discord.exe").as_deref(), Some("discord.exe"));
        assert_eq!(squirrel_app(t, "--processStart \"Discord.exe\"").as_deref(), Some("discord.exe"));
        assert_eq!(squirrel_app(t, ""), None);
        assert_eq!(squirrel_app("c:\\apps\\code.exe", "--processStart Discord.exe"), None);
    }

    #[test]
    fn squirrel_pin_matches_only_its_app() {
        let pin = Pin {
            path: "Discord.lnk".into(),
            name: "Discord".into(),
            exe: "c:\\users\\me\\appdata\\local\\discord\\update.exe".into(),
            squirrel_app: Some("discord.exe".into()),
        };
        let folder = "c:\\users\\me\\appdata\\local\\discord";
        assert!(window_matches_pin("c:\\users\\me\\appdata\\local\\discord\\app-1.0.9\\discord.exe", &pin, folder));
        assert!(!window_matches_pin("c:\\users\\me\\appdata\\local\\discord\\app-1.0.9\\other.exe", &pin, folder));
    }
}
