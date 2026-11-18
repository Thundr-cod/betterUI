// Process helpers shared by the audio mixer and the dock: a process's exe
// path and the friendly name from the exe's version info.

use std::ffi::c_void;

use windows::core::{w, HSTRING, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Friendly name for an exe: its "File description", else the file name.
pub fn app_name(path: &str) -> String {
    file_description(path).unwrap_or_else(|| {
        let stem = std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut c = stem.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => stem,
        }
    })
}

pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The "File description" from an exe's version info, e.g. "Google Chrome".
pub fn file_description(path: &str) -> Option<String> {
    version_string(path, "FileDescription")
}

/// The "Company" from an exe's version info, e.g. "Valve Corporation".
pub fn company_name(path: &str) -> Option<String> {
    version_string(path, "CompanyName")
}

fn version_string(path: &str, field: &str) -> Option<String> {
    unsafe {
        let wpath = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&wpath, None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(&wpath, 0, size, buf.as_mut_ptr() as *mut c_void).ok()?;

        let mut p: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(
            buf.as_ptr() as *const c_void,
            w!("\\VarFileInfo\\Translation"),
            &mut p,
            &mut len,
        )
        .as_bool()
            || len < 4
            || p.is_null()
        {
            return None;
        }
        let lang = *(p as *const u16);
        let codepage = *(p as *const u16).add(1);
        let query = HSTRING::from(format!(
            "\\StringFileInfo\\{lang:04x}{codepage:04x}\\{field}"
        ));
        if !VerQueryValueW(buf.as_ptr() as *const c_void, &query, &mut p, &mut len).as_bool()
            || len < 2
            || p.is_null()
        {
            return None;
        }
        let text = std::slice::from_raw_parts(p as *const u16, len as usize - 1);
        let s = String::from_utf16_lossy(text).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}
