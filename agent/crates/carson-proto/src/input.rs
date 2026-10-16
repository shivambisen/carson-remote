//! Binary input protocol carried on the `ptr` (unordered, no retransmits) and `ctl`
//! (reliable) data channels. Byte layouts are pinned by `proto/input-vectors.json`.

use serde::{Deserialize, Serialize};

/// Label of the unreliable, unordered channel (pointer moves only: newest wins).
pub const POINTER_CHANNEL: &str = "ptr";
/// Label of the reliable, ordered channel (everything else, plus JSON control text).
pub const CONTROL_CHANNEL: &str = "ctl";

/// Largest clipboard text accepted (reassembled), in bytes.
pub const MAX_CLIPBOARD_BYTES: usize = 1 << 20;
/// Clipboard chunk payload size; well under every browser's SCTP message limit.
pub const CLIPBOARD_CHUNK_BYTES: usize = 16 * 1024;
/// Longest accepted `KeyboardEvent.code` string.
pub const MAX_KEY_CODE_LEN: usize = 32;

pub mod mods {
    pub const SHIFT: u16 = 1;
    pub const CTRL: u16 = 2;
    pub const ALT: u16 = 4;
    pub const META: u16 = 8;
    pub const CAPS_LOCK: u16 = 16;
}

/// Mouse buttons, numbered like DOM `MouseEvent.button`.
pub mod button {
    pub const LEFT: u8 = 0;
    pub const MIDDLE: u8 = 1;
    pub const RIGHT: u8 = 2;
    pub const BACK: u8 = 3;
    pub const FORWARD: u8 = 4;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputMessage {
    /// Absolute position, normalized 0..=65535 over the streamed display.
    PointerMove {
        x: u16,
        y: u16,
    },
    Button {
        button: u8,
        down: bool,
    },
    /// Pixel deltas, DOM sign convention (positive `dy` scrolls down).
    Wheel {
        dx: i16,
        dy: i16,
    },
    /// A physical key, as DOM `KeyboardEvent.code` (layout-independent).
    Key {
        down: bool,
        mods: u16,
        code: String,
    },
    /// One piece of clipboard text; `last` ends the transfer.
    ClipboardChunk {
        last: bool,
        data: String,
    },
    /// Release every pressed key and button (viewer lost focus, etc.).
    ReleaseAll,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    UnknownType(u8),
    Truncated,
    TrailingBytes,
    BadUtf8,
    TooLong,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid input message: {self:?}")
    }
}

impl std::error::Error for DecodeError {}

const POINTER_MOVE: u8 = 1;
const BUTTON: u8 = 2;
const WHEEL: u8 = 3;
const KEY: u8 = 4;
const CLIPBOARD_CHUNK: u8 = 5;
const RELEASE_ALL: u8 = 6;

