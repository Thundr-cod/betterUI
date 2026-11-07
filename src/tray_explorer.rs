// The tray icons Explorer itself holds, read through ITrayNotify: the
// (undocumented, long-stable) COM interface Windows' own "notification area
// icons" settings use to list every tray icon. Interface and structure
// layouts as documented by Geoff Chappell and used by Squirrel.Windows.
//
// Why: some apps never send their icon to betterUI's tray. The NVIDIA app's
// icon belongs to a background process that ignores the "TaskbarCreated"
// broadcast (for example because its window is a message-only window, which
// broadcasts don't reach), so its icon only ever lives in Explorer's tray.
// This lists what Explorer has, so betterUI can ask those apps directly or,
// failing that, show Explorer's copy.

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::S_OK;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CopyIcon, DispatchMessageW, PeekMessageW, TranslateMessage, HICON, MSG, PM_REMOVE,
};

const CLSID_TRAY_NOTIFY: GUID = GUID::from_u128(0x25dead04_1eac_4911_9e3a_ad0a4ab560fd);

/// NOTIFYITEM: one icon in Explorer's notification area.
#[repr(C)]
pub struct NotifyItem {
    exe_name: *const u16,
    tip: *const u16,
    icon: isize,
    hwnd: isize,
    preference: u32,
    uid: u32,
    guid: GUID,
}

#[interface("D782CCBA-AFB0-43F1-94DB-FDA3779EACCB")]
unsafe trait INotificationCB: IUnknown {
    fn Notify(&self, event: u32, item: *const NotifyItem) -> HRESULT;
}

/// The Windows 8 and later version of ITrayNotify.
#[interface("D133CE13-3537-48BA-93A7-AFCD5D2053B4")]
unsafe trait ITrayNotify: IUnknown {
    fn RegisterCallback(&self, callback: *mut core::ffi::c_void, handle: *mut u32) -> HRESULT;
    fn UnregisterCallback(&self, handle: u32) -> HRESULT;
}

/// An icon as Explorer knows it, copied out of the callback.
#[derive(Clone, Debug)]
pub struct ExplorerIcon {
    pub exe: String,
    pub tip: String,
    /// Our own copy (CopyIcon); the receiver owns it.
    pub icon: Option<HICON>,
    pub hwnd: isize,
    pub uid: u32,
    pub guid: Option<GUID>,
}

#[implement(INotificationCB)]
struct Collector {
    items: Rc<RefCell<Vec<ExplorerIcon>>>,
}

impl INotificationCB_Impl for Collector_Impl {
    unsafe fn Notify(&self, event: u32, item: *const NotifyItem) -> HRESULT {
        // 0 = added (also how existing icons are reported), 1 = modified.
        if item.is_null() || event > 1 {
            return S_OK;
        }
        let it = &*item;
        let icon = if it.icon != 0 { CopyIcon(HICON(it.icon as *mut _)).ok() } else { None };
        let guid = if it.guid == GUID::zeroed() { None } else { Some(it.guid) };
        let mut items = self.items.borrow_mut();
        if let Some(existing) = items.iter_mut().find(|e| e.hwnd == it.hwnd && e.uid == it.uid && e.guid == guid) {
            existing.tip = pwstr(it.tip);
            if icon.is_some() {
                existing.icon = icon;
            }
        } else {
            items.push(ExplorerIcon { exe: pwstr(it.exe_name), tip: pwstr(it.tip), icon, hwnd: it.hwnd, uid: it.uid, guid });
        }
        S_OK
    }
}

unsafe fn pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while *p.add(n) != 0 && n < 4096 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}

/// List Explorer's tray icons. Blocks for about half a second; call it off
/// the UI thread. Registers a callback, which Explorer answers with every
/// icon it has, then unregisters straight away (Explorer keeps only one
/// callback, so holding it would break the Settings page that uses it).
pub fn list() -> std::result::Result<Vec<ExplorerIcon>, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = list_inner();
        CoUninitialize();
        result
    }
}

unsafe fn list_inner() -> std::result::Result<Vec<ExplorerIcon>, String> {
    let tray: ITrayNotify =
        CoCreateInstance(&CLSID_TRAY_NOTIFY, None, CLSCTX_LOCAL_SERVER).map_err(|e| format!("TrayNotify unavailable: {e}"))?;
    let items = Rc::new(RefCell::new(Vec::new()));
    let collector: INotificationCB = Collector { items: items.clone() }.into();
    let mut handle = 0u32;
    tray.RegisterCallback(collector.as_raw(), &mut handle).ok().map_err(|e| format!("RegisterCallback failed: {e}"))?;
    // Explorer may answer during the call or just after it: let calls in
    // for a moment.
    let start = std::time::Instant::now();
    let mut msg = MSG::default();
    while start.elapsed() < std::time::Duration::from_millis(500) {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if let Err(e) = tray.UnregisterCallback(handle).ok() {
        crate::log::write(&format!("tray: UnregisterCallback failed: {e}"));
    }
    drop(collector);
    let items = items.borrow().clone();
    Ok(items)
}
