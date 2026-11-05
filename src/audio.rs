// Core Audio wrapper: devices, master volume, default-device switching and
// per-app volume (audio sessions).
//
// Everything is read on demand (polled) rather than through change callbacks.
// The calls are cheap, and polling can't get out of sync or leave a dangling
// callback when a device disappears, which is a common source of flaky audio
// widgets.

use std::collections::HashMap;
use std::ffi::c_void;

use windows::core::{w, Interface, HSTRING, PWSTR, Result};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, AudioSessionStateExpired,
    EDataFlow, IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};


#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    Output,
    Input,
}

impl Flow {
    fn edf(self) -> EDataFlow {
        match self {
            Flow::Output => eRender,
            Flow::Input => eCapture,
        }
    }
}

#[derive(Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Copy)]
pub struct Level {
    pub volume: f32,
    pub muted: bool,
}

/// One row in the mixer: every audio session of one app.
#[derive(Clone)]
pub struct App {
    pub name: String,
    pub volume: f32,
    pub muted: bool,
    controls: Vec<ISimpleAudioVolume>,
}

impl App {
    pub fn set_volume(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        for c in &self.controls {
            unsafe {
                let _ = c.SetMasterVolume(v, std::ptr::null());
                if self.muted && v > 0.0 {
                    let _ = c.SetMute(false, std::ptr::null());
                }
            }
        }
        self.volume = v;
        if v > 0.0 {
            self.muted = false;
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        for c in &self.controls {
            unsafe {
                let _ = c.SetMute(muted, std::ptr::null());
            }
        }
        self.muted = muted;
    }
}

pub struct Audio {
    enumerator: IMMDeviceEnumerator,
    names: HashMap<u32, String>,
}

unsafe fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const c_void));
    s
}

impl Audio {
    pub fn new() -> Result<Self> {
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        Ok(Self { enumerator, names: HashMap::new() })
    }

    fn default_device(&self, flow: Flow) -> Option<IMMDevice> {
        unsafe { self.enumerator.GetDefaultAudioEndpoint(flow.edf(), eConsole).ok() }
    }

    fn endpoint(&self, flow: Flow) -> Option<IAudioEndpointVolume> {
        let dev = self.default_device(flow)?;
        unsafe { dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok() }
    }

    /// All active devices for a direction, sorted by name.
    pub fn devices(&self, flow: Flow) -> Vec<Device> {
        if demo() {
            return demo_devices(flow);
        }
        let default_id = self
            .default_device(flow)
            .and_then(|d| unsafe { d.GetId().ok().map(|p| take_pwstr(p)) });
        let mut out = Vec::new();
        unsafe {
            let Ok(col) = self.enumerator.EnumAudioEndpoints(flow.edf(), DEVICE_STATE_ACTIVE) else {
                return out;
            };
            let count = col.GetCount().unwrap_or(0);
            for i in 0..count {
                let Ok(dev) = col.Item(i) else { continue };
                let Ok(id) = dev.GetId().map(|p| take_pwstr(p)) else { continue };
                let name = dev
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| "Unknown device".to_string());
                let is_default = default_id.as_deref() == Some(id.as_str());
                out.push(Device { id, name, is_default });
            }
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    /// Volume and mute state of the default device, if there is one.
    pub fn level(&self, flow: Flow) -> Option<Level> {
        if demo() {
            return Some(match flow {
                Flow::Output => Level { volume: 0.62, muted: false },
                Flow::Input => Level { volume: 0.80, muted: false },
            });
        }
        let ep = self.endpoint(flow)?;
        unsafe {
            Some(Level {
                volume: ep.GetMasterVolumeLevelScalar().ok()?,
                muted: ep.GetMute().ok()?.as_bool(),
            })
        }
    }

    pub fn set_volume(&self, flow: Flow, v: f32) {
        let Some(ep) = self.endpoint(flow) else { return };
        let v = v.clamp(0.0, 1.0);
        unsafe {
            let _ = ep.SetMasterVolumeLevelScalar(v, std::ptr::null());
            // Dragging the slider up un-mutes, like the Windows flyout.
            if v > 0.0 && ep.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
                let _ = ep.SetMute(false, std::ptr::null());
            }
        }
    }

