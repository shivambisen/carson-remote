//! Screen capture for carson-agent.
//!
//! Every source implements [`Capturer`]. Frames are tightly packed BGRA. Sources keep only
//! the newest frame (a slow encoder skips frames instead of building latency).

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod macos;
mod test_pattern;

pub use test_pattern::TestPattern;

/// One captured frame, BGRA, `width * 4` bytes per row.
#[derive(Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
    pub captured_at: Instant,
}

#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub fps: u32,
    /// Output is downscaled (keeping aspect ratio) to at most this width.
    pub max_width: usize,
    /// Index into the list of displays (0 = main).
    pub display_index: usize,
    pub show_cursor: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            fps: 30,
            max_width: 1920,
            display_index: 0,
            show_cursor: true,
        }
    }
}

/// A source of frames. Dropping it stops capture.
pub trait Capturer: Send {
    /// Waits up to `timeout` for a frame newer than the last one returned.
    /// Returns `None` on timeout (e.g. the screen did not change).
    fn next_frame(&mut self, timeout: Duration) -> Option<Frame>;
}

/// The OS denied screen recording (macOS: System Settings → Privacy & Security → Screen Recording).
#[derive(Debug)]
pub struct PermissionDenied;

impl std::fmt::Display for PermissionDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "screen recording permission is not granted (System Settings → Privacy & Security → Screen Recording)",
        )
    }
}

impl std::error::Error for PermissionDenied {}

/// Starts capturing a display.
pub fn screen(opts: &CaptureOptions) -> anyhow::Result<Box<dyn Capturer>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::ScreenCapturer::start(opts)?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = opts;
        anyhow::bail!("screen capture is not implemented on this OS yet (Windows lands in M4)")
    }
}

/// Fits `width x height` into `max_width`, preserving aspect ratio, with even dimensions
/// (H.264 4:2:0 requires them).
pub fn scaled_size(width: usize, height: usize, max_width: usize) -> (usize, usize) {
    let (w, h) = if width > max_width && width > 0 {
        (max_width, height * max_width / width)
    } else {
        (width, height)
    };
    ((w & !1).max(2), (h & !1).max(2))
}

/// Holds the newest frame; producers overwrite, the consumer takes.
#[derive(Default)]
pub(crate) struct FrameSlot {
    frame: Mutex<Option<Frame>>,
    ready: Condvar,
}

impl FrameSlot {
    pub(crate) fn put(&self, frame: Frame) {
        *self.frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        self.ready.notify_one();
    }

    pub(crate) fn take(&self, timeout: Duration) -> Option<Frame> {
        let guard = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        let (mut guard, _) = self
            .ready
            .wait_timeout_while(guard, timeout, |f| f.is_none())
            .unwrap_or_else(|e| e.into_inner());
        guard.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_size_keeps_aspect_and_even_dims() {
        assert_eq!(scaled_size(1920, 1080, 1920), (1920, 1080));
        assert_eq!(scaled_size(3024, 1964, 1920), (1920, 1246));
        assert_eq!(scaled_size(1511, 983, 1920), (1510, 982));
        assert_eq!(scaled_size(1, 1, 1920), (2, 2));
    }

    #[test]
    fn slot_keeps_only_newest_frame() {
        let slot = FrameSlot::default();
        let frame = |w| Frame {
            width: w,
            height: 2,
            bgra: vec![0; w * 8],
            captured_at: Instant::now(),
        };
        slot.put(frame(2));
        slot.put(frame(4));
        assert_eq!(slot.take(Duration::ZERO).map(|f| f.width), Some(4));
        assert!(slot.take(Duration::from_millis(10)).is_none());
    }

    #[test]
    fn slot_wakes_waiting_consumer() {
        let slot = std::sync::Arc::new(FrameSlot::default());
        let producer = std::sync::Arc::clone(&slot);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            producer.put(Frame {
                width: 2,
                height: 2,
                bgra: vec![0; 16],
                captured_at: Instant::now(),
            });
        });
        assert!(slot.take(Duration::from_secs(2)).is_some());
        t.join().unwrap();
    }
}
