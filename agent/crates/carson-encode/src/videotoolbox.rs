//! Hardware H.264 via Apple VideoToolbox (Apple Silicon and Intel Macs with a media engine).
//!
//! Low-latency rate control, Constrained Baseline, no frame reordering, and one frame in /
//! one frame out (`CompleteFrames` after every frame). If a session cannot be created the
//! encoder falls back to OpenH264 transparently.
#![allow(unsafe_code)] // VideoToolbox/CoreMedia FFI; every block has a SAFETY note.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    kCMSampleAttachmentKey_NotSync, kCMTimeInvalid, kCMVideoCodecType_H264,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSession, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
};

use crate::{EncodedFrame, EncoderSettings, H264Encoder, VideoEncoder};

/// VideoToolbox ships with every supported macOS; whether a hardware session can actually be
/// created is discovered on the first frame (with fallback).
pub fn available() -> bool {
    true
}

/// Where the output callback writes the encoded frame.
#[derive(Default)]
struct Output {
    data: Vec<u8>,
    keyframe: bool,
    status: i32,
    produced: bool,
}

struct Session {
    session: CFRetained<VTCompressionSession>,
    width: usize,
    height: usize,
    /// Boxed so its address (passed to VideoToolbox as refcon) is stable.
    out: Box<Mutex<Output>>,
}

// SAFETY: a compression session may be used from any thread, one call at a time; the encoder
// owns it exclusively and the callback only touches the Mutex-protected output.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: invalidating stops callbacks before `out` is freed (field drop order follows).
        unsafe { self.session.invalidate() };
    }
}

fn as_session(s: &VTCompressionSession) -> &VTSession {
    // SAFETY: VTCompressionSession is a VTSession (a CFType); this is the documented upcast.
    unsafe { &*(s as *const VTCompressionSession).cast::<VTSession>() }
}

fn set_property(s: &VTCompressionSession, key: &CFString, value: &CFType) -> Result<()> {
    // SAFETY: valid session, key and value for the lifetime of the call.
    let st = unsafe { VTSessionSetProperty(as_session(s), key, Some(value)) };
    if st != 0 {
        bail!("VTSessionSetProperty({key}) failed: {st}");
    }
    Ok(())
}

/// Called by VideoToolbox (on its thread, or inline during CompleteFrames) per encoded frame.
unsafe extern "C-unwind" fn on_encoded(
    refcon: *mut c_void,
    _source_frame: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: refcon is the Box<Mutex<Output>> owned by the live Session.
    let out = unsafe { &*(refcon as *const Mutex<Output>) };
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    o.produced = true;
    o.status = status;
    if status != 0 || sample.is_null() {
        return; // dropped frame or error; reported to the caller
    }
    // SAFETY: VideoToolbox guarantees a valid sample buffer for the duration of the callback.
    let sample = unsafe { &*sample };
    o.keyframe = is_sync(sample);
    let mut data = Vec::new();
    if o.keyframe {
        append_parameter_sets(sample, &mut data);
    }
    if append_frame(sample, &mut data).is_ok() {
        o.data = data;
    } else {
        o.status = -1;
    }
}

/// A sample is a sync frame (IDR) unless its attachments say NotSync.
fn is_sync(sample: &CMSampleBuffer) -> bool {
    // SAFETY: read-only queries on a valid sample buffer.
    unsafe {
        let Some(attachments) = sample.sample_attachments_array(false) else {
            return true;
        };
        if attachments.count() == 0 {
            return true;
        }
        let dict = attachments.value_at_index(0) as *const CFDictionary;
        if dict.is_null() {
            return true;
        }
        let key: *const CFString = kCMSampleAttachmentKey_NotSync;
        !(*dict).contains_ptr_key(key.cast())
    }
}

