//! Capture → encode pipeline. Capture and encoding run on a dedicated OS thread (both are
//! blocking, CPU-bound work); encoded frames cross into async code over a small channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use carson_capture::{CaptureOptions, Capturer, Frame, TestPattern};
use carson_encode::{EncodedFrame, EncoderPreference, EncoderSettings, new_encoder};
use tokio::sync::mpsc;

/// Where frames come from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    Screen,
    /// Synthetic moving pattern (CI, headless machines, debugging).
    TestPattern,
}

/// How viewer input is applied.
#[derive(Debug, Clone)]
pub enum InputMode {
    /// Inject into the OS (needs Accessibility permission on macOS).
    Native,
    /// Log input instead of injecting (testing without touching this machine).
    Log,
    /// View only: ignore input.
    Off,
    /// Record injected events (tests).
    #[cfg(test)]
    Record(std::sync::Arc<std::sync::Mutex<Vec<String>>>),
}

#[derive(Debug, Clone)]
pub struct MediaOptions {
    pub source: Source,
    pub capture: CaptureOptions,
    pub encoder: EncoderSettings,
    pub input: InputMode,
    /// Sync text clipboard with the viewer.
    pub clipboard: bool,
    /// Hardware encoder if available (default) or software only.
    pub encoder_pref: EncoderPreference,
}

impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            source: Source::Screen,
            capture: CaptureOptions::default(),
            encoder: EncoderSettings::default(),
            input: InputMode::Native,
            clipboard: true,
            encoder_pref: EncoderPreference::Auto,
        }
    }
}

const TEST_PATTERN_SIZE: (usize, usize) = (1280, 720);

/// Displays the viewer can choose from.
pub fn list_displays(opts: &MediaOptions) -> anyhow::Result<Vec<carson_capture::DisplayInfo>> {
    match opts.source {
        Source::Screen => carson_capture::displays(),
        Source::TestPattern => {
            let (w, h) = TEST_PATTERN_SIZE;
            Ok(vec![carson_capture::DisplayInfo {
                index: 0,
                name: "Test pattern".into(),
                width: w as u32,
                height: h as u32,
                primary: true,
                bounds: (0.0, 0.0, w as f64, h as f64),
            }])
        }
    }
}

/// Opens the configured frame source. Fails fast (e.g. missing permission) so a session
/// can be rejected with a useful reason instead of streaming black frames.
pub fn open_source(opts: &MediaOptions) -> anyhow::Result<Box<dyn Capturer>> {
    match opts.source {
        Source::Screen => carson_capture::screen(&opts.capture),
        Source::TestPattern => {
            let (w, h) = TEST_PATTERN_SIZE;
            Ok(Box::new(TestPattern::new(w, h, opts.capture.fps)))
        }
    }
}

/// An encoded frame with its capture time.
pub struct TimedFrame {
    pub frame: EncodedFrame,
    pub captured_at: Instant,
}

/// Controls a running pipeline.
#[derive(Clone, Default)]
pub struct PipelineControl {
    pub keyframe: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
    /// Set to a display index to switch the capture source.
    pub switch_display: Arc<std::sync::Mutex<Option<usize>>>,
    /// New target quality from the adaptive controller (applied before the next frame).
    pub quality: Arc<std::sync::Mutex<Option<crate::abr::Quality>>>,
    /// Name of the encoder in use ("videotoolbox", "openh264"), for status reports.
    pub encoder_name: Arc<std::sync::Mutex<&'static str>>,
}

/// How long the screen may be static before the last frame is re-sent. Keeps the stream
/// (and stats) alive and lets late keyframe requests be answered.
const IDLE_REFRESH: Duration = Duration::from_secs(1);
/// Consecutive encoder failures tolerated (each followed by a fresh encoder) before giving up.
const MAX_ENCODER_FAILURES: u32 = 3;

