//! macOS input via Quartz event services. Posting events requires the Accessibility
//! permission (System Settings → Privacy & Security → Accessibility).
#![allow(unsafe_code)] // AXIsProcessTrustedWithOptions FFI; see SAFETY note.

use anyhow::{Result, anyhow};
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::CFString;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField, ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::{Backend, PermissionDenied, keymap};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
}

/// Whether this process may post input events. With `prompt`, macOS shows its
/// "allow accessibility" dialog the first time.
pub fn accessibility_trusted(prompt: bool) -> bool {
    let key = CFString::from_static_string("AXTrustedCheckOptionPrompt");
    let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::from(prompt).as_CFType())]);
    // SAFETY: `options` is a valid CFDictionary that outlives the call.
    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) }
}

pub struct MacBackend {
    source: CGEventSource,
    flags: CGEventFlags,
}

// CGEventSource is a reference-counted CF object; we only use it from one thread at a time.
unsafe impl Send for MacBackend {}

impl MacBackend {
    pub fn new() -> Result<Self> {
        if !accessibility_trusted(true) {
            return Err(PermissionDenied.into());
        }
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|()| anyhow!("create CGEventSource"))?;
        Ok(Self { source, flags: CGEventFlags::CGEventFlagNull })
    }

    fn post(&self, event: CGEvent) {
        event.set_flags(self.flags);
        event.post(CGEventTapLocation::HID);
    }
}

fn modifier_flag(code: &str) -> Option<CGEventFlags> {
    Some(match code {
        "ShiftLeft" | "ShiftRight" => CGEventFlags::CGEventFlagShift,
        "ControlLeft" | "ControlRight" => CGEventFlags::CGEventFlagControl,
        "AltLeft" | "AltRight" => CGEventFlags::CGEventFlagAlternate,
        "MetaLeft" | "MetaRight" => CGEventFlags::CGEventFlagCommand,
        _ => return None,
    })
}

impl Backend for MacBackend {
    fn move_to(&mut self, x: f64, y: f64, held: u8) -> Result<()> {
        let (kind, button) = if held & 1 != 0 {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if held & 4 != 0 {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if held != 0 {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };
        let ev = CGEvent::new_mouse_event(self.source.clone(), kind, CGPoint::new(x, y), button)
            .map_err(|()| anyhow!("create mouse move"))?;
        self.post(ev);
        Ok(())
    }

    fn button(&mut self, button: u8, down: bool, x: f64, y: f64, clicks: u32) -> Result<()> {
        let (kind, cg_button) = match (button, down) {
            (0, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (0, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (2, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (2, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (_, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (_, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };
        let ev = CGEvent::new_mouse_event(self.source.clone(), kind, CGPoint::new(x, y), cg_button)
            .map_err(|()| anyhow!("create mouse button"))?;
        ev.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i64::from(clicks));
        if !matches!(button, 0 | 2) {
            // DOM: 1 middle, 3 back, 4 forward. Quartz: 2 middle, 3 back, 4 forward.
            let number = if button == 1 { 2 } else { i64::from(button) };
            ev.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, number);
        }
        self.post(ev);
        Ok(())
    }

    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        // Quartz: positive wheel values scroll up/left; DOM: positive deltas scroll down/right.
        let ev = CGEvent::new_scroll_event(self.source.clone(), ScrollEventUnit::PIXEL, 2, -dy, -dx, 0)
            .map_err(|()| anyhow!("create scroll"))?;
        self.post(ev);
        Ok(())
    }

    fn key(&mut self, code: &str, down: bool) -> Result<bool> {
        let Some(keycode) = keymap::mac_keycode(code) else {
            return Ok(false);
        };
        if let Some(flag) = modifier_flag(code) {
            if down {
                self.flags.insert(flag);
            } else {
                self.flags.remove(flag);
            }
        } else if code == "CapsLock" && down {
            self.flags.toggle(CGEventFlags::CGEventFlagAlphaShift);
        }
        let ev = CGEvent::new_keyboard_event(self.source.clone(), keycode, down)
            .map_err(|()| anyhow!("create key event"))?;
        self.post(ev);
        Ok(true)
    }
}