/// Prepends SPS and PPS (Annex B) so a decoder can join at this IDR.
fn append_parameter_sets(sample: &CMSampleBuffer, out: &mut Vec<u8>) {
    // SAFETY: read-only queries; parameter set pointers stay valid while `desc` is alive.
    unsafe {
        let Some(desc) = sample.format_description() else {
            return;
        };
        let mut count = 0usize;
        let _ = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            &desc,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut count,
            std::ptr::null_mut(),
        );
        for i in 0..count {
            let (mut ptr, mut len) = (std::ptr::null::<u8>(), 0usize);
            let st = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                &desc,
                i,
                &mut ptr,
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if st == 0 && !ptr.is_null() {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(std::slice::from_raw_parts(ptr, len));
            }
        }
    }
}

/// Copies the frame's AVCC payload and converts it to Annex B.
fn append_frame(sample: &CMSampleBuffer, out: &mut Vec<u8>) -> Result<()> {
    // SAFETY: we copy out of a valid block buffer into an owned, correctly sized Vec.
    unsafe {
        let block = sample.data_buffer().context("sample has no data")?;
        let len = block.data_length();
        let mut avcc = vec![0u8; len];
        let st = block.copy_data_bytes(
            0,
            len,
            NonNull::new(avcc.as_mut_ptr().cast()).expect("non-null"),
        );
        if st != 0 {
            bail!("CMBlockBufferCopyDataBytes failed: {st}");
        }
        crate::avcc_to_annexb(&avcc, out)
    }
}

