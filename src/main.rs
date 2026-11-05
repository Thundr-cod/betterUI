// betterUI — a lightweight native desktop shell for Windows 11.
#![windows_subsystem = "windows"]

mod appbar;
mod audio;
mod audio_flyout;
mod backdrop;
mod bar;
mod config;
mod log;
mod policy;
mod render;
mod sysinfo;
mod taskbar;
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
    if args.iter().any(|a| a == "--restore-taskbar") {
        taskbar::restore();
        return;
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
    if let Err(e) = bar::run() {
        log::write(&format!("fatal error: {e:?}"));
    }

    // Always leave the desktop the way we found it.
    taskbar::restore();
    log::write("betterUI exited");
}
