//! macOS capture via ScreenCaptureKit (macOS 14+).
#![allow(unsafe_code)] // reading CVPixelBuffer memory requires it; see SAFETY notes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use screencapturekit::cm::{CMSampleBufferExt, SCFrameStatus};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;

use crate::{
    CaptureOptions, Capturer, DisplayInfo, Frame, FrameSlot, PermissionDenied, scaled_size,
};

/// Lists displays in ScreenCaptureKit order (the order `display_index` refers to).
pub fn displays() -> Result<Vec<DisplayInfo>> {
    let content = SCShareableContent::get().map_err(|e| {
        anyhow::Error::new(PermissionDenied).context(format!("list displays: {e:?}"))
    })?;
    Ok(content
        .displays()
        .iter()
        .enumerate()
        .map(|(index, d)| {
            let cg = core_graphics::display::CGDisplay::new(d.display_id());
            let b = cg.bounds();
            let primary = cg.is_main();
            DisplayInfo {
                index,
                name: format!(
                    "Display {}{}",
                    index + 1,
                    if primary { " (main)" } else { "" }
                ),
                width: d.width(),
                height: d.height(),
                primary,
                bounds: (b.origin.x, b.origin.y, b.size.width, b.size.height),
            }
        })
        .collect())
}

pub struct ScreenCapturer {
    stream: SCStream,
    slot: Arc<FrameSlot>,
}

struct Handler {
    slot: Arc<FrameSlot>,
}

impl SCStreamOutputTrait for Handler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, kind: SCStreamOutputType) {
        if !matches!(kind, SCStreamOutputType::Screen) {
            return;
        }
        // Idle frames mean "nothing changed"; skip them so the encoder can idle too.
        if !matches!(sample.frame_status(), Some(SCFrameStatus::Complete) | None) {
            return;
        }
        let Some(pixels) = sample.pixel_buffer() else {
            return;
        };
        let Ok(guard) = pixels.lock(CVPixelBufferLockFlags::READ_ONLY) else {
            return;
        };
        let (w, h, stride) = (guard.width(), guard.height(), guard.bytes_per_row());
        if w == 0 || h == 0 || stride < w * 4 {
            return;
        }
        let base = guard.base_address();
        if base.is_null() {
            return;
        }
        // SAFETY: the buffer is locked read-only for the guard's lifetime, and a BGRA
        // CVPixelBuffer holds `bytes_per_row * height` bytes starting at its base address.
        let src = unsafe { std::slice::from_raw_parts(base, stride * h) };
        let (w, h) = (w & !1, h & !1);
        let mut bgra = Vec::with_capacity(w * h * 4);
        for row in src.chunks_exact(stride).take(h) {
            bgra.extend_from_slice(&row[..w * 4]);
        }
        self.slot.put(Frame {
            width: w,
            height: h,
            bgra,
            captured_at: Instant::now(),
        });
    }
}

impl ScreenCapturer {
    pub fn start(opts: &CaptureOptions) -> Result<Self> {
        // Listing shareable content is the first call that needs Screen Recording permission.
        let content = SCShareableContent::get().map_err(|e| {
            anyhow::Error::new(PermissionDenied).context(format!("list displays: {e:?}"))
        })?;
        let displays = content.displays();
        let display = displays
            .get(opts.display_index)
            .or_else(|| displays.first())
            .context("no displays found")?;
        let (width, height) = scaled_size(
            display.width() as usize,
            display.height() as usize,
            opts.max_width,
        );
        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[])
            .build()
            .map_err(|e| anyhow::anyhow!("content filter: {e:?}"))?;
        let config = SCStreamConfiguration::new()
            .with_width(width as u32)
            .with_height(height as u32)
            .with_pixel_format(PixelFormat::BGRA)
            .with_shows_cursor(opts.show_cursor)
            .with_fps(opts.fps)
            .with_queue_depth(3);
        let slot = Arc::new(FrameSlot::default());
        let mut stream =
            SCStream::new(&filter, &config).map_err(|e| anyhow::anyhow!("create stream: {e:?}"))?;
        stream
            .add_output_handler(
                Handler {
                    slot: Arc::clone(&slot),
                },
                SCStreamOutputType::Screen,
            )
            .map_err(|e| anyhow::anyhow!("add output handler: {e:?}"))?;
        stream.start_capture().map_err(|e| {
            anyhow::Error::new(PermissionDenied).context(format!("start capture: {e:?}"))
        })?;
        Ok(Self { stream, slot })
    }
}

impl Capturer for ScreenCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Option<Frame> {
        self.slot.take(timeout)
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        let _ = self.stream.stop_capture();
    }
}