impl Session {
    fn create(width: usize, height: usize, settings: &EncoderSettings) -> Result<Self> {
        let out: Box<Mutex<Output>> = Box::default();
        // SAFETY: all CF objects passed in outlive the call; `raw` receives a +1 session.
        let session = unsafe {
            let spec = CFDictionary::<CFString, CFType>::from_slices(
                &[kVTVideoEncoderSpecification_EnableLowLatencyRateControl],
                &[CFBoolean::new(true).as_ref()],
            );
            let (fmt, w, h) = (
                CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32),
                CFNumber::new_i32(width as i32),
                CFNumber::new_i32(height as i32),
            );
            let iosurface = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
            let attrs = CFDictionary::<CFString, CFType>::from_slices(
                &[
                    kCVPixelBufferPixelFormatTypeKey,
                    kCVPixelBufferWidthKey,
                    kCVPixelBufferHeightKey,
                    kCVPixelBufferIOSurfacePropertiesKey,
                ],
                &[fmt.as_ref(), w.as_ref(), h.as_ref(), iosurface.as_ref()],
            );
            let mut raw: *mut VTCompressionSession = std::ptr::null_mut();
            let st = VTCompressionSession::create(
                None,
                width as i32,
                height as i32,
                kCMVideoCodecType_H264,
                Some(spec.as_ref()),
                Some(attrs.as_ref()),
                None,
                Some(on_encoded),
                (&*out as *const Mutex<Output>).cast_mut().cast(),
                NonNull::from(&mut raw),
            );
            if st != 0 || raw.is_null() {
                bail!("VTCompressionSessionCreate failed: {st}");
            }
            CFRetained::from_raw(NonNull::new_unchecked(raw))
        };
        let s = &*session;
        set_property(
            s,
            unsafe { kVTCompressionPropertyKey_RealTime },
            CFBoolean::new(true).as_ref(),
        )?;
        set_property(
            s,
            unsafe { kVTCompressionPropertyKey_AllowFrameReordering },
            CFBoolean::new(false).as_ref(),
        )?;
        set_property(
            s,
            unsafe { kVTCompressionPropertyKey_ProfileLevel },
            unsafe { kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel }.as_ref(),
        )?;
        set_property(
            s,
            unsafe { kVTCompressionPropertyKey_AverageBitRate },
            CFNumber::new_i32(settings.bitrate_bps as i32).as_ref(),
        )?;
        set_property(
            s,
            unsafe { kVTCompressionPropertyKey_ExpectedFrameRate },
            CFNumber::new_i32(settings.fps as i32).as_ref(),
        )?;
        let gop = (settings.fps as f64 * settings.keyframe_interval.as_secs_f64()).max(1.0) as i32;
        set_property(
            s,
            unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval },
            CFNumber::new_i32(gop).as_ref(),
        )?;
        // SAFETY: valid session.
        unsafe { session.prepare_to_encode_frames() };
        Ok(Self {
            session,
            width,
            height,
            out,
        })
    }

    fn pool(&self) -> Result<CFRetained<CVPixelBufferPool>> {
        // SAFETY: valid session; the pool is retained.
        unsafe { self.session.pixel_buffer_pool() }.context("encoder has no pixel buffer pool")
    }

    fn encode(
        &mut self,
        bgra: &[u8],
        frame_no: i64,
        fps: u32,
        force_key: bool,
    ) -> Result<Option<(Vec<u8>, bool)>> {
        let pool = self.pool()?;
        // SAFETY: the pool vends a BGRA buffer of our size; we hold the base address lock while
        // copying exactly `height` rows of `width * 4` bytes into it.
        let pixel_buffer = unsafe {
            let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
            let st = CVPixelBufferPool::create_pixel_buffer(None, &pool, NonNull::from(&mut raw));
            if st != 0 || raw.is_null() {
                bail!("CVPixelBufferPoolCreatePixelBuffer failed: {st}");
            }
            let pb = CFRetained::from_raw(NonNull::new_unchecked(raw));
            if CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags(0)) != 0 {
                bail!("lock pixel buffer");
            }
            let base = CVPixelBufferGetBaseAddress(&pb).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRow(&pb);
            let row = self.width * 4;
            for y in 0..self.height {
                std::ptr::copy_nonoverlapping(
                    bgra.as_ptr().add(y * row),
                    base.add(y * stride),
                    row,
                );
            }
            CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags(0));
            pb
        };
        let props = force_key.then(|| unsafe {
            CFDictionary::<CFString, CFType>::from_slices(
                &[kVTEncodeFrameOptionKey_ForceKeyFrame],
                &[CFBoolean::new(true).as_ref()],
            )
        });
        *self.out.lock().unwrap_or_else(|e| e.into_inner()) = Output::default();
        // SAFETY: valid session and buffer; CompleteFrames runs the callback before returning.
        unsafe {
            let pts = CMTime::new(frame_no, fps as i32);
            let st = self.session.encode_frame(
                &pixel_buffer,
                pts,
                kCMTimeInvalid,
                props.as_deref().map(|d| d.as_ref()),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if st != 0 {
                bail!("VTCompressionSessionEncodeFrame failed: {st}");
            }
            let st = self.session.complete_frames(kCMTimeInvalid);
            if st != 0 {
                bail!("VTCompressionSessionCompleteFrames failed: {st}");
            }
        }
        let mut o = self.out.lock().unwrap_or_else(|e| e.into_inner());
        if !o.produced || o.data.is_empty() {
            return if o.status != 0 {
                Err(anyhow!("encoder error {}", o.status))
            } else {
                Ok(None)
            };
        }
        Ok(Some((std::mem::take(&mut o.data), o.keyframe)))
    }

    fn set_bitrate(&self, bps: u32) -> Result<()> {
        set_property(
            &self.session,
            unsafe { kVTCompressionPropertyKey_AverageBitRate },
            CFNumber::new_i32(bps as i32).as_ref(),
        )
    }
}

pub struct VtEncoder {
    settings: EncoderSettings,
    session: Option<Session>,
    /// Software fallback if no hardware session could be created.
    fallback: Option<H264Encoder>,
    force_key: bool,
    frame_no: i64,
    last_key: Option<Instant>,
}

impl VtEncoder {
    pub fn new(settings: EncoderSettings) -> Self {
        Self {
            settings,
            session: None,
            fallback: None,
            force_key: true,
            frame_no: 0,
            last_key: None,
        }
    }
}