impl InputMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(8);
        match self {
            Self::PointerMove { x, y } => {
                b.push(POINTER_MOVE);
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
            }
            Self::Button { button, down } => {
                b.extend_from_slice(&[BUTTON, *button, u8::from(*down)])
            }
            Self::Wheel { dx, dy } => {
                b.push(WHEEL);
                b.extend_from_slice(&dx.to_le_bytes());
                b.extend_from_slice(&dy.to_le_bytes());
            }
            Self::Key { down, mods, code } => {
                let code = &code.as_bytes()[..code.len().min(MAX_KEY_CODE_LEN)];
                b.extend_from_slice(&[KEY, u8::from(*down)]);
                b.extend_from_slice(&mods.to_le_bytes());
                b.push(code.len() as u8);
                b.extend_from_slice(code);
            }
            Self::ClipboardChunk { last, data } => {
                b.extend_from_slice(&[CLIPBOARD_CHUNK, u8::from(*last)]);
                b.extend_from_slice(data.as_bytes());
            }
            Self::ReleaseAll => b.push(RELEASE_ALL),
        }
        b
    }

    /// Decodes one message. Rejects truncated, oversized, or trailing input.
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let (&kind, rest) = buf.split_first().ok_or(DecodeError::Empty)?;
        let exact = |n: usize| match rest.len().cmp(&n) {
            std::cmp::Ordering::Less => Err(DecodeError::Truncated),
            std::cmp::Ordering::Greater => Err(DecodeError::TrailingBytes),
            std::cmp::Ordering::Equal => Ok(()),
        };
        let u16_at = |i: usize| u16::from_le_bytes([rest[i], rest[i + 1]]);
        match kind {
            POINTER_MOVE => {
                exact(4)?;
                Ok(Self::PointerMove {
                    x: u16_at(0),
                    y: u16_at(2),
                })
            }
            BUTTON => {
                exact(2)?;
                Ok(Self::Button {
                    button: rest[0],
                    down: rest[1] != 0,
                })
            }
            WHEEL => {
                exact(4)?;
                Ok(Self::Wheel {
                    dx: u16_at(0) as i16,
                    dy: u16_at(2) as i16,
                })
            }
            KEY => {
                if rest.len() < 4 {
                    return Err(DecodeError::Truncated);
                }
                let len = usize::from(rest[3]);
                if len > MAX_KEY_CODE_LEN {
                    return Err(DecodeError::TooLong);
                }
                exact(4 + len)?;
                let code = std::str::from_utf8(&rest[4..]).map_err(|_| DecodeError::BadUtf8)?;
                Ok(Self::Key {
                    down: rest[0] != 0,
                    mods: u16_at(1),
                    code: code.to_owned(),
                })
            }
            CLIPBOARD_CHUNK => {
                let (&flags, data) = rest.split_first().ok_or(DecodeError::Truncated)?;
                if data.len() > CLIPBOARD_CHUNK_BYTES {
                    return Err(DecodeError::TooLong);
                }
                let data = std::str::from_utf8(data).map_err(|_| DecodeError::BadUtf8)?;
                Ok(Self::ClipboardChunk {
                    last: flags & 1 != 0,
                    data: data.to_owned(),
                })
            }
            RELEASE_ALL => {
                exact(0)?;
                Ok(Self::ReleaseAll)
            }
            other => Err(DecodeError::UnknownType(other)),
        }
    }
}

/// Splits clipboard text into chunks on UTF-8 boundaries. Text over the limit is truncated.
pub fn clipboard_chunks(text: &str) -> Vec<InputMessage> {
    let mut text = &text[..floor_char_boundary(text, MAX_CLIPBOARD_BYTES)];
    let mut out = Vec::new();
    loop {
        let cut = floor_char_boundary(text, CLIPBOARD_CHUNK_BYTES);
        let (head, tail) = text.split_at(cut);
        out.push(InputMessage::ClipboardChunk {
            last: tail.is_empty(),
            data: head.to_owned(),
        });
        if tail.is_empty() {
            return out;
        }
        text = tail;
    }
}

fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Reassembles clipboard chunks, refusing transfers over [`MAX_CLIPBOARD_BYTES`].
#[derive(Default)]
pub struct ClipboardAssembler {
    buf: String,
    overflow: bool,
}

impl ClipboardAssembler {
    /// Feeds a chunk; returns the full text when the last chunk arrives.
    pub fn push(&mut self, last: bool, data: &str) -> Option<String> {
        if self.buf.len() + data.len() > MAX_CLIPBOARD_BYTES {
            self.overflow = true;
        } else if !self.overflow {
            self.buf.push_str(data);
        }
        if !last {
            return None;
        }
        let overflow = std::mem::take(&mut self.overflow);
        let text = std::mem::take(&mut self.buf);
        (!overflow).then_some(text)
    }
}

