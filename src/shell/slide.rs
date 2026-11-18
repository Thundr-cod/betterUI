// Sliding the bar or the dock in and out of view (their auto-hide modes).
//
// The window polls where the mouse is and tells the slide whether it should
// be visible. Hiding waits a few polls, so moving the mouse briefly away
// doesn't make it flicker; showing is immediate. The slide then eases the
// window's offset towards its target on a ~60 fps timer.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer, ShowWindow, SW_HIDE, SW_SHOWNA};

/// Polls (100 ms each) to wait before hiding.
const HIDE_DELAY_POLLS: u32 = 4;
const FRAME_MS: u32 = 15;

pub struct Slide {
    hwnd: HWND,
    timer: usize,
    /// 0 when shown; the window's offset from its shown position otherwise.
    pub offset: f32,
    hidden: bool,
    waiting: u32,
    animating: bool,
}

impl Slide {
    /// `timer` is the window's timer id for animation frames: call `step`
    /// when it fires.
    pub fn new(hwnd: HWND, timer: usize) -> Self {
        Self { hwnd, timer, offset: 0.0, hidden: false, waiting: 0, animating: false }
    }

    /// Is any of the window on screen? `hidden_offset` is its offset when
    /// fully hidden.
    pub fn shown(&self, hidden_offset: f32) -> bool {
        (self.offset - hidden_offset).abs() > 1.0
    }

    /// The result of a poll: should the window be visible now?
    pub fn want_visible(&mut self, visible: bool) {
        if visible {
            self.waiting = 0;
            self.set_hidden(false);
        } else {
            self.waiting += 1;
            if self.waiting >= HIDE_DELAY_POLLS {
                self.set_hidden(true);
            }
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        if hidden == self.hidden {
            return;
        }
        self.hidden = hidden;
        unsafe {
            if !hidden {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            }
            if !self.animating {
                self.animating = true;
                SetTimer(self.hwnd, self.timer, FRAME_MS, None);
            }
        }
    }

    /// One animation frame. The caller then moves the window by `offset`.
    pub fn step(&mut self, hidden_offset: f32) {
        let target = if self.hidden { hidden_offset } else { 0.0 };
        let diff = target - self.offset;
        if diff.abs() < 1.0 {
            self.offset = target;
            self.animating = false;
            unsafe {
                let _ = KillTimer(self.hwnd, self.timer);
                if self.hidden {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
        } else {
            self.offset += diff * 0.35;
        }
    }

    /// Go straight to hidden or shown, without animating.
    pub fn jump(&mut self, hidden: bool, hidden_offset: f32) {
        self.hidden = hidden;
        self.waiting = 0;
        self.offset = if hidden { hidden_offset } else { 0.0 };
    }
}
