// Watchdog: puts the Windows taskbar back when betterUI is ended without a
// chance to do it itself, e.g. Task Manager's End task (which kills the
// process outright) or a crash the panic hook can't catch.
//
// It's a second copy of betterui.exe, started with --watchdog=<pid>, that
// waits for the main process to exit. A clean exit restores the taskbar and
// removes taskbar_state.txt first, so the watchdog only acts when that file
// is still there afterwards. It does nothing if another betterUI has
// already started, since that copy owns the taskbar now.

use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::core::w;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenMutexW, OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Start a watchdog for this process. It goes through a short-lived middle
/// process so the watchdog isn't betterUI's child: Task Manager's End task
/// ends an app's child processes along with it.
pub fn start() {
    let Ok(exe) = std::env::current_exe() else { return };
    let pid = std::process::id();
    if let Err(e) = Command::new(exe).arg(format!("--watchdog-start={pid}")).creation_flags(CREATE_NO_WINDOW).spawn() {
        crate::log::write(&format!("could not start the watchdog: {e}"));
    }
}

/// The middle process: start the real watchdog and exit straight away.
pub fn start_detached(pid: u32) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).arg(format!("--watchdog={pid}")).creation_flags(CREATE_NO_WINDOW).spawn();
    }
}

/// Wait for betterUI (`pid`) to exit, then put the taskbar back if it
/// didn't.
pub fn watch(pid: u32) {
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return };
        WaitForSingleObject(process, INFINITE);
        let _ = CloseHandle(process);
    }
    if !crate::taskbar::hidden_by_us() || another_instance_running() {
        return;
    }
    crate::log::write("betterUI ended without putting the taskbar back (End task or crash); the watchdog restored it");
    crate::taskbar::restore_and_settle();
}

fn another_instance_running() -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\betterUI.singleton")) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}
