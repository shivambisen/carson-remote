//! Input injection for carson-agent.
//!
//! [`Injector`] turns decoded [`InputMessage`]s into OS input events through a platform
//! [`Backend`]. It owns the safety-critical state: which keys and buttons are held (so a
//! disconnect never leaves anything stuck), where the pointer is, and click counting.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use anyhow::Result;
use carson_proto::input::InputMessage;

pub mod keymap;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Windows: makes every coordinate API (monitor bounds, capture size, SendInput) use physical
/// pixels. Must run at process start: monitor bounds measured before it are in scaled
/// (logical) pixels on 125%/150% displays, which put the remote cursor in the wrong place.
/// No-op elsewhere.
/// macOS: whether Accessibility (needed for remote control) is allowed; `prompt` shows the
/// system dialog the first time. Always true elsewhere.
pub fn accessibility_allowed(prompt: bool) -> bool {
    #[cfg(target_os = "macos")]
    return macos::accessibility_trusted(prompt);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt;
        true
    }
}

pub fn enable_dpi_awareness() {
    #[cfg(target_os = "windows")]
    windows::enable_dpi_awareness();
}

/// The streamed display's rectangle in the OS's global input coordinate space
/// (macOS: points, origin top-left of the main display; Windows: virtual-desktop pixels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Low-level OS input. Coordinates are global; scroll deltas are pixels (DOM sign convention).
pub trait Backend: Send {
    /// Moves the pointer. `held` is the bitmask of pressed buttons (1 << button), for drags.
    fn move_to(&mut self, x: f64, y: f64, held: u8) -> Result<()>;
    fn button(&mut self, button: u8, down: bool, x: f64, y: f64, click_count: u32) -> Result<()>;
    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()>;
    /// Presses or releases a key by DOM code. Returns false if the key is not mappable.
    fn key(&mut self, code: &str, down: bool) -> Result<bool>;
}

/// Input injection is not allowed (macOS: Accessibility permission).
#[derive(Debug)]
pub struct PermissionDenied;

impl std::fmt::Display for PermissionDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "input control needs Accessibility permission (System Settings → Privacy & Security → Accessibility)",
        )
    }
}

impl std::error::Error for PermissionDenied {}

/// The platform's native backend.
pub fn native_backend() -> Result<Box<dyn Backend>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::MacBackend::new()?))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(windows::WindowsBackend::new()?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        anyhow::bail!("input injection is not implemented on this OS yet")
    }
}

const MAX_BUTTON: u8 = 4;

/// Modifier bits and their physical keys (left key is used when synthesizing).
const MODIFIERS: [(u16, [&str; 2]); 4] = [
    (
        carson_proto::input::mods::SHIFT,
        ["ShiftLeft", "ShiftRight"],
    ),
    (
        carson_proto::input::mods::CTRL,
        ["ControlLeft", "ControlRight"],
    ),
    (carson_proto::input::mods::ALT, ["AltLeft", "AltRight"]),
    (carson_proto::input::mods::META, ["MetaLeft", "MetaRight"]),
];
const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_DISTANCE: f64 = 5.0;

pub struct Injector {
    backend: Box<dyn Backend>,
    display: Rect,
    pos: (f64, f64),
    keys: BTreeSet<String>,
    buttons: u8,
    last_down: Option<(u8, Instant, (f64, f64), u32)>,
}

impl Injector {
    pub fn new(backend: Box<dyn Backend>, display: Rect) -> Self {
        let pos = (display.x + display.w / 2.0, display.y + display.h / 2.0);
        Self {
            backend,
            display,
            pos,
            keys: BTreeSet::new(),
            buttons: 0,
            last_down: None,
        }
    }

    /// Switches the target display (e.g. the viewer picked another monitor).
    pub fn set_display(&mut self, display: Rect) {
        self.release_all();
        self.display = display;
    }

    /// Maps normalized 0..=65535 coordinates onto the display.
    pub fn map(&self, x: u16, y: u16) -> (f64, f64) {
        let fx = f64::from(x) / 65535.0;
        let fy = f64::from(y) / 65535.0;
        (
            self.display.x + fx * (self.display.w - 1.0).max(0.0),
            self.display.y + fy * (self.display.h - 1.0).max(0.0),
        )
    }

