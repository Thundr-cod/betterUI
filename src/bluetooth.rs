// Paired Bluetooth devices and whether each is connected. Covers both
// classic devices (headphones, speakers) and Bluetooth LE (mice, keyboards,
// controllers). Blocking: only called from background threads.

use windows::core::Result;
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;

#[derive(Clone, Debug)]
pub struct BtDevice {
    pub name: String,
    pub connected: bool,
}

pub fn paired() -> Vec<BtDevice> {
    if crate::audio::demo() {
        return vec![
            BtDevice { name: "Arctis Nova 7".into(), connected: true },
            BtDevice { name: "MX Master 3S".into(), connected: true },
            BtDevice { name: "Xbox Wireless Controller".into(), connected: false },
        ];
    }
    let mut out: Vec<BtDevice> = Vec::new();
    for found in [classic(), low_energy()] {
        match found {
            Ok(list) => {
                for d in list {
                    // A device can show up as both classic and LE: keep one,
                    // connected if either says so.
                    match out.iter_mut().find(|e| e.name == d.name) {
                        Some(e) => e.connected |= d.connected,
                        None => out.push(d),
                    }
                }
            }
            Err(e) => crate::log::write(&format!("bluetooth list failed: {e:?}")),
        }
    }
    out.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

fn classic() -> Result<Vec<BtDevice>> {
    let selector = BluetoothDevice::GetDeviceSelectorFromPairingState(true)?;
    let infos = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.get()?;
    let mut out = Vec::new();
    for i in 0..infos.Size()? {
        let info = infos.GetAt(i)?;
        let name = info.Name()?.to_string();
        let connected = BluetoothDevice::FromIdAsync(&info.Id()?)
            .and_then(|op| op.get())
            .and_then(|d| d.ConnectionStatus())
            .map(|s| s == BluetoothConnectionStatus::Connected)
            .unwrap_or(false);
        if !name.is_empty() {
            out.push(BtDevice { name, connected });
        }
    }
    Ok(out)
}

fn low_energy() -> Result<Vec<BtDevice>> {
    let selector = BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)?;
    let infos = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.get()?;
    let mut out = Vec::new();
    for i in 0..infos.Size()? {
        let info = infos.GetAt(i)?;
        let name = info.Name()?.to_string();
        let connected = BluetoothLEDevice::FromIdAsync(&info.Id()?)
            .and_then(|op| op.get())
            .and_then(|d| d.ConnectionStatus())
            .map(|s| s == BluetoothConnectionStatus::Connected)
            .unwrap_or(false);
        if !name.is_empty() {
            out.push(BtDevice { name, connected });
        }
    }
    Ok(out)
}
