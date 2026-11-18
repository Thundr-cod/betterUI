// betterUI — a lightweight native desktop shell for Windows 11.
#![windows_subsystem = "windows"]

mod autostart;
mod config;
mod flyouts;
mod log;
mod shell;
mod storage;
mod system;
mod ui;
mod watchdog;

use shell::{bar, taskbar, tray};

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

    // The watchdog that restores the taskbar if betterUI is killed
    // (watchdog.rs), and the short-lived process that starts it.
    if let Some(pid) = args.iter().find_map(|a| a.strip_prefix("--watchdog-start=")).and_then(|p| p.parse().ok()) {
        watchdog::start_detached(pid);
        return;
    }
    if let Some(pid) = args.iter().find_map(|a| a.strip_prefix("--watchdog=")).and_then(|p| p.parse().ok()) {
        watchdog::watch(pid);
        return;
    }

    // Only one copy may run at a time.
    let mutex = unsafe { CreateMutexW(None, false, w!("Local\\betterUI.singleton")) };
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

    log::write(&format!("betterUI {} starting", env!("CARGO_PKG_VERSION")));
    storage::remove_legacy_files();
    watchdog::start();
    log_memory_use();
    wait_for_explorer_taskbar();
    autostart::refresh_path();
    if let Err(e) = bar::run() {
        log::write(&format!("fatal error: {e:?}"));
    }

    // Always leave the desktop the way we found it. Nothing may hide the
    // taskbar from here on (taskbar.rs, QUITTING). Let a new betterUI
    // start while the restore settles (it takes a few seconds).
    if let Ok(m) = mutex {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(m);
        }
    }
    taskbar::set_quitting();
    tray::stop();
    taskbar::restore_and_settle();
    log::write("betterUI exited");
    // End here, skipping the late cleanup of thread-local window state: the
    // flyouts' Direct2D objects released that late made DXGI raise 0x87A
    // (logged on every manual quit in v0.0.911). Everything that matters is
    // already saved, and Windows frees the rest.
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        let _ = TerminateProcess(GetCurrentProcess(), 0);
    }
}

/// Note betterUI's memory use in log.txt a minute after starting and then
/// every hour, so a slow leak shows up without anyone watching Task Manager.
fn log_memory_use() {
    std::thread::spawn(|| {
        use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut wait = std::time::Duration::from_secs(60);
        loop {
            std::thread::sleep(wait);
            wait = std::time::Duration::from_secs(3600);
            let mut pmc = PROCESS_MEMORY_COUNTERS_EX::default();
            let ok = unsafe {
                GetProcessMemoryInfo(
                    GetCurrentProcess(),
                    &mut pmc as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                    std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                )
            };
            if ok.is_ok() {
                // Counts too, so a jump in memory can be matched to what grew.
                let (handles, gdi, user) = unsafe {
                    use windows::Win32::System::Threading::{GetGuiResources, GetProcessHandleCount, GR_GDIOBJECTS, GR_USEROBJECTS};
                    let mut handles = 0u32;
                    let _ = GetProcessHandleCount(GetCurrentProcess(), &mut handles);
                    (handles, GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS), GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS))
                };
                log::write(&format!(
                    "memory: {} K working set, {} K private; {handles} handles, {gdi} GDI, {user} USER objects, {} threads",
                    pmc.WorkingSetSize / 1024,
                    pmc.PrivateUsage / 1024,
                    thread_count()
                ));
            }
        }
    });
}

/// How many threads betterUI has (background work that never finished shows up here).
fn thread_count() -> usize {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32};
    let pid = std::process::id();
    let mut count = 0;
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else { return 0 };
        let mut entry = THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
        let mut more = Thread32First(snap, &mut entry).is_ok();
        while more {
            if entry.th32OwnerProcessID == pid {
                count += 1;
            }
            more = Thread32Next(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    count
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