    pub fn handle(&mut self, msg: &InputMessage) -> Result<()> {
        match msg {
            InputMessage::PointerMove { x, y } => {
                self.pos = self.map(*x, *y);
                self.backend.move_to(self.pos.0, self.pos.1, self.buttons)
            }
            InputMessage::Button { button, down } => {
                if *button > MAX_BUTTON {
                    return Ok(());
                }
                let bit = 1u8 << button;
                if *down == (self.buttons & bit != 0) {
                    return Ok(()); // duplicate press/release: keep state consistent
                }
                let clicks = if *down {
                    self.click_count(*button)
                } else {
                    self.last_down.map_or(1, |l| l.3)
                };
                if *down {
                    self.buttons |= bit;
                } else {
                    self.buttons &= !bit;
                }
                self.backend
                    .button(*button, *down, self.pos.0, self.pos.1, clicks)
            }
            InputMessage::Wheel { dx, dy } => self.backend.scroll(i32::from(*dx), i32::from(*dy)),
            InputMessage::Key { down, mods, code } => {
                if *down {
                    self.sync_modifiers(*mods, code)?;
                    // Repeated keydowns (auto-repeat) are forwarded: injected keys do not repeat by themselves.
                    self.keys.insert(code.clone());
                } else if !self.keys.remove(code) {
                    return Ok(()); // never release what we did not press
                }
                self.backend.key(code, *down).map(|_| ())
            }
            InputMessage::ReleaseAll => {
                self.release_all();
                Ok(())
            }
            InputMessage::ClipboardChunk { .. } => Ok(()), // handled by the agent's clipboard sync
        }
    }

