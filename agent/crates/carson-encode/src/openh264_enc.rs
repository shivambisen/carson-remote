//! OpenH264 software encoder (screen-content tuning).

use std::time::Instant;

use crate::{EncodedFrame, EncoderSettings, VideoEncoder};

use anyhow::{Context, Result};
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile,
    RateControlMode, SpsPpsStrategy, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer};

pub struct H264Encoder {
    settings: EncoderSettings,
    inner: Option<(Encoder, YUVBuffer, usize, usize)>,
    force_keyframe: bool,
    last_keyframe: Option<Instant>,
}

impl H264Encoder {
    pub fn new(settings: EncoderSettings) -> anyhow::Result<Self> {
        Ok(Self {
            settings,
            inner: None,
            force_keyframe: true,
            last_keyframe: None,
        })
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
            let enc =
                Encoder::with_api_config(api()?, config).context("create OpenH264 encoder")?;
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

/// The OpenH264 library: Cisco's binary when available (free to distribute), else the
/// source build (development only).
fn api() -> anyhow::Result<OpenH264API> {
    #[cfg(feature = "openh264-cisco")]
    if let Some(path) = crate::openh264_cisco::installed_path() {
        return OpenH264API::from_blob_path(&path)
            .map_err(|e| anyhow::anyhow!("load {}: {e}", path.display()));
    }
    #[cfg(feature = "openh264-source")]
    {
        Ok(OpenH264API::from_source())
    }
    #[cfg(not(feature = "openh264-source"))]
    {
        anyhow::bail!(
            "OpenH264 is not installed; run `carson-agent setup` to download Cisco's free OpenH264 library"
        )
    }
}

impl VideoEncoder for H264Encoder {
    fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: usize,
        height: usize,
    ) -> anyhow::Result<Option<EncodedFrame>> {
        H264Encoder::encode_bgra(self, bgra, width, height)
    }
    fn request_keyframe(&mut self) {
        H264Encoder::request_keyframe(self);
    }
    fn set_bitrate(&mut self, bps: u32) {
        // OpenH264 has no stable live-rate API in this crate: rebuild on the next frame.
        // Rate changes are rare (the controller holds each step for seconds), so the extra
        // IDR is acceptable for the software path.
        if bps != self.settings.bitrate_bps {
            self.settings.bitrate_bps = bps;
            self.inner = None;
        }
    }
    fn name(&self) -> &'static str {
        "openh264"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nal_types;
    use crate::test_frames::frame;

    const SPS: u8 = 7;
    const PPS: u8 = 8;
    const IDR: u8 = 5;
    const NON_IDR: u8 = 1;

    #[test]
    fn first_frame_is_idr_with_parameter_sets_then_p_frames() {
        let mut enc = H264Encoder::new(EncoderSettings::default()).unwrap();
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
        let mut enc = H264Encoder::new(EncoderSettings::default()).unwrap();
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
            keyframe_interval: std::time::Duration::from_millis(30),
            ..Default::default()
        })
        .unwrap();
        enc.encode_bgra(&frame(32, 32, 1), 32, 32).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(40));
        assert!(
            enc.encode_bgra(&frame(32, 32, 2), 32, 32)
                .unwrap()
                .unwrap()
                .keyframe
        );
    }

    #[test]
    fn rejects_bad_input() {
        let mut enc = H264Encoder::new(EncoderSettings::default()).unwrap();
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
