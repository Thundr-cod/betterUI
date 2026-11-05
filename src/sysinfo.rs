// System readouts: memory usage and network connection type.
// Both are cheap API calls and are polled every 2 seconds.

use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GET_ADAPTERS_ADDRESSES_FLAGS, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

#[derive(Clone, Copy, Default)]
pub struct Memory {
    pub used_gb: f64,
    pub total_gb: f64,
}

pub fn memory() -> Memory {
    let mut m = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    if unsafe { GlobalMemoryStatusEx(&mut m) }.is_err() {
        return Memory::default();
    }
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let total = m.ullTotalPhys as f64;
    let used = (m.ullTotalPhys - m.ullAvailPhys) as f64;
    Memory {
        used_gb: used / GB,
        total_gb: total / GB,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Wifi,
    Ethernet,
    Other,
    Offline,
}

const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
const IF_TYPE_IEEE80211: u32 = 71;
const IF_OPER_STATUS_UP: i32 = 1;
const ERROR_BUFFER_OVERFLOW: u32 = 111;
// GAA_FLAG_SKIP_ANYCAST | SKIP_MULTICAST | SKIP_DNS_SERVER | INCLUDE_GATEWAYS
const GAA_FLAGS: u32 = 0x0002 | 0x0004 | 0x0008 | 0x0080;

/// How the PC is connected to the internet: an adapter that is up and has a
/// default gateway. Wi-Fi/Ethernet win over virtual adapters (VPNs, Hyper-V).
pub fn network() -> Network {
    let mut size: u32 = 16 * 1024;
    for _ in 0..3 {
        // u64 storage keeps the buffer 8-byte aligned for the struct.
        let mut buf: Vec<u64> = vec![0; (size as usize / 8) + 1];
        let ptr = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        let rc = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC.0 as u32,
                GET_ADAPTERS_ADDRESSES_FLAGS(GAA_FLAGS),
                None,
                Some(ptr),
                &mut size,
            )
        };
        if rc == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if rc != 0 {
            return Network::Offline;
        }
        let mut best = Network::Offline;
        let mut cur = ptr as *const IP_ADAPTER_ADDRESSES_LH;
        while !cur.is_null() {
            let a = unsafe { &*cur };
            if a.OperStatus.0 == IF_OPER_STATUS_UP && !a.FirstGatewayAddress.is_null() {
                match a.IfType {
                    IF_TYPE_ETHERNET_CSMACD => return Network::Ethernet,
                    IF_TYPE_IEEE80211 => best = Network::Wifi,
                    _ => {
                        if best == Network::Offline {
                            best = Network::Other;
                        }
                    }
                }
            }
            cur = a.Next;
        }
        return best;
    }
    Network::Offline
}