    /// Makes held modifiers match the event's modifier bits before a non-modifier key.
    /// Covers modifiers pressed before the viewer had focus (their keydown never arrived) or
    /// released while it did not (their keyup never arrived).
    fn sync_modifiers(&mut self, mods: u16, code: &str) -> Result<()> {
        if MODIFIERS.iter().any(|(_, keys)| keys.contains(&code)) {
            return Ok(());
        }
        for (bit, keys) in MODIFIERS {
            let held = keys.iter().any(|k| self.keys.contains(*k));
            let wanted = mods & bit != 0;
            if wanted && !held {
                self.keys.insert(keys[0].to_owned());
                self.backend.key(keys[0], true)?;
            } else if !wanted && held {
                for k in keys {
                    if self.keys.remove(k) {
                        self.backend.key(k, false)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn click_count(&mut self, button: u8) -> u32 {
        let now = Instant::now();
        let count = match self.last_down {
            Some((b, t, (px, py), n))
                if b == button
                    && now.duration_since(t) <= DOUBLE_CLICK_TIME
                    && (px - self.pos.0).hypot(py - self.pos.1) <= DOUBLE_CLICK_DISTANCE =>
            {
                n + 1
            }
            _ => 1,
        };
        self.last_down = Some((button, now, self.pos, count));
        count
    }

    /// Releases every held key and button. Called on disconnect, focus loss, and drop.
    pub fn release_all(&mut self) {
        for code in std::mem::take(&mut self.keys) {
            let _ = self.backend.key(&code, false);
        }
        for b in 0..=MAX_BUTTON {
            if self.buttons & (1 << b) != 0 {
                let _ = self.backend.button(b, false, self.pos.0, self.pos.1, 1);
            }
        }
        self.buttons = 0;
    }

    /// Keys currently held (diagnostics/tests).
    pub fn held_keys(&self) -> Vec<String> {
        self.keys.iter().cloned().collect()
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// Records calls instead of injecting (tests, `--input log`).
#[derive(Default)]
pub struct RecordingBackend {
    pub events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Backend for RecordingBackend {
    fn move_to(&mut self, x: f64, y: f64, held: u8) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("move {x:.0},{y:.0} held={held}"));
        Ok(())
    }
    fn button(&mut self, button: u8, down: bool, x: f64, y: f64, clicks: u32) -> Result<()> {
        self.events.lock().unwrap().push(format!(
            "button {button} {} at {x:.0},{y:.0} x{clicks}",
            if down { "down" } else { "up" }
        ));
        Ok(())
    }
    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("scroll {dx},{dy}"));
        Ok(())
    }
    fn key(&mut self, code: &str, down: bool) -> Result<bool> {
        self.events
            .lock()
            .unwrap()
            .push(format!("key {code} {}", if down { "down" } else { "up" }));
        Ok(keymap::mac_keycode(code).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn injector(display: Rect) -> (Injector, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let rec = RecordingBackend::default();
        let events = std::sync::Arc::clone(&rec.events);
        (Injector::new(Box::new(rec), display), events)
    }

    const SECOND_MONITOR: Rect = Rect {
        x: 1440.0,
        y: -200.0,
        w: 1920.0,
        h: 1080.0,
    };

    #[test]
    fn maps_normalized_coordinates_onto_the_display() {
        let (inj, _) = injector(SECOND_MONITOR);
        assert_eq!(inj.map(0, 0), (1440.0, -200.0));
        assert_eq!(inj.map(65535, 65535), (1440.0 + 1919.0, -200.0 + 1079.0));
        let (cx, cy) = inj.map(32768, 32768);
        assert!((cx - 2399.5).abs() < 0.1 && (cy - 339.5).abs() < 0.1);
    }

    #[test]
    fn drag_reports_held_buttons_and_clicks_are_counted() {
        let (mut inj, ev) = injector(SECOND_MONITOR);
        let press = |inj: &mut Injector, down| {
            inj.handle(&InputMessage::Button { button: 0, down })
                .unwrap()
        };
        inj.handle(&InputMessage::PointerMove { x: 0, y: 0 })
            .unwrap();
        press(&mut inj, true);
        inj.handle(&InputMessage::PointerMove { x: 100, y: 0 })
            .unwrap();
        press(&mut inj, false);
        // Double click at the same spot.
        press(&mut inj, true);
        press(&mut inj, false);
        press(&mut inj, true);
        press(&mut inj, false);
        let ev = ev.lock().unwrap();
        assert_eq!(
            ev[2], "move 1443,-200 held=1",
            "move while pressed is a drag"
        );
        assert_eq!(ev[3], "button 0 up at 1443,-200 x1");
        assert_eq!(ev[4], "button 0 down at 1443,-200 x2");
        assert_eq!(
            ev[6], "button 0 down at 1443,-200 x3",
            "third click is a triple click"
        );
    }

    #[test]
    fn release_all_frees_everything_once() {
        let (mut inj, ev) = injector(SECOND_MONITOR);
        for code in ["ShiftLeft", "KeyA"] {
            inj.handle(&InputMessage::Key {
                down: true,
                mods: carson_proto::input::mods::SHIFT,
                code: code.into(),
            })
            .unwrap();
        }
        inj.handle(&InputMessage::Button {
            button: 2,
            down: true,
        })
        .unwrap();
        inj.handle(&InputMessage::ReleaseAll).unwrap();
        assert!(inj.held_keys().is_empty());
        let tail: Vec<_> = ev.lock().unwrap()[3..].to_vec();
        assert_eq!(
            tail,
            [
                "key KeyA up",
                "key ShiftLeft up",
                "button 2 up at 2400,340 x1"
            ]
        );
        inj.handle(&InputMessage::ReleaseAll).unwrap();
        assert_eq!(ev.lock().unwrap().len(), 6, "second release_all is a no-op");
    }

    #[test]
    fn modifiers_follow_the_event_bits() {
        let (mut inj, ev) = injector(SECOND_MONITOR);
        let key = |inj: &mut Injector, down, mods, code: &str| {
            inj.handle(&InputMessage::Key {
                down,
                mods,
                code: code.into(),
            })
            .unwrap()
        };
        // Shift held before focus: only KeyA arrives, but with the shift bit.
        key(&mut inj, true, carson_proto::input::mods::SHIFT, "KeyA");
        key(&mut inj, false, carson_proto::input::mods::SHIFT, "KeyA");
        // Shift released while unfocused: next key has no shift bit.
        key(&mut inj, true, 0, "KeyB");
        // Real modifier keys are passed through untouched.
        key(&mut inj, true, carson_proto::input::mods::META, "MetaLeft");
        assert_eq!(
            *ev.lock().unwrap(),
            [
                "key ShiftLeft down",
                "key KeyA down",
                "key KeyA up",
                "key ShiftLeft up",
                "key KeyB down",
                "key MetaLeft down"
            ]
        );
    }

    #[test]
    fn dropping_the_injector_releases_held_keys() {
        let (mut inj, ev) = injector(SECOND_MONITOR);
        inj.handle(&InputMessage::Key {
            down: true,
            mods: 8,
            code: "MetaLeft".into(),
        })
        .unwrap();
        drop(inj);
        assert_eq!(ev.lock().unwrap().last().unwrap(), "key MetaLeft up");
    }

    #[test]
    fn ignores_spurious_releases_and_bad_buttons() {
        let (mut inj, ev) = injector(SECOND_MONITOR);
        inj.handle(&InputMessage::Key {
            down: false,
            mods: 0,
            code: "KeyQ".into(),
        })
        .unwrap();
        inj.handle(&InputMessage::Button {
            button: 0,
            down: false,
        })
        .unwrap();
        inj.handle(&InputMessage::Button {
            button: 9,
            down: true,
        })
        .unwrap();
        assert!(ev.lock().unwrap().is_empty());
    }

    #[test]
    fn switching_display_releases_input() {
        let (mut inj, ev) = injector(SECOND_MONITOR);
        inj.handle(&InputMessage::Key {
            down: true,
            mods: 0,
            code: "KeyW".into(),
        })
        .unwrap();
        inj.set_display(Rect {
            x: 0.0,
            y: 0.0,
            w: 1440.0,
            h: 900.0,
        });
        assert_eq!(ev.lock().unwrap().last().unwrap(), "key KeyW up");
        assert_eq!(inj.map(0, 0), (0.0, 0.0));
    }
}
