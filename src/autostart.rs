// "Start with Windows": a value under
// HKCU\Software\Microsoft\Windows\CurrentVersion\Run, the same place apps
// register themselves when you tick "start at login". Per user, no admin
// rights needed, and it shows up (and can be switched off) in Task Manager's
// Startup apps.

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("betterUI");

/// The command line Windows runs at sign-in, if betterUI is registered.
fn registered_command() -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if rc.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn current_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

pub fn is_enabled() -> bool {
    registered_command().is_some()
}

pub fn set_enabled(on: bool) {
    if on {
        let Some(cmd) = current_command() else { return };
        let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
        let rc = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                REG_SZ.0,
                Some(wide.as_ptr() as *const _),
                (wide.len() * 2) as u32,
            )
        };
        if rc.is_err() {
            crate::log::write(&format!("could not enable start with Windows: {rc:?}"));
        }
    } else {
        let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
    }
}

/// If start-with-Windows is on but points at a different copy of the exe
/// (e.g. a newer release downloaded to another folder), point it at this one.
pub fn refresh_path() {
    if let (Some(registered), Some(current)) = (registered_command(), current_command()) {
        if !registered.eq_ignore_ascii_case(&current) {
            crate::log::write(&format!("start with Windows now points to {current}"));
            set_enabled(true);
        }
    }
}
