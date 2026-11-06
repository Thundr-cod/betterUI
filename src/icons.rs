// App icons for the dock, loaded through the Windows shell (the same icons
// Explorer and the taskbar show) and converted to WIC images that Direct2D
// can draw. Results are cached by path, including failures.

use std::collections::HashMap;

use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
    WICBitmapUsePremultipliedAlpha,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassLongPtrW, SendMessageTimeoutW, GCLP_HICON, HICON, ICON_BIG, SMTO_ABORTIFHUNG,
    WM_GETICON,
};

pub struct Icons {
    wic: IWICImagingFactory,
    cache: HashMap<String, Option<IWICBitmapSource>>,
}

impl Icons {
    pub fn new() -> Result<Self> {
        let wic = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        Ok(Self { wic, cache: HashMap::new() })
    }

    /// Icon for a file (exe or shortcut), `size` pixels square.
    pub fn for_path(&mut self, path: &str, size: i32) -> Option<IWICBitmapSource> {
        let key = format!("{size}|{}", path.to_lowercase());
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let loaded = self.load_path(path, size).ok();
        if loaded.is_none() {
            crate::log::write(&format!("no icon for {path}"));
        }
        self.cache.insert(key, loaded.clone());
        loaded
    }

    /// Icon a window shows in its title bar (used for Store apps, whose exe
    /// is a shared host process).
    pub fn for_window(&mut self, hwnd: HWND, key: &str) -> Option<IWICBitmapSource> {
        let key = format!("window|{key}");
        if let Some(Some(hit)) = self.cache.get(&key) {
            return Some(hit.clone());
        }
        let loaded = unsafe {
            let mut result = 0usize;
            let _ = SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                100,
                Some(&mut result),
            );
            let mut hicon = result as isize;
            if hicon == 0 {
                hicon = GetClassLongPtrW(hwnd, GCLP_HICON) as isize;
            }
            if hicon == 0 {
                None
            } else {
                self.from_hicon(HICON(hicon as *mut _)).ok()
            }
        };
        if loaded.is_some() {
            self.cache.insert(key, loaded.clone());
        }
        loaded
    }

    fn load_path(&self, path: &str, size: i32) -> Result<IWICBitmapSource> {
        unsafe {
            let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path), None)?;
            let hbmp = factory.GetImage(SIZE { cx: size, cy: size }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)?;
            let bitmap = self.wic.CreateBitmapFromHBITMAP(hbmp, None, WICBitmapUsePremultipliedAlpha);
            let _ = DeleteObject(hbmp);
            self.to_pbgra(&bitmap?.into())
        }
    }

    fn from_hicon(&self, hicon: HICON) -> Result<IWICBitmapSource> {
        unsafe {
            let bitmap = self.wic.CreateBitmapFromHICON(hicon)?;
            self.to_pbgra(&bitmap.into())
        }
    }

    /// Convert to the premultiplied BGRA format Direct2D expects, and copy it
    /// so it no longer depends on the source handle.
    fn to_pbgra(&self, source: &IWICBitmapSource) -> Result<IWICBitmapSource> {
        unsafe {
            let converter = self.wic.CreateFormatConverter()?;
            converter.Initialize(
                source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let copy = self.wic.CreateBitmapFromSource(&converter, WICBitmapCacheOnLoad)?;
            Ok(copy.into())
        }
    }
}
