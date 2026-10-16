//! H.264 encoding for carson-agent.
//!
//! [`new_encoder`] picks the best encoder for this machine: the OS hardware encoder
//! (VideoToolbox on macOS) when available, otherwise OpenH264 in software.
//!
//! Licensing (carson is free; we keep it that way): hardware encoders are licensed by the OS
//! vendor. OpenH264 is used through Cisco's prebuilt binary, which Cisco distributes
//! royalty-free (`openh264-cisco` feature + [`openh264_cisco`] downloader). The `openh264-source`
//! feature compiles OpenH264 from source for development and tests only; release builds must
//! not enable it.

use std::time::Duration;

#[cfg(feature = "openh264-cisco")]
pub mod openh264_cisco;
mod openh264_enc;
#[cfg(target_os = "macos")]
mod videotoolbox;

pub use openh264_enc::H264Encoder;

#[derive(Debug, Clone, Copy)]
pub struct EncoderSettings {
    pub fps: u32,
    pub bitrate_bps: u32,
    /// Safety-net keyframe interval; keyframes are also produced on demand (PLI).
    pub keyframe_interval: Duration,
}

impl Default for EncoderSettings {
    fn default() -> Self {
        Self {
            fps: 30,
            bitrate_bps: 4_000_000,
            keyframe_interval: Duration::from_secs(4),
        }
    }
}

/// An encoded access unit in Annex B format (start-code delimited NAL units).
#[derive(Debug)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub width: usize,
    pub height: usize,
}

/// A real-time H.264 (Constrained Baseline) encoder.
pub trait VideoEncoder: Send {
    /// Encodes one tightly packed BGRA frame (even dimensions). `None` = frame skipped.
    fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: usize,
        height: usize,
    ) -> anyhow::Result<Option<EncodedFrame>>;
    /// Makes the next encoded frame an IDR.
    fn request_keyframe(&mut self);
    /// Changes the target bitrate (takes effect within a frame or two).
    fn set_bitrate(&mut self, bps: u32);
    /// Short name for logs and stats ("videotoolbox", "openh264").
    fn name(&self) -> &'static str;
}

/// Which encoders to consider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncoderPreference {
    /// Hardware if available, else software.
    #[default]
    Auto,
    /// Software only (comparisons, debugging).
    Software,
}

/// Creates the best available encoder.
pub fn new_encoder(
    settings: EncoderSettings,
    pref: EncoderPreference,
) -> anyhow::Result<Box<dyn VideoEncoder>> {
    #[cfg(target_os = "macos")]
    if pref == EncoderPreference::Auto {
        // Created lazily on the first frame (size known then); failures fall back below.
        if videotoolbox::available() {
            return Ok(Box::new(videotoolbox::VtEncoder::new(settings)));
        }
    }
    let _ = pref;
    Ok(Box::new(H264Encoder::new(settings)?))
}

/// Iterates over NAL unit types in an Annex B buffer (for tests and diagnostics).
pub fn nal_types(annexb: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < annexb.len() {
        let three = annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1;
        let four = i + 4 < annexb.len()
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1;
        if four {
            types.push(annexb[i + 4] & 0x1f);
            i += 4;
        } else if three {
            types.push(annexb[i + 3] & 0x1f);
            i += 3;
        } else {
            i += 1;
        }
    }
    types
}

/// Converts AVCC (4-byte big-endian length prefixes) to Annex B start codes.
pub(crate) fn avcc_to_annexb(avcc: &[u8], out: &mut Vec<u8>) -> anyhow::Result<()> {
    let mut i = 0;
    while i < avcc.len() {
        anyhow::ensure!(i + 4 <= avcc.len(), "truncated AVCC length");
        let len = u32::from_be_bytes([avcc[i], avcc[i + 1], avcc[i + 2], avcc[i + 3]]) as usize;
        i += 4;
        anyhow::ensure!(i + len <= avcc.len(), "AVCC NAL overruns buffer");
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[i..i + len]);
        i += len;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_frames {
    /// A gradient with a small square whose position depends on `seed` (screen-like motion).
    pub fn frame(w: usize, h: usize, seed: u8) -> Vec<u8> {
        let mut buf: Vec<u8> = (0..w * h * 4)
            .map(|i| ((i / 4) % w * 255 / w) as u8)
            .collect();
        let x0 = (seed as usize * 4) % (w - 8);
        for y in 8..16 {
            for x in x0..x0 + 8 {
                buf[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nal_parser() {
        assert_eq!(
            nal_types(&[0, 0, 0, 1, 0x67, 9, 0, 0, 1, 0x68, 9, 0, 0, 0, 1, 0x65]),
            vec![7, 8, 5]
        );
    }

    #[test]
    fn avcc_conversion() {
        let mut out = Vec::new();
        avcc_to_annexb(&[0, 0, 0, 2, 0x65, 0xAA, 0, 0, 0, 1, 0x41], &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xAA, 0, 0, 0, 1, 0x41]);
        assert!(avcc_to_annexb(&[0, 0, 0, 9, 1], &mut Vec::new()).is_err());
    }
}
