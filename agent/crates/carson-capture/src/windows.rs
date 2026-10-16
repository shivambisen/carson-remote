//! Windows capture via Windows Graphics Capture (Windows 10 1903+), through `windows-capture`.
#![allow(unsafe_code)] // GetMonitorInfoW for monitor bounds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WgcFrame;
use windows_capture::graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl};
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::{CaptureOptions, Capturer, DisplayInfo, Frame, FrameSlot, downscale_bgra, scaled_size};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn monitor_bounds(m: &Monitor) -> Option<(f64, f64, f64, f64)> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: the HMONITOR comes from a live enumeration and `info` is a valid, sized struct.
    let ok = unsafe { GetMonitorInfoW(HMONITOR(m.as_raw_hmonitor()), &mut info).as_bool() };
    ok.then(|| {
        let r = info.rcMonitor;
        (
            f64::from(r.left),
            f64::from(r.top),
            f64::from(r.right - r.left),
            f64::from(r.bottom - r.top),
        )
    })
}

pub fn displays() -> Result<Vec<DisplayInfo>> {
    let primary = Monitor::primary().ok().map(|m| m.as_raw_hmonitor());
    let monitors = Monitor::enumerate().map_err(|e| anyhow!("enumerate monitors: {e}"))?;
    Ok(monitors
        .iter()
        .enumerate()
        .map(|(index, m)| {
            let width = m.width().unwrap_or(0);
            let height = m.height().unwrap_or(0);
            let is_primary = primary == Some(m.as_raw_hmonitor());
            let name = m
                .name()
                .or_else(|_| m.device_name())
                .unwrap_or_else(|_| format!("Display {}", index + 1));
            DisplayInfo {
                index,
                name: if is_primary {
                    format!("{name} (main)")
                } else {
                    name
                },
                width,
                height,
                primary: is_primary,
                bounds: monitor_bounds(m).unwrap_or((
                    0.0,
                    0.0,
                    f64::from(width),
                    f64::from(height),
                )),
            }
        })
        .collect())
}

struct Handler {
    slot: Arc<FrameSlot>,
    max_width: usize,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = (Arc<FrameSlot>, usize);
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let (slot, max_width) = ctx.flags;
        Ok(Self {
            slot,
            max_width,
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WgcFrame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let fb = frame.buffer()?;
        let (w, h) = (fb.width() as usize, fb.height() as usize);
        let tight = fb.as_nopadding_buffer(&mut self.scratch);
        let (dw, dh) = scaled_size(w, h, self.max_width);
        let bgra = if (dw, dh) == (w, h) {
            tight.to_vec()
        } else {
            downscale_bgra(tight, w, h, dw, dh)
        };
        self.slot.put(Frame {
            width: dw,
            height: dh,
            bgra,
            captured_at: Instant::now(),
        });
        Ok(())
    }
}

pub struct ScreenCapturer {
    control: Option<CaptureControl<Handler, BoxError>>,
    slot: Arc<FrameSlot>,
}

impl ScreenCapturer {
    pub fn start(opts: &CaptureOptions) -> Result<Self> {
        let monitor = Monitor::from_index(opts.display_index + 1) // windows-capture indexes from 1
            .or_else(|_| Monitor::primary())
            .map_err(|e| anyhow!("find monitor: {e}"))?;
        let slot = Arc::new(FrameSlot::default());
        // windows-capture refuses to start if any non-default setting is unavailable on this
        // Windows build, so each optional one is used only where supported: cursor toggle
        // (Windows 10 2004+), hiding the yellow border (Windows 11), minimum update interval
        // (Windows 11 24H2). Without the last one the encoder's own fps cap still applies.
        let supported = |check: fn() -> Result<
            bool,
            windows_capture::graphics_capture_api::Error,
        >| { check().unwrap_or(false) };
        let cursor = if !supported(GraphicsCaptureApi::is_cursor_settings_supported) {
            CursorCaptureSettings::Default
        } else if opts.show_cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        };
        let border = if supported(GraphicsCaptureApi::is_border_settings_supported) {
            DrawBorderSettings::WithoutBorder
        } else {
            DrawBorderSettings::Default
        };
        let interval = if supported(GraphicsCaptureApi::is_minimum_update_interval_supported) {
            MinimumUpdateIntervalSettings::Custom(Duration::from_secs(1) / opts.fps.max(1))
        } else {
            MinimumUpdateIntervalSettings::Default
        };
        tracing::info!(
            ?cursor,
            ?border,
            ?interval,
            "Windows Graphics Capture settings"
        );
        let settings = Settings::new(
            monitor,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            (Arc::clone(&slot), opts.max_width),
        );
        let control = Handler::start_free_threaded(settings)
            .map_err(|e| anyhow!("start capture: {e}"))
            .context("Windows Graphics Capture")?;
        Ok(Self {
            control: Some(control),
            slot,
        })
    }
}

impl Capturer for ScreenCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Option<Frame> {
        self.slot.take(timeout)
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }
    }
}
