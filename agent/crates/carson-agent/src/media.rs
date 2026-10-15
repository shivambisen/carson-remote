//! Capture → encode pipeline. Capture and encoding run on a dedicated OS thread (both are
//! blocking, CPU-bound work); encoded frames cross into async code over a small channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use carson_capture::{CaptureOptions, Capturer, Frame, TestPattern};
use carson_encode::{EncodedFrame, EncoderSettings, H264Encoder};
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
}

impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            source: Source::Screen,
            capture: CaptureOptions::default(),
            encoder: EncoderSettings::default(),
            input: InputMode::Native,
            clipboard: true,
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
}

/// How long the screen may be static before the last frame is re-sent. Keeps the stream
/// (and stats) alive and lets late keyframe requests be answered.
const IDLE_REFRESH: Duration = Duration::from_secs(1);

/// Starts the capture/encode thread. The thread exits when `ctl.stop` is set or the
/// receiver is dropped.
pub fn spawn_pipeline(
    mut capturer: Box<dyn Capturer>,
    mut opts: MediaOptions,
    ctl: PipelineControl,
) -> mpsc::Receiver<TimedFrame> {
    let settings = opts.encoder;
    let (tx, rx) = mpsc::channel::<TimedFrame>(4);
    std::thread::Builder::new()
        .name("carson-encode".into())
        .spawn(move || {
            let mut encoder = H264Encoder::new(settings);
            let mut last: Option<Frame> = None;
            let mut last_sent = Instant::now();
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
                if ctl.keyframe.swap(false, Ordering::Relaxed) {
                    encoder.request_keyframe();
                }
                match encoder.encode_bgra(&frame.bgra, frame.width, frame.height) {
                    Ok(Some(encoded)) => {
                        last_sent = Instant::now();
                        let timed = TimedFrame { frame: encoded, captured_at: frame.captured_at };
                        if tx.blocking_send(timed).is_err() {
                            break; // session gone
                        }
                    }
                    Ok(None) => {} // rate control skipped this frame
                    Err(e) => {
                        tracing::error!(error = %format!("{e:#}"), "encoder failed; stopping stream");
                        break;
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