/// Starts the capture/encode thread. The thread exits when `ctl.stop` is set or the
/// receiver is dropped.
pub fn spawn_pipeline(
    mut capturer: Box<dyn Capturer>,
    mut opts: MediaOptions,
    ctl: PipelineControl,
) -> mpsc::Receiver<TimedFrame> {
    let mut settings = opts.encoder;
    let pref = opts.encoder_pref;
    let (tx, rx) = mpsc::channel::<TimedFrame>(4);
    std::thread::Builder::new()
        .name("carson-encode".into())
        .spawn(move || {
            let make_encoder = |s: EncoderSettings| match new_encoder(s, pref) {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "no H.264 encoder available");
                    None
                }
            };
            let Some(mut encoder) = make_encoder(settings) else { return };
            let mut last: Option<Frame> = None;
            let mut last_sent = Instant::now();
            let mut last_encoded: Option<Instant> = None;
            let mut quality = crate::abr::Quality { bitrate_bps: settings.bitrate_bps, fps: settings.fps, scale_pct: 100 };
            let mut failures = 0u32;
            let mut reported_name = "";
            while !ctl.stop.load(Ordering::Relaxed) {
                let switch = ctl.switch_display.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(index) = switch {
                    let mut next = opts.clone();
                    next.capture.display_index = index;
                    match open_source(&next) {
                        Ok(c) => {
                            capturer = c; // old capturer dropped (stopped) here
                            opts = next;
                            last = None;
                            encoder.request_keyframe();
                            tracing::info!(display = index, "switched display");
                        }
                        Err(e) => tracing::warn!(display = index, error = %format!("{e:#}"), "display switch failed"),
                    }
                }
                let pending = ctl.quality.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(q) = pending {
                    if q.bitrate_bps != quality.bitrate_bps {
                        encoder.set_bitrate(q.bitrate_bps);
                        settings.bitrate_bps = q.bitrate_bps;
                    }
                    tracing::info!(kbps = q.bitrate_bps / 1000, fps = q.fps, scale = q.scale_pct, "quality changed");
                    quality = q;
                }
                let frame = match capturer.next_frame(Duration::from_millis(200)) {
                    Some(f) => {
                        last = Some(f.clone());
                        f
                    }
                    None => {
                        let wants_key = ctl.keyframe.load(Ordering::Relaxed);
                        match &last {
                            Some(f) if wants_key || last_sent.elapsed() >= IDLE_REFRESH => {
                                Frame { captured_at: Instant::now(), ..f.clone() }
                            }
                            _ => continue,
                        }
                    }
                };
                let wants_key = ctl.keyframe.swap(false, Ordering::Relaxed);
                // Frame-rate cap from the adaptive controller (keyframe requests are never delayed).
                let min_gap = Duration::from_secs(1) / quality.fps.max(1);
                // Compare capture times (not encode completion), or encode time eats into the gap.
                if !wants_key
                    && last_encoded.is_some_and(|t: Instant| frame.captured_at.saturating_duration_since(t) < min_gap.mul_f32(0.9))
                {
                    continue;
                }
                if wants_key {
                    encoder.request_keyframe();
                }
                let scaled;
                let (pixels, w, h): (&[u8], usize, usize) = if quality.scale_pct < 100 {
                    let (w, h) = carson_capture::scaled_size(frame.width, frame.height, frame.width * quality.scale_pct as usize / 100);
                    scaled = carson_capture::downscale_bgra(&frame.bgra, frame.width, frame.height, w, h);
                    (&scaled, w, h)
                } else {
                    (&frame.bgra, frame.width, frame.height)
                };
                match encoder.encode_bgra(pixels, w, h) {
                    Ok(Some(encoded)) => {
                        failures = 0;
                        last_sent = Instant::now();
                        last_encoded = Some(frame.captured_at);
                        if encoder.name() != reported_name {
                            reported_name = encoder.name();
                            *ctl.encoder_name.lock().unwrap_or_else(|e| e.into_inner()) = reported_name;
                            tracing::info!(encoder = reported_name, "encoder ready");
                        }
                        let timed = TimedFrame { frame: encoded, captured_at: frame.captured_at };
                        if tx.blocking_send(timed).is_err() {
                            break; // session gone
                        }
                    }
                    Ok(None) => {} // rate control skipped this frame
                    Err(e) => {
                        // Self-heal: rebuild the encoder (fresh IDR) instead of ending the session.
                        failures += 1;
                        tracing::error!(error = %format!("{e:#}"), failures, "encoder failed");
                        if failures >= MAX_ENCODER_FAILURES {
                            tracing::error!("encoder keeps failing; stopping stream");
                            break;
                        }
                        match make_encoder(settings) {
                            Some(e) => encoder = e,
                            None => break,
                        }
                    }
                }
            }
            tracing::debug!("encode thread exiting");
        })
        .expect("spawn encode thread");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pattern_pipeline_produces_h264_and_honours_keyframe_requests() {
        let opts = MediaOptions {
            source: Source::TestPattern,
            ..Default::default()
        };
        let ctl = PipelineControl::default();
        let mut rx = spawn_pipeline(open_source(&opts).unwrap(), opts.clone(), ctl.clone());

        let first = rx.recv().await.unwrap();
        assert!(first.frame.keyframe, "stream must start with an IDR");
        assert_eq!((first.frame.width, first.frame.height), (1280, 720));
        let mut saw_p = false;
        for _ in 0..5 {
            saw_p |= !rx.recv().await.unwrap().frame.keyframe;
        }
        assert!(saw_p, "steady state should be P-frames");

        ctl.keyframe.store(true, Ordering::Relaxed);
        let mut got_key = false;
        for _ in 0..3 {
            got_key |= rx.recv().await.unwrap().frame.keyframe;
        }
        assert!(got_key, "keyframe request not honoured within 3 frames");

        ctl.stop.store(true, Ordering::Relaxed);
        while rx.recv().await.is_some() {}
    }
}
