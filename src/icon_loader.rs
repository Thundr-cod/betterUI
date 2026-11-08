// Loads dock icons on a background thread, so a slow one never stalls the
// dock. A Store app's icon comes from the Apps folder, and the first load
// can take a few hundred milliseconds; done on the dock's own thread that
// froze drawing and made icons pop in. The worker hands back plain
// premultiplied BGRA pixels, which the dock turns into a bitmap when it
// draws.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::IWICBitmapSource;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::icons::Icons;

pub enum Source {
    /// An exe or shortcut (a pin, or a running app's exe).
    Path(String),
    /// A window, for apps whose exe says nothing (Store apps).
    Window(isize),
}

pub struct Request {
    pub key: String,
    pub source: Source,
    pub size: i32,
}

/// Premultiplied BGRA, 4 bytes per pixel, rows packed.
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

pub struct Loaded {
    pub key: String,
    pub pixels: Option<Pixels>,
}

pub struct Loader {
    tx: Sender<Request>,
    done: Arc<Mutex<Vec<Loaded>>>,
}

impl Loader {
    /// Start the worker. Each finished icon posts `message` to `notify`.
    /// The worker ends when the Loader is dropped.
    pub fn start(notify: HWND, message: u32) -> Loader {
        let (tx, rx) = channel::<Request>();
        let done = Arc::new(Mutex::new(Vec::new()));
        let results = done.clone();
        let target = notify.0 as isize;
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let mut icons = Icons::new().map_err(|e| crate::log::write(&format!("icon loader unavailable: {e:?}"))).ok();
            while let Ok(req) = rx.recv() {
                let started = std::time::Instant::now();
                let pixels = icons.as_mut().and_then(|icons| {
                    let source = match &req.source {
                        Source::Path(p) => icons.for_path(p, req.size),
                        Source::Window(h) => icons.for_window(HWND(*h as *mut _), &req.key, req.size),
                    };
                    source.and_then(|s| pixels_of(&s))
                });
                let ms = started.elapsed().as_millis();
                if ms >= 200 {
                    crate::log::write(&format!("dock icon slow: {} took {ms} ms", req.key));
                }
                if let Ok(mut d) = results.lock() {
                    d.push(Loaded { key: req.key, pixels });
                }
                unsafe {
                    let _ = PostMessageW(HWND(target as *mut _), message, WPARAM(0), LPARAM(0));
                }
            }
        });
        Loader { tx, done }
    }

    pub fn request(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    pub fn take_done(&self) -> Vec<Loaded> {
        self.done.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default()
    }
}

fn pixels_of(source: &IWICBitmapSource) -> Option<Pixels> {
    unsafe {
        let (mut width, mut height) = (0u32, 0u32);
        source.GetSize(&mut width, &mut height).ok()?;
        let mut data = vec![0u8; (width * height * 4) as usize];
        source.CopyPixels(std::ptr::null(), width * 4, &mut data).ok()?;
        Some(Pixels { width, height, data })
    }
}
