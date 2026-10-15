//! H.264 encoding for carson-agent.
//!
//! Milestone 3 uses OpenH264 in software (Constrained Baseline, screen-content tuning).
//! Hardware encoders (VideoToolbox, Media Foundation, VA-API) replace it in M7.
//!
//! Licensing: the default `openh264` build compiles Cisco's source, which is NOT covered by
//! Cisco's royalty-free binary license. Distributed builds must switch to the crate's
//! `libloading` feature and load Cisco's prebuilt binary (tracked for M8 packaging).

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile,
    RateControlMode, SpsPpsStrategy, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer};

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

pub struct H264Encoder {
    settings: EncoderSettings,
    inner: Option<(Encoder, YUVBuffer, usize, usize)>,
    force_keyframe: bool,
    last_keyframe: Option<Instant>,
}

impl H264Encoder {
    pub fn new(settings: EncoderSettings) -> Self {
        Self {
            settings,
            inner: None,
            force_keyframe: true,
            last_keyframe: None,
        }
    }

    /// Makes the next encoded frame a keyframe (IDR).
    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn encoder_for(
        &mut self,
        width: usize,
        height: usize,
    ) -> Result<&mut (Encoder, YUVBuffer, usize, usize)> {
        let reuse = matches!(&self.inner, Some((_, _, w, h)) if *w == width && *h == height);
        if !reuse {
            let config = EncoderConfig::new()
                .usage_type(UsageType::ScreenContentRealTime)
                .profile(Profile::Baseline)
                .bitrate(BitRate::from_bps(self.settings.bitrate_bps))
                .max_frame_rate(FrameRate::from_hz(self.settings.fps as f32))
                .rate_control_mode(RateControlMode::Bitrate)
                .complexity(Complexity::Low)
                // Repeat SPS/PPS so a decoder can join at any IDR.
                .sps_pps_strategy(SpsPpsStrategy::ConstantId)
                // We schedule keyframes ourselves.
                .intra_frame_period(IntraFramePeriod::from_num_frames(0))
                // Without frame skipping OpenH264 cannot hold the target bitrate.
                .skip_frames(true)
                .adaptive_quantization(false)
                .background_detection(false);
            let enc = Encoder::with_api_config(OpenH264API::from_source(), config)
                .context("create OpenH264 encoder")?;
            self.inner = Some((enc, YUVBuffer::new(width, height), width, height));
            self.force_keyframe = true;
        }
        Ok(self.inner.as_mut().expect("encoder initialized above"))
    }

    /// Encodes one tightly packed BGRA frame. Dimensions must be even. Returns `None` when the
    /// encoder skipped the frame.
    pub fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: usize,
        height: usize,
    ) -> Result<Option<EncodedFrame>> {
        anyhow::ensure!(
            width.is_multiple_of(2) && height.is_multiple_of(2),
            "dimensions must be even, got {width}x{height}"
        );
        anyhow::ensure!(
            bgra.len() == width * height * 4,
            "buffer is {} bytes, expected {}",
            bgra.len(),
            width * height * 4
        );
        let due = self
            .last_keyframe
            .is_none_or(|t| t.elapsed() >= self.settings.keyframe_interval);
        let want_key = self.force_keyframe || due;
        let (enc, yuv, _, _) = self.encoder_for(width, height)?;
        yuv.read_bgra8(BgraSliceU8::new(bgra, (width, height)));
        if want_key {
            enc.force_intra_frame();
        }
        let bitstream = enc.encode(yuv).context("encode frame")?;
        let frame_type = bitstream.frame_type();
        if matches!(frame_type, FrameType::Skip | FrameType::Invalid) {
            return Ok(None);
        }
        // Only IDR frames are decoder entry points; scene-change I-frames are not.
        let keyframe = frame_type == FrameType::IDR;
        let data = bitstream.to_vec();
        if keyframe {
            self.force_keyframe = false;
            self.last_keyframe = Some(Instant::now());
        }
        Ok(Some(EncodedFrame {
            data,
            keyframe,
            width,
            height,
        }))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: u8 = 7;
    const PPS: u8 = 8;
    const IDR: u8 = 5;
    const NON_IDR: u8 = 1;

    /// A gradient with a small square whose position depends on `seed` (screen-like motion).
    fn frame(w: usize, h: usize, seed: u8) -> Vec<u8> {
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

    #[test]
    fn first_frame_is_idr_with_parameter_sets_then_p_frames() {
        let mut enc = H264Encoder::new(EncoderSettings::default());
        let first = enc.encode_bgra(&frame(64, 48, 3), 64, 48).unwrap().unwrap();
        assert!(first.keyframe);
        let types = nal_types(&first.data);
        assert!(
            types.contains(&SPS) && types.contains(&PPS) && types.contains(&IDR),
            "{types:?}"
        );

        let second = enc.encode_bgra(&frame(64, 48, 5), 64, 48).unwrap().unwrap();
        assert!(!second.keyframe);
        assert!(nal_types(&second.data).contains(&NON_IDR));
    }

    #[test]
    fn keyframe_on_request_and_on_resize() {
        let mut enc = H264Encoder::new(EncoderSettings::default());
        enc.encode_bgra(&frame(64, 48, 3), 64, 48).unwrap();
        enc.encode_bgra(&frame(64, 48, 4), 64, 48).unwrap();
        enc.request_keyframe();
        assert!(
            enc.encode_bgra(&frame(64, 48, 5), 64, 48)
                .unwrap()
                .unwrap()
                .keyframe
        );

        let resized = enc.encode_bgra(&frame(96, 64, 6), 96, 64).unwrap().unwrap();
        assert!(resized.keyframe, "a new resolution must start with an IDR");
        assert_eq!((resized.width, resized.height), (96, 64));
    }

    #[test]
    fn periodic_keyframe() {
        let mut enc = H264Encoder::new(EncoderSettings {
            keyframe_interval: Duration::from_millis(30),
            ..Default::default()
        });
        enc.encode_bgra(&frame(32, 32, 1), 32, 32).unwrap();
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            enc.encode_bgra(&frame(32, 32, 2), 32, 32)
                .unwrap()
                .unwrap()
                .keyframe
        );
    }

    #[test]
    fn rejects_bad_input() {
        let mut enc = H264Encoder::new(EncoderSettings::default());
        assert!(enc.encode_bgra(&frame(33, 32, 1), 33, 32).is_err());
        assert!(enc.encode_bgra(&[0; 10], 32, 32).is_err());
    }

    #[test]
    fn nal_parser() {
        assert_eq!(
            nal_types(&[0, 0, 0, 1, 0x67, 9, 0, 0, 1, 0x68, 9, 0, 0, 0, 1, 0x65]),
            vec![SPS, PPS, IDR]
        );
    }
}
