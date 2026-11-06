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
mod testhooks;
mod tray;
mod tray_flyout;
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
    autostart::refresh_path();
    if let Err(e) = bar::run() {
        log::write(&format!("fatal error: {e:?}"));
    }

    // Always leave the desktop the way we found it.
    taskbar::restore();
    log::write("betterUI exited");
}
