// Core Audio wrapper: devices, master volume, default-device switching and
// per-app volume (audio sessions).
//
// Everything is read on demand (polled) rather than through change callbacks.
// The calls are cheap, and polling can't get out of sync or leave a dangling
// callback when a device disappears, which is a common source of flaky audio
// widgets.

use std::ffi::c_void;

use windows::core::{Interface, PWSTR, Result};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, AudioSessionStateActive, AudioSessionStateExpired,
    EDataFlow, IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use crate::system::procinfo::{app_name, process_path};


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
    /// Whether one of its sessions is playing right now (its volume is shown).
    playing: bool,
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
        Ok(Self { enumerator })
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
        out.sort_by_key(|d| d.name.to_lowercase());
        out
    }

    /// Volume and mute state of the default device, if there is one (what
    /// the master slider shows).
    pub fn level(&self, flow: Flow) -> Option<Level> {
        let ep = self.endpoint(flow)?;
        unsafe {
            Some(Level {
                volume: ep.GetMasterVolumeLevelScalar().ok()?,
                muted: ep.GetMute().ok()?.as_bool(),
            })
        }
    }

    /// What the master slider controls. Output: every active output device,
    /// the default one first, so it turns all audio up or down (a headset's
    /// separate Game and Chat devices included). Input: the default
    /// microphone.
    fn master_endpoints(&self, flow: Flow) -> Vec<IAudioEndpointVolume> {
        match flow {
            Flow::Output => self
                .output_devices()
                .iter()
                .filter_map(|d| unsafe { d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok() })
                .collect(),
            Flow::Input => self.endpoint(flow).into_iter().collect(),
        }
    }

    /// Set the master volume: the default device goes to `v`, and other
    /// output devices change by the same ratio, so their balance against it
    /// (e.g. game vs. chat) is kept.
    pub fn set_volume(&self, flow: Flow, v: f32) {
        let endpoints = self.master_endpoints(flow);
        let Some(main) = endpoints.first() else { return };
        let v = v.clamp(0.0, 1.0);
        unsafe {
            let old = main.GetMasterVolumeLevelScalar().unwrap_or(v);
            for (i, ep) in endpoints.iter().enumerate() {
                let target = if i == 0 || old < 0.01 {
                    v
                } else {
                    (ep.GetMasterVolumeLevelScalar().unwrap_or(old) * v / old).clamp(0.0, 1.0)
                };
                let _ = ep.SetMasterVolumeLevelScalar(target, std::ptr::null());
                // Dragging the slider up un-mutes, like the Windows flyout.
                if target > 0.0 && ep.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
                    let _ = ep.SetMute(false, std::ptr::null());
                }
            }
        }
    }

    pub fn set_muted(&self, flow: Flow, muted: bool) {
        for ep in self.master_endpoints(flow) {
            unsafe {
                let _ = ep.SetMute(muted, std::ptr::null());
            }
        }
    }

    /// Make `id` the default device for all roles (apps, media, calls).
    pub fn set_default(&self, id: &str) {
        crate::system::policy::set_default_endpoint(id);
    }

    /// Apps with audio sessions on any active output device. An app that
    /// plays on several devices (Discord: voice on a headset, notifications
    /// on the default speakers) is one row, and changing it changes all of
    /// its sessions.
    pub fn apps(&mut self) -> Vec<App> {
        let mut apps = self.apps_inner().unwrap_or_default();
        apps.sort_by(|a, b| {
            let sa = a.name == SYSTEM_SOUNDS;
            let sb = b.name == SYSTEM_SOUNDS;
            sa.cmp(&sb).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        apps
    }

    /// Every active output device, the default one first.
    fn output_devices(&self) -> Vec<IMMDevice> {
        let default_id = self.default_device(Flow::Output).and_then(|d| unsafe { d.GetId().ok().map(|p| take_pwstr(p)) });
        let mut out = Vec::new();
        unsafe {
            let Ok(col) = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return out };
            for i in 0..col.GetCount().unwrap_or(0) {
                let Ok(dev) = col.Item(i) else { continue };
                let is_default = dev.GetId().ok().map(|p| take_pwstr(p)) == default_id;
                if is_default {
                    out.insert(0, dev);
                } else {
                    out.push(dev);
                }
            }
        }
        out
    }

    fn apps_inner(&self) -> Result<Vec<App>> {
        let mut apps: Vec<App> = Vec::new();
        for dev in self.output_devices() {
            if let Err(e) = Self::add_sessions(&dev, &mut apps) {
                crate::log::write(&format!("audio sessions of a device unavailable: {e:?}"));
            }
        }
        Ok(apps)
    }

    /// Add one device's audio sessions to `apps`, merged by app name. The
    /// row shows the volume of a session that is playing, if any, else the
    /// first one found (default device first).
    fn add_sessions(dev: &IMMDevice, apps: &mut Vec<App>) -> Result<()> {
        unsafe {
            let mgr = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)?;
            let sessions = mgr.GetSessionEnumerator()?;
            for i in 0..sessions.GetCount()? {
                let Ok(ctl) = sessions.GetSession(i) else { continue };
                let Ok(state) = ctl.GetState() else { continue };
                if state == AudioSessionStateExpired {
                    continue;
                }
                let playing = state == AudioSessionStateActive;
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
                        process_path(pid).map(|p| app_name(&p)).unwrap_or_else(|| format!("Process {pid}"))
                    }
                };
                let volume = vol.GetMasterVolume().unwrap_or(1.0);
                let muted = vol.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                match apps.iter_mut().find(|a| a.name == name) {
                    Some(app) => {
                        if playing && !app.playing {
                            app.volume = volume;
                            app.muted = muted;
                            app.playing = true;
                        }
                        app.controls.push(vol);
                    }
                    None => apps.push(App { name, volume, muted, playing, controls: vec![vol] }),
                }
            }
        }
        Ok(())
    }
}

pub const SYSTEM_SOUNDS: &str = "System sounds";