    pub fn set_muted(&self, flow: Flow, muted: bool) {
        if let Some(ep) = self.endpoint(flow) {
            unsafe {
                let _ = ep.SetMute(muted, std::ptr::null());
            }
        }
    }

    /// Make `id` the default device for all roles (apps, media, calls).
    pub fn set_default(&self, id: &str) {
        crate::policy::set_default_endpoint(id);
    }

    /// Apps with audio sessions on the default output device.
    pub fn apps(&mut self) -> Vec<App> {
        if demo() {
            return demo_apps();
        }
        let mut apps = self.apps_inner().unwrap_or_default();
        apps.sort_by(|a, b| {
            let sa = a.name == SYSTEM_SOUNDS;
            let sb = b.name == SYSTEM_SOUNDS;
            sa.cmp(&sb).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        if self.names.len() > 500 {
            self.names.clear();
        }
        apps
    }

    fn apps_inner(&mut self) -> Result<Vec<App>> {
        let mut apps: Vec<App> = Vec::new();
        let Some(dev) = self.default_device(Flow::Output) else { return Ok(apps) };
        unsafe {
            let mgr = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)?;
            let sessions = mgr.GetSessionEnumerator()?;
            for i in 0..sessions.GetCount()? {
                let Ok(ctl) = sessions.GetSession(i) else { continue };
                if ctl.GetState().map(|s| s == AudioSessionStateExpired).unwrap_or(true) {
                    continue;
                }
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
                let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() else { continue };
                let pid = ctl2.GetProcessId().unwrap_or(0);
                let name = if pid == 0 {
                    SYSTEM_SOUNDS.to_string()
                } else {
                    let display = ctl.GetDisplayName().map(|p| take_pwstr(p)).unwrap_or_default();
                    if !display.is_empty() && !display.starts_with('@') {
                        display
                    } else {
                        self.process_name(pid)
                    }
                };
                let volume = vol.GetMasterVolume().unwrap_or(1.0);
                let muted = vol.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                match apps.iter_mut().find(|a| a.name == name) {
                    Some(app) => app.controls.push(vol),
                    None => apps.push(App { name, volume, muted, controls: vec![vol] }),
                }
            }
        }
        Ok(apps)
    }

    fn process_name(&mut self, pid: u32) -> String {
        if let Some(n) = self.names.get(&pid) {
            return n.clone();
        }
        let name = process_path(pid)
            .map(|path| {
                file_description(&path).unwrap_or_else(|| {
                    let stem = std::path::Path::new(&path)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let mut c = stem.chars();
                    match c.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        None => stem,
                    }
                })
            })
            .unwrap_or_else(|| format!("Process {pid}"));
        self.names.insert(pid, name.clone());
        name
    }
}

pub const SYSTEM_SOUNDS: &str = "System sounds";

/// Demo mode (`--demo-audio`): made-up devices and apps so the CI screenshot
/// test can show the full flyout on machines without a sound card. Nothing
/// is read from or written to real audio devices in this mode.
static DEMO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn enable_demo() {
    DEMO.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn demo() -> bool {
    DEMO.load(std::sync::atomic::Ordering::Relaxed)
}

fn demo_devices(flow: Flow) -> Vec<Device> {
    let names: &[&str] = match flow {
        Flow::Output => &["Headphones (Arctis Nova 7)", "Speakers (Realtek(R) Audio)", "LG ULTRAGEAR (NVIDIA High Definition Audio)"],
        Flow::Input => &["Microphone (Arctis Nova 7)", "Microphone Array (Realtek(R) Audio)"],
    };
    names
        .iter()
        .enumerate()
        .map(|(i, n)| Device { id: format!("demo-{i}"), name: n.to_string(), is_default: i == 0 })
        .collect()
}

fn demo_apps() -> Vec<App> {
    [("Discord", 0.8, false), ("Google Chrome", 0.55, false), ("Spotify", 0.35, true), (SYSTEM_SOUNDS, 1.0, false)]
        .iter()
        .map(|(n, v, m)| App { name: n.to_string(), volume: *v, muted: *m, controls: Vec::new() })
        .collect()
}

fn process_path(pid: u32) -> Option<String> {
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
fn file_description(path: &str) -> Option<String> {
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
            "\\StringFileInfo\\{lang:04x}{codepage:04x}\\FileDescription"
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
