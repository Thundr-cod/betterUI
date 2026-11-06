// Wi-Fi and Bluetooth on/off, through the Windows.Devices.Radios API (the
// same switches as Windows' quick settings). These calls can take a moment,
// so they are only ever made from background threads.

use windows::core::Result;
use windows::Devices::Radios::{Radio, RadioKind, RadioState};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct RadioStates {
    /// None = this PC has no such radio.
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
}

/// Demo mode (CI recordings): Wi-Fi and Bluetooth on/off, remembered.
static DEMO_RADIOS: std::sync::Mutex<(bool, bool)> = std::sync::Mutex::new((true, true));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Wifi,
    Bluetooth,
}

fn radios() -> Result<Vec<Radio>> {
    // Desktop apps are normally allowed; the result is informational.
    if let Ok(op) = Radio::RequestAccessAsync() {
        let _ = op.get();
    }
    let list = Radio::GetRadiosAsync()?.get()?;
    let mut out = Vec::new();
    for i in 0..list.Size()? {
        out.push(list.GetAt(i)?);
    }
    Ok(out)
}

pub fn states() -> RadioStates {
    if crate::audio::demo() {
        let (wifi, bt) = *DEMO_RADIOS.lock().unwrap_or_else(|e| e.into_inner());
        return RadioStates { wifi: Some(wifi), bluetooth: Some(bt) };
    }
    let mut s = RadioStates::default();
    match radios() {
        Ok(list) => {
            for r in list {
                let on = r.State().map(|st| st == RadioState::On).unwrap_or(false);
                match r.Kind() {
                    Ok(k) if k == RadioKind::WiFi => s.wifi = Some(s.wifi.unwrap_or(false) || on),
                    Ok(k) if k == RadioKind::Bluetooth => {
                        s.bluetooth = Some(s.bluetooth.unwrap_or(false) || on)
                    }
                    _ => {}
                }
            }
        }
        Err(e) => crate::log::write(&format!("radio list failed: {e:?}")),
    }
    s
}

pub fn set(kind: Kind, on: bool) {
    if crate::audio::demo() {
        let mut d = DEMO_RADIOS.lock().unwrap_or_else(|e| e.into_inner());
        match kind {
            Kind::Wifi => d.0 = on,
            Kind::Bluetooth => d.1 = on,
        }
        return;
    }
    let want = match kind {
        Kind::Wifi => RadioKind::WiFi,
        Kind::Bluetooth => RadioKind::Bluetooth,
    };
    let state = if on { RadioState::On } else { RadioState::Off };
    match radios() {
        Ok(list) => {
            for r in list {
                if r.Kind().map(|k| k == want).unwrap_or(false) {
                    match r.SetStateAsync(state).and_then(|op| op.get()) {
                        Ok(status) => crate::log::write(&format!("radio {kind:?} -> {on}: {status:?}")),
                        Err(e) => crate::log::write(&format!("radio {kind:?} -> {on} failed: {e:?}")),
                    }
                }
            }
        }
        Err(e) => crate::log::write(&format!("radio list failed: {e:?}")),
    }
}
