//! Write serious errors to log.txt with the libraries on the call stack.
//!
//! betterUI sometimes crashes about a second after quitting ("betterUI
//! exited" is the last log line) with exception code 0x87A raised in
//! KERNELBASE.dll, and Windows keeps no stack for it. A vectored exception
//! handler is called by Windows first, before anything else handles the
//! error, so it can note where it came from. It only logs and never handles
//! the error itself.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, RtlCaptureStackBackTrace, EXCEPTION_POINTERS};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::Threading::GetCurrentThreadId;

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
/// The code seen in the quit crashes.
const QUIT_CRASH_CODE: u32 = 0x87A;
/// At most this many errors are logged per run, so a burst can't fill the log.
const MAX_LOGGED: u32 = 20;
static LOGGED: AtomicU32 = AtomicU32::new(0);

pub fn install() {
    unsafe {
        if AddVectoredExceptionHandler(1, Some(on_exception)).is_null() {
            crate::log::write("could not install the crash logger");
        }
    }
}

/// Worth logging: the quit-crash code, and any error-severity exception
/// (access violation, heap corruption …). Informational ones that Windows
/// libraries raise and handle all the time (thread names, C++ exceptions,
/// RPC) are skipped.
fn worth_logging(code: u32) -> bool {
    code == QUIT_CRASH_CODE || code >= 0xC000_0000
}

unsafe extern "system" fn on_exception(info: *mut EXCEPTION_POINTERS) -> i32 {
    let record = match info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) {
        Some(r) => r,
        None => return EXCEPTION_CONTINUE_SEARCH,
    };
    let code = record.ExceptionCode.0 as u32;
    if !worth_logging(code) || LOGGED.fetch_add(1, Ordering::Relaxed) >= MAX_LOGGED {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let mut frames = [std::ptr::null_mut(); 32];
    let n = RtlCaptureStackBackTrace(0, &mut frames, None) as usize;
    let stack: Vec<String> = frames[..n].iter().map(|&a| module_offset(a as usize)).collect();
    crate::log::write(&format!(
        "exception {code:#x} at {} on thread {}; stack: {}",
        module_offset(record.ExceptionAddress as usize),
        GetCurrentThreadId(),
        stack.join(" < ")
    ));
    EXCEPTION_CONTINUE_SEARCH
}

/// `module.dll+0x1234` for a code address.
fn module_offset(addr: usize) -> String {
    unsafe {
        let mut module = HMODULE::default();
        let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        if GetModuleHandleExW(flags, PCWSTR(addr as *const u16), &mut module).is_err() {
            return format!("{addr:#x}");
        }
        let mut buf = [0u16; 260];
        let len = GetModuleFileNameW(module, &mut buf) as usize;
        let path = String::from_utf16_lossy(&buf[..len]);
        let name = path.rsplit('\\').next().unwrap_or(&path);
        format!("{name}+{:#x}", addr - module.0 as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_quit_crash_and_errors_only() {
        assert!(worth_logging(0x87A));
        assert!(worth_logging(0xC000_0005)); // access violation
        assert!(!worth_logging(0x406D_1388)); // thread name
        assert!(!worth_logging(0xE06D_7363)); // C++ exception, handled by its own code
        assert!(!worth_logging(0x6BA)); // RPC server unavailable
    }

    #[test]
    fn names_the_module_of_an_address() {
        let here = module_offset(names_the_module_of_an_address as usize);
        assert!(here.contains(".exe+0x"), "{here}");
    }
}
