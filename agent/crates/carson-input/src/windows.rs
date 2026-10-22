//! Windows input via SendInput, using scancodes so keys are layout-independent.
//! Reaching the secure desktop (UAC prompts, the lock screen) additionally needs SYSTEM
//! privilege, which only `carson-input-svc` (an installed Windows service) has; a normal
//! user process can open and switch to every other desktop but not that one.
#![allow(unsafe_code)] // Win32 FFI; each call passes fully initialized, stack-owned structs.

use anyhow::{Result, bail};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, DESKTOP_CREATEMENU,
    DESKTOP_CREATEWINDOW, DESKTOP_ENUMERATE, DESKTOP_HOOKCONTROL, DESKTOP_JOURNALPLAYBACK,
    DESKTOP_JOURNALRECORD, DESKTOP_READOBJECTS, DESKTOP_SWITCHDESKTOP, DESKTOP_WRITEOBJECTS,
    OpenInputDesktop, SetThreadDesktop,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::{Backend, keymap};

/// Moves the calling thread onto whatever desktop currently receives input (the ordinary
/// desktop, or Winlogon's secure desktop during a UAC prompt / the lock screen) before
/// injecting. A normal user process can already do this for the ordinary desktop; only
/// SYSTEM can open the secure one, so this is a no-op failure (input still goes to the
/// thread's last desktop) everywhere except inside `carson-input-svc`.
fn switch_to_input_desktop() {
    const ACCESS: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(
        DESKTOP_CREATEMENU.0
            | DESKTOP_CREATEWINDOW.0
            | DESKTOP_ENUMERATE.0
            | DESKTOP_HOOKCONTROL.0
            | DESKTOP_JOURNALPLAYBACK.0
            | DESKTOP_JOURNALRECORD.0
            | DESKTOP_READOBJECTS.0
            | DESKTOP_SWITCHDESKTOP.0
            | DESKTOP_WRITEOBJECTS.0,
    );
    // SAFETY: OpenInputDesktop/SetThreadDesktop/CloseDesktop take no pointers we own; `desk`
    // is a valid handle from OpenInputDesktop, used only while non-null and closed once.
    unsafe {
        let Ok(desk) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, ACCESS) else {
            return; // not privileged enough to see the current input desktop; carry on as before
        };
        let _ = SetThreadDesktop(desk);
        let _ = CloseDesktop(desk);
    }
}

const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;
/// Chrome on Windows reports ~100 px per wheel notch; Windows apps expect 120 units per notch.
const WHEEL_UNITS_PER_PIXEL: f64 = 1.2;

pub struct WindowsBackend {
    wheel_rem: (f64, f64),
}

/// Makes coordinates physical pixels on every monitor. Call once, before capturing.
pub fn enable_dpi_awareness() {
    // SAFETY: plain Win32 call; failure (already set) is harmless.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

impl WindowsBackend {
    pub fn new() -> Result<Self> {
        enable_dpi_awareness();
        Ok(Self {
            wheel_rem: (0.0, 0.0),
        })
    }

    fn send(inputs: &[INPUT]) -> Result<()> {
        switch_to_input_desktop();
        // SAFETY: `inputs` is a valid slice of initialized INPUT structs.
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            bail!(
                "SendInput injected {sent}/{} events (blocked by UIPI or the secure desktop?)",
                inputs.len()
            );
        }
        Ok(())
    }

    fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
}

/// Converts virtual-desktop pixels to SendInput's 0..65535 absolute space.
fn to_absolute(x: f64, y: f64) -> (i32, i32) {
    // SAFETY: GetSystemMetrics has no preconditions.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    let ax = ((x - f64::from(vx)) * 65535.0 / f64::from(vw - 1)).round() as i32;
    let ay = ((y - f64::from(vy)) * 65535.0 / f64::from(vh - 1)).round() as i32;
    (ax.clamp(0, 65535), ay.clamp(0, 65535))
}

impl Backend for WindowsBackend {
    fn move_to(&mut self, x: f64, y: f64, _held: u8) -> Result<()> {
        let (ax, ay) = to_absolute(x, y);
        Self::send(&[Self::mouse(
            ax,
            ay,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )])
    }

    fn button(&mut self, button: u8, down: bool, x: f64, y: f64, _clicks: u32) -> Result<()> {
        let (flags, data) = match (button, down) {
            (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (0, false) => (MOUSEEVENTF_LEFTUP, 0),
            (1, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (1, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (2, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (2, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
            (3, false) => (MOUSEEVENTF_XUP, XBUTTON1),
            (_, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
            (_, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        };
        // Windows detects double clicks itself from timing and position.
        let (ax, ay) = to_absolute(x, y);
        Self::send(&[
            Self::mouse(
                ax,
                ay,
                0,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            ),
            Self::mouse(0, 0, data, flags),
        ])
    }

    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        // Accumulate so slow trackpad scrolls are not rounded away.
        self.wheel_rem.0 += f64::from(dx) * WHEEL_UNITS_PER_PIXEL;
        self.wheel_rem.1 += -f64::from(dy) * WHEEL_UNITS_PER_PIXEL;
        let (h, v) = (self.wheel_rem.0.trunc(), self.wheel_rem.1.trunc());
        self.wheel_rem.0 -= h;
        self.wheel_rem.1 -= v;
        let mut inputs = Vec::new();
        if v != 0.0 {
            inputs.push(Self::mouse(0, 0, v as i32 as u32, MOUSEEVENTF_WHEEL));
        }
        if h != 0.0 {
            inputs.push(Self::mouse(0, 0, h as i32 as u32, MOUSEEVENTF_HWHEEL));
        }
        if inputs.is_empty() {
            Ok(())
        } else {
            Self::send(&inputs)
        }
    }

    fn key(&mut self, code: &str, down: bool) -> Result<bool> {
        let Some((scan, extended)) = keymap::windows_scancode(code) else {
            return Ok(false);
        };
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: scan,
                    dwFlags: KEYBD_EVENT_FLAGS(flags.0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        Self::send(&[input])?;
        Ok(true)
    }
}