impl VideoEncoder for VtEncoder {
    fn encode_bgra(
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
        if let Some(fb) = &mut self.fallback {
            return fb.encode_bgra(bgra, width, height);
        }
        if !matches!(&self.session, Some(s) if s.width == width && s.height == height) {
            match Session::create(width, height, &self.settings) {
                Ok(s) => {
                    self.session = Some(s);
                    self.force_key = true;
                }
                Err(e) => {
                    tracing_lite(&format!("VideoToolbox unavailable ({e:#}); using OpenH264"));
                    let mut fb = H264Encoder::new(self.settings)?;
                    let r = fb.encode_bgra(bgra, width, height);
                    self.fallback = Some(fb);
                    return r;
                }
            }
        }
        let due = self
            .last_key
            .is_none_or(|t| t.elapsed() >= self.settings.keyframe_interval);
        let force = self.force_key || due;
        let session = self.session.as_mut().expect("created above");
        let result = session.encode(bgra, self.frame_no, self.settings.fps, force)?;
        self.frame_no += 1;
        Ok(result.map(|(data, keyframe)| {
            if keyframe {
                self.force_key = false;
                self.last_key = Some(Instant::now());
            }
            EncodedFrame {
                data,
                keyframe,
                width,
                height,
            }
        }))
    }

    fn request_keyframe(&mut self) {
        self.force_key = true;
        if let Some(fb) = &mut self.fallback {
            fb.request_keyframe();
        }
    }

    fn set_bitrate(&mut self, bps: u32) {
        self.settings.bitrate_bps = bps;
        if let Some(fb) = &mut self.fallback {
            fb.set_bitrate(bps);
        } else if let Some(s) = &self.session
            && let Err(e) = s.set_bitrate(bps)
        {
            tracing_lite(&format!("set bitrate: {e:#}"));
        }
    }

    fn name(&self) -> &'static str {
        if self.fallback.is_some() {
            "openh264"
        } else {
            "videotoolbox"
        }
    }
}

/// carson-encode has no logging dependency; warnings go to stderr.
fn tracing_lite(msg: &str) {
    eprintln!("carson-encode: {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nal_types;
    use crate::test_frames::frame;

    #[test]
    fn hardware_encoder_produces_idr_then_p_frames() {
        let mut enc = VtEncoder::new(EncoderSettings::default());
        let (w, h) = (640, 360);
        let first = enc
            .encode_bgra(&frame(w, h, 1), w, h)
            .unwrap()
            .expect("first frame");
        assert_eq!(
            enc.name(),
            "videotoolbox",
            "this test needs a Mac with VideoToolbox"
        );
        assert!(first.keyframe);
        let types = nal_types(&first.data);
        assert!(
            types.contains(&7) && types.contains(&8) && types.contains(&5),
            "{types:?}"
        );

        let mut saw_p = false;
        for i in 2..10 {
            if let Some(f) = enc.encode_bgra(&frame(w, h, i), w, h).unwrap() {
                saw_p |= !f.keyframe && nal_types(&f.data).contains(&1);
            }
        }
        assert!(saw_p, "steady state should be P-frames");

        enc.request_keyframe();
        let k = enc.encode_bgra(&frame(w, h, 11), w, h).unwrap().unwrap();
        assert!(k.keyframe, "forced keyframe");
        enc.set_bitrate(1_000_000);
        assert!(enc.encode_bgra(&frame(w, h, 12), w, h).unwrap().is_some());

        let resized = enc
            .encode_bgra(&frame(320, 180, 13), 320, 180)
            .unwrap()
            .unwrap();
        assert!(resized.keyframe && resized.width == 320);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::test_frames::frame;

    /// `cargo test -p carson-encode --release -- --ignored --nocapture encode_speed`
    #[test]
    #[ignore]
    fn encode_speed() {
        let (w, h) = (1280, 720);
        let frames: Vec<Vec<u8>> = (0..60).map(|i| frame(w, h, i as u8)).collect();
        for (name, mut enc) in [
            (
                "videotoolbox",
                Box::new(VtEncoder::new(EncoderSettings::default())) as Box<dyn VideoEncoder>,
            ),
            (
                "openh264",
                Box::new(H264Encoder::new(EncoderSettings::default()).unwrap()),
            ),
        ] {
            enc.encode_bgra(&frames[0], w, h).unwrap();
            let t = Instant::now();
            for f in &frames {
                enc.encode_bgra(f, w, h).unwrap();
            }
            eprintln!(
                "{name}: {:.2} ms/frame at {w}x{h}",
                t.elapsed().as_secs_f64() * 1000.0 / frames.len() as f64
            );
        }
    }
}
