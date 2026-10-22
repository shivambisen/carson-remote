//! IPC with `carson-input-svc`, the SYSTEM-level Windows service that can inject input on
//! the secure desktop (UAC prompts, the lock screen) where a normal process cannot.
//!
//! The wire format mirrors [`Backend`] directly (already-resolved global coordinates, same
//! as what [`crate::Injector`] passes to a local `Backend`) rather than the browser-facing,
//! normalized [`carson_proto::input::InputMessage`] — the service has no display rect to
//! turn normalized coordinates back into pixels, so it must receive pixels already.
//!
//! Framing: a u32-LE byte length, then that many bytes of JSON.
#![allow(unsafe_code)] // only the pipe path string crosses the FFI boundary, via std::fs

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Local-only; never exposed over the network (Windows only shares named pipes remotely
/// when the Server service explicitly lists them, which glenley never does).
pub const PIPE_PATH: &str = r"\\.\pipe\glenley-input";

/// One [`Backend`] call, serialized for `carson-input-svc`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BackendCall {
    MoveTo {
        x: f64,
        y: f64,
        held: u8,
    },
    Button {
        button: u8,
        down: bool,
        x: f64,
        y: f64,
        clicks: u32,
    },
    Scroll {
        dx: i32,
        dy: i32,
    },
    Key {
        code: String,
        down: bool,
    },
}

/// Largest single frame accepted: generous for an input event, small enough to bound a
/// misbehaving or hostile writer.
const MAX_FRAME_BYTES: u32 = 4 * 1024;

pub fn write_call<W: Write>(w: &mut W, call: &BackendCall) -> Result<()> {
    let body = serde_json::to_vec(call).context("encode BackendCall")?;
    w.write_all(&(body.len() as u32).to_le_bytes())
        .context("write pipe frame length")?;
    w.write_all(&body).context("write pipe frame body")?;
    w.flush().context("flush pipe")
}

pub fn read_call<R: Read>(r: &mut R) -> Result<BackendCall> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)
        .context("read pipe frame length")?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        bail!("pipe frame too large ({len} bytes)");
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).context("read pipe frame body")?;
    serde_json::from_slice(&body).context("decode BackendCall")
}

#[cfg(target_os = "windows")]
pub use windows_client::PipeBackend;

#[cfg(target_os = "windows")]
mod windows_client {
    use std::fs::OpenOptions;

    use anyhow::{Context, Result};

    use crate::Backend;

    use super::{BackendCall, PIPE_PATH, write_call};

    /// Connects to `carson-input-svc`'s pipe; every [`Backend`] call is forwarded to it
    /// instead of injecting directly, so the service (running as SYSTEM) can reach the
    /// secure desktop that this process cannot.
    pub struct PipeBackend {
        pipe: std::fs::File,
    }

    impl PipeBackend {
        /// Fails immediately if the service is not installed or not running — callers fall
        /// back to injecting directly in that case.
        pub fn connect() -> Result<Self> {
            let pipe = OpenOptions::new()
                .read(true)
                .write(true)
                .open(PIPE_PATH)
                .context("connect to carson-input-svc pipe")?;
            Ok(Self { pipe })
        }

        fn send(&mut self, call: BackendCall) -> Result<()> {
            write_call(&mut self.pipe, &call)
        }
    }

    impl Backend for PipeBackend {
        fn move_to(&mut self, x: f64, y: f64, held: u8) -> Result<()> {
            self.send(BackendCall::MoveTo { x, y, held })
        }
        fn button(&mut self, button: u8, down: bool, x: f64, y: f64, clicks: u32) -> Result<()> {
            self.send(BackendCall::Button {
                button,
                down,
                x,
                y,
                clicks,
            })
        }
        fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
            self.send(BackendCall::Scroll { dx, dy })
        }
        fn key(&mut self, code: &str, down: bool) -> Result<bool> {
            self.send(BackendCall::Key {
                code: code.to_owned(),
                down,
            })?;
            Ok(true) // the service maps the code; an unmappable key is simply a no-op there
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        for call in [
            BackendCall::MoveTo {
                x: 12.5,
                y: -3.0,
                held: 1,
            },
            BackendCall::Button {
                button: 2,
                down: true,
                x: 0.0,
                y: 0.0,
                clicks: 2,
            },
            BackendCall::Scroll { dx: -5, dy: 120 },
            BackendCall::Key {
                code: "KeyA".into(),
                down: false,
            },
        ] {
            let mut buf = Vec::new();
            write_call(&mut buf, &call).unwrap();
            let mut cursor = std::io::Cursor::new(buf);
            assert_eq!(read_call(&mut cursor).unwrap(), call);
        }
    }

    #[test]
    fn an_oversized_length_prefix_is_rejected_before_allocating() {
        let mut buf = (MAX_FRAME_BYTES + 1).to_le_bytes().to_vec();
        buf.extend_from_slice(b"padding-that-is-never-read");
        let mut cursor = std::io::Cursor::new(buf);
        assert!(read_call(&mut cursor).is_err());
    }
}
