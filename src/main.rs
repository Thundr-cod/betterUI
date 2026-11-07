// betterUI — a lightweight native desktop shell for Windows 11.
#![windows_subsystem = "windows"]

mod appbar;
mod apps;
mod audio;
mod audio_flyout;
mod autostart;
mod backdrop;
mod bluetooth;
mod brightness;
mod bar;
mod config;
mod dock;
mod icons;
mod log;
mod panel;
mod policy;
mod popup;
mod procinfo;
mod radios;
mod render;
mod sysinfo;
mod taskbar;
mod tray;
mod tray_flyout;
mod tray_explorer;
mod ui;

use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Emergency escape hatch: `betterui.exe --restore-taskbar` brings the
    // Windows taskbar back exactly as it was, without starting the shell.
    // `--start-with-windows=on|off` changes the setting and exits.
    if let Some(v) = args.iter().find_map(|a| a.strip_prefix("--start-with-windows=")) {
        autostart::set_enabled(v == "on");
        return;
    }

    if args.iter().any(|a| a == "--restore-taskbar") {
        taskbar::restore();
        return;
    }

    // Demo data for the CI screenshot test (no real devices are touched).
    if args.iter().any(|a| a == "--demo-audio" || a == "--demo") {
        audio::enable_demo();
    }

    // Only one copy may run at a time.
    let _mutex = unsafe { CreateMutexW(None, false, w!("Local\\betterUI.singleton")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    // If anything ever panics, put the taskbar back before the process dies.
    std::panic::set_hook(Box::new(|info| {
        log::write(&format!("PANIC: {info}"));
        taskbar::restore();
    }));

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    log::write("betterUI starting");
    wait_for_explorer_taskbar();
    autostart::refresh_path();
    if let Err(e) = bar::run() {
        log::write(&format!("fatal error: {e:?}"));
    }

    // Always leave the desktop the way we found it.
    taskbar::restore();
    log::write("betterUI exited");
}

/// Started at sign-in, betterUI can be up before Explorer has made its
/// taskbar. Hiding the taskbar and taking over the tray only work once it
/// exists, so wait for it (at most 60 s, e.g. if Explorer isn't running at
/// all), then give Explorer a moment to finish setting it up.
fn wait_for_explorer_taskbar() {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    let found = || unsafe { FindWindowW(w!("Shell_TrayWnd"), None).map(|h| !h.is_invalid()).unwrap_or(false) };
    if found() {
        return;
    }
    let started = std::time::Instant::now();
    while !found() && started.elapsed().as_secs() < 60 {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let waited = started.elapsed().as_secs_f32();
    log::write(&format!("waited {waited:.1} s for Explorer's taskbar (found: {})", found()));
    if found() {
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}
