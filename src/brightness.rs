// Screen brightness for external monitors over DDC/CI (the same signal the
// monitor's own buttons use). Each call talks to the monitor and can take
// ~50 ms, so reads happen on background threads and writes go through one
// worker thread that always applies only the newest value. Dragging the
// slider therefore never queues up a backlog of slow monitor commands.
//
// Laptop built-in screens don't support DDC/CI; there the slider is hidden.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTOPRIMARY};

static DEMO_VALUE: Mutex<f32> = Mutex::new(0.7);

/// The physical monitors behind the display at `area` (screen coordinates).
fn with_monitors<T>(area: RECT, f: impl FnOnce(&[PHYSICAL_MONITOR]) -> T) -> Option<T> {
    unsafe {
        let hmon = MonitorFromRect(&area, MONITOR_DEFAULTTOPRIMARY);
        let mut count = 0u32;
        GetNumberOfPhysicalMonitorsFromHMONITOR(hmon, &mut count).ok()?;
        if count == 0 {
            return None;
        }
        let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
        GetPhysicalMonitorsFromHMONITOR(hmon, &mut monitors).ok()?;
        let result = f(&monitors);
        let _ = DestroyPhysicalMonitors(&monitors);
        Some(result)
    }
}

/// Current brightness 0..1 of the monitor at `area`, or None if it doesn't
/// support DDC/CI.
pub fn read(area: RECT) -> Option<f32> {
    if crate::audio::demo() {
        return DEMO_VALUE.lock().ok().map(|v| *v);
    }
    with_monitors(area, |monitors| {
        for m in monitors {
            let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
            let ok = unsafe { GetMonitorBrightness(m.hPhysicalMonitor, &mut min, &mut cur, &mut max) };
            if ok != 0 && max > min {
                return Some((cur.saturating_sub(min)) as f32 / (max - min) as f32);
            }
        }
        None
    })
    .flatten()
}

fn write(area: RECT, v: f32) {
    if crate::audio::demo() {
        if let Ok(mut d) = DEMO_VALUE.lock() {
            *d = v;
        }
        return;
    }
    with_monitors(area, |monitors| {
        for m in monitors {
            let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
            unsafe {
                if GetMonitorBrightness(m.hPhysicalMonitor, &mut min, &mut cur, &mut max) != 0
                    && max > min
                {
                    let value = min + ((max - min) as f32 * v.clamp(0.0, 1.0)).round() as u32;
                    if SetMonitorBrightness(m.hPhysicalMonitor, value) == 0 {
                        crate::log::write("SetMonitorBrightness failed");
                    }
                }
            }
        }
    });
}

static WORKER: OnceLock<Mutex<Sender<(RECT, f32)>>> = OnceLock::new();

/// Set the brightness of the monitor at `area` without blocking. Only the
/// latest value is applied.
pub fn set(area: RECT, v: f32) {
    let sender = WORKER.get_or_init(|| {
        let (tx, rx) = channel::<(RECT, f32)>();
        std::thread::spawn(move || {
            while let Ok(mut job) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    job = newer;
                }
                write(job.0, job.1);
            }
        });
        Mutex::new(tx)
    });
    if let Ok(tx) = sender.lock() {
        let _ = tx.send((area, v));
    }
}
