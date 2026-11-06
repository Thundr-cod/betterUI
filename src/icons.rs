// App icons for the dock and tray, loaded through the Windows shell (the same icons
// Explorer and the taskbar show) and converted to WIC images that Direct2D
// can draw. Results are cached by path, including failures.

use std::collections::HashMap;

use windows::core::{Interface, Result, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource,
    IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic,
    WICBitmapPaletteTypeCustom, WICBitmapUseAlpha,
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
    pub fn for_window(&mut self, hwnd: HWND, key: &str, size: i32) -> Option<IWICBitmapSource> {
        let key = format!("window|{size}|{key}");
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
                self.from_hicon(HICON(hicon as *mut _), size).ok()
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
            let bitmap = self.wic.CreateBitmapFromHBITMAP(hbmp, None, WICBitmapUseAlpha);
            let _ = DeleteObject(hbmp);
            let source = self.fix_alpha(&bitmap?.cast()?)?;
            self.to_pbgra(&source, size)
        }
    }

    /// The shell hands back some icons with straight alpha and some already
    /// premultiplied, in the same kind of bitmap. Treating straight alpha as
    /// premultiplied makes see-through edge pixels too bright (white specks
    /// on rounded corners and soft shadows), so look at the pixels: any colour
    /// value above its alpha can only be straight alpha.
    fn fix_alpha(&self, source: &IWICBitmapSource) -> Result<IWICBitmapSource> {
        unsafe {
            let format = source.GetPixelFormat()?;
            if format != GUID_WICPixelFormat32bppBGRA {
                // Not expected from a 32-bit shell bitmap; keep the old
                // behaviour and note it, so a wrong-looking icon can be traced.
                crate::log::write(&format!("icon in unexpected pixel format {format:?}; alpha left as is"));
                return Ok(source.clone());
            }
            let (mut w, mut h) = (0u32, 0u32);
            source.GetSize(&mut w, &mut h)?;
            let stride = w * 4;
            let mut pixels = vec![0u8; (stride * h) as usize];
            source.CopyPixels(std::ptr::null(), stride, &mut pixels)?;
            let straight = pixels.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
            let format = if straight { GUID_WICPixelFormat32bppBGRA } else { GUID_WICPixelFormat32bppPBGRA };
            let bitmap = self.wic.CreateBitmapFromMemory(w, h, &format, stride, &pixels)?;
            bitmap.cast()
        }
    }

    /// Convert an icon handle (e.g. a tray icon) without caching it.
    pub fn for_hicon(&self, hicon: HICON, size: i32) -> Option<IWICBitmapSource> {
        self.from_hicon(hicon, size).ok()
    }

    fn from_hicon(&self, hicon: HICON, size: i32) -> Result<IWICBitmapSource> {
        unsafe {
            let bitmap = self.wic.CreateBitmapFromHICON(hicon)?;
            self.to_pbgra(&bitmap.cast()?, size)
        }
    }

    /// Convert to the premultiplied BGRA format Direct2D expects, resize it
    /// once with a high-quality filter so its longer side is exactly `size`
    /// pixels (keeping its shape, so Direct2D draws it 1:1 instead of
    /// shrinking a 256 px icon with a rough filter), and copy it so it no
    /// longer depends on the source handle.
    fn to_pbgra(&self, source: &IWICBitmapSource, size: i32) -> Result<IWICBitmapSource> {
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
            let converted: IWICBitmapSource = converter.cast()?;
            let (mut w, mut h) = (0u32, 0u32);
            converted.GetSize(&mut w, &mut h)?;
            let size = size.max(1) as f64;
            let fit = size / w.max(h).max(1) as f64;
            let nw = ((w as f64 * fit).round() as u32).max(1);
            let nh = ((h as f64 * fit).round() as u32).max(1);
            let sized: IWICBitmapSource = if nw == w && nh == h {
                converted
            } else {
                // Scaling premultiplied pixels keeps edges free of dark or
                // light fringes.
                let scaler = self.wic.CreateBitmapScaler()?;
                scaler.Initialize(&converted, nw, nh, WICBitmapInterpolationModeHighQualityCubic)?;
                scaler.cast()?
            };
            let copy = self.wic.CreateBitmapFromSource(&sized, WICBitmapCacheOnLoad)?;
            copy.cast()
        }
    }
}