/// JSON control messages exchanged as text on the `ctl` channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Agent → viewer: available displays and which one is streamed.
    Displays {
        displays: Vec<DisplayDescriptor>,
        current: usize,
    },
    /// Viewer → agent: switch the streamed display.
    SelectDisplay {
        index: usize,
    },
    /// Agent → viewer: this host needs its unattended-access PIN before anything is shared.
    PinRequired {
        attempts_left: u32,
    },
    /// Viewer → agent: the PIN (only ever inside the encrypted data channel).
    Pin {
        pin: String,
    },
    /// Agent → viewer: result of a PIN attempt.
    PinResult {
        ok: bool,
        attempts_left: u32,
        #[serde(default, skip_serializing_if = "is_zero")]
        retry_after_s: u64,
    },
    /// Viewer → agent, ~1/s: receive-side network stats for adaptive quality.
    ViewerStats {
        #[serde(default)]
        rtt_ms: Option<u32>,
        /// Fraction of packets lost since the previous report (0..1).
        loss: f32,
        fps: u32,
    },
    /// Agent → viewer: what the encoder is doing (stats overlay).
    EncoderStatus {
        encoder: String,
        bitrate_kbps: u32,
        fps: u32,
        scale_pct: u32,
    },
    /// Clock sync for latency measurement: viewer sends its clock (ms), agent echoes it with its own.
    Ping {
        t: f64,
    },
    Pong {
        t: f64,
        agent_ms: f64,
    },
    /// Agent → viewer: whether input injection works (e.g. missing Accessibility permission).
    InputStatus {
        available: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
    },
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayDescriptor {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn matches_shared_vectors() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../../../proto/input-vectors.json")).unwrap();
        for vector in v["vectors"].as_array().unwrap() {
            let name = vector["name"].as_str().unwrap();
            let msg: InputMessage = serde_json::from_value(vector["message"].clone()).unwrap();
            let want = vector["hex"].as_str().unwrap();
            assert_eq!(hex(&msg.encode()), want, "encode {name}");
            let bytes: Vec<u8> = (0..want.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&want[i..i + 2], 16).unwrap())
                .collect();
            assert_eq!(InputMessage::decode(&bytes).unwrap(), msg, "decode {name}");
        }
    }

    #[test]
    fn rejects_malformed_messages() {
        assert_eq!(InputMessage::decode(&[]), Err(DecodeError::Empty));
        assert_eq!(
            InputMessage::decode(&[99]),
            Err(DecodeError::UnknownType(99))
        );
        assert_eq!(
            InputMessage::decode(&[1, 0, 0, 0]),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            InputMessage::decode(&[2, 0, 1, 9]),
            Err(DecodeError::TrailingBytes)
        );
        assert_eq!(
            InputMessage::decode(&[4, 1, 0, 0, 3, b'K', b'e']),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            InputMessage::decode(&[4, 1, 0, 0, 200]),
            Err(DecodeError::TooLong)
        );
        assert_eq!(
            InputMessage::decode(&[4, 1, 0, 0, 1, 0xff]),
            Err(DecodeError::BadUtf8)
        );
        assert_eq!(
            InputMessage::decode(&[6, 0]),
            Err(DecodeError::TrailingBytes)
        );
        let mut big = vec![5, 1];
        big.extend(std::iter::repeat_n(b'a', CLIPBOARD_CHUNK_BYTES + 1));
        assert_eq!(InputMessage::decode(&big), Err(DecodeError::TooLong));
    }

    #[test]
    fn clipboard_round_trip_respects_utf8_and_limits() {
        let text = "é".repeat(CLIPBOARD_CHUNK_BYTES); // 2 bytes each → spans chunks mid-char
        let chunks = clipboard_chunks(&text);
        assert!(chunks.len() >= 2);
        let mut asm = ClipboardAssembler::default();
        let mut got = None;
        for c in &chunks {
            let InputMessage::ClipboardChunk { last, data } =
                InputMessage::decode(&c.encode()).unwrap()
            else {
                panic!()
            };
            got = asm.push(last, &data);
        }
        assert_eq!(got.as_deref(), Some(text.as_str()));

        assert_eq!(
            clipboard_chunks(""),
            vec![InputMessage::ClipboardChunk {
                last: true,
                data: String::new()
            }]
        );
        let huge = "x".repeat(MAX_CLIPBOARD_BYTES + 10);
        let total: usize = clipboard_chunks(&huge)
            .iter()
            .map(|c| match c {
                InputMessage::ClipboardChunk { data, .. } => data.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(total, MAX_CLIPBOARD_BYTES, "oversized text is truncated");
    }

    #[test]
    fn assembler_drops_oversized_transfer_and_recovers() {
        let mut asm = ClipboardAssembler::default();
        let chunk = "x".repeat(CLIPBOARD_CHUNK_BYTES);
        for _ in 0..(MAX_CLIPBOARD_BYTES / CLIPBOARD_CHUNK_BYTES) {
            assert_eq!(asm.push(false, &chunk), None);
        }
        assert_eq!(
            asm.push(true, "overflow"),
            None,
            "transfer over the limit is discarded"
        );
        assert_eq!(
            asm.push(true, "next").as_deref(),
            Some("next"),
            "next transfer works"
        );
    }

    #[test]
    fn control_messages_json_shape() {
        let m = ControlMessage::SelectDisplay { index: 1 };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"type":"select_display","index":1}"#
        );
        let s: ControlMessage =
            serde_json::from_str(r#"{"type":"input_status","available":false}"#).unwrap();
        assert_eq!(
            s,
            ControlMessage::InputStatus {
                available: false,
                message: String::new()
            }
        );
    }
}
