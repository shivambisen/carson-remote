//! Synthetic moving pattern: lets CI and headless machines exercise the whole video
//! pipeline, and carries a timestamp barcode for glass-to-glass latency measurement.
//!
//! Barcode (top-left, survives H.264): 56 blocks of 16x16 px in one row. Bits 0-7 are the
//! sync byte 0xB2, bits 8-55 the capture time in milliseconds since the UNIX epoch (low 48
//! bits), most significant bit first. White = 1, black = 0. The viewer decodes it.

use std::time::{Duration, Instant};

use crate::{Capturer, Frame};

pub struct TestPattern {
    width: usize,
    height: usize,
    interval: Duration,
    next_at: Instant,
    n: u64,
}

impl TestPattern {
    pub fn new(width: usize, height: usize, fps: u32) -> Self {
        Self {
            width: width & !1,
            height: height & !1,
            interval: Duration::from_secs(1) / fps.max(1),
            next_at: Instant::now(),
            n: 0,
        }
    }

    fn render(&self, stamp_ms: u64) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let mut buf = vec![0u8; w * h * 4];
        let shift = (self.n as usize * 4) % w.max(1);
        let bar_x = (self.n as usize * 8) % w.max(1);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let gx = ((x + shift) * 255 / w.max(1)) as u8;
                let gy = (y * 255 / h.max(1)) as u8;
                let on_bar = x.abs_diff(bar_x) < 12;
                buf[i] = if on_bar { 255 } else { gy }; // B
                buf[i + 1] = if on_bar { 255 } else { gx / 2 }; // G
                buf[i + 2] = if on_bar { 255 } else { gx }; // R
                buf[i + 3] = 255;
            }
        }
        draw_barcode(&mut buf, w, h, stamp_ms);
        buf
    }
}

impl Capturer for TestPattern {
    fn next_frame(&mut self, timeout: Duration) -> Option<Frame> {
        let now = Instant::now();
        if self.next_at > now {
            let wait = self.next_at - now;
            if wait > timeout {
                std::thread::sleep(timeout);
                return None;
            }
            std::thread::sleep(wait);
        }
        self.next_at = self.next_at.max(Instant::now() - self.interval) + self.interval;
        let captured_at = Instant::now();
        let frame = Frame {
            width: self.width,
            height: self.height,
            bgra: self.render(unix_ms()),
            captured_at,
        };
        self.n += 1;
        Some(frame)
    }
}

pub const BARCODE_SYNC: u8 = 0xB2;
pub const BARCODE_BLOCK: usize = 16;
pub const BARCODE_BITS: usize = 56;

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Draws sync byte + 48-bit timestamp as black/white blocks along the top-left edge.
pub fn draw_barcode(buf: &mut [u8], w: usize, h: usize, stamp_ms: u64) {
    let value = (u64::from(BARCODE_SYNC) << 48) | (stamp_ms & ((1 << 48) - 1));
    for bit in 0..BARCODE_BITS {
        let on = (value >> (BARCODE_BITS - 1 - bit)) & 1 == 1;
        let v = if on { 255 } else { 0 };
        for y in 0..BARCODE_BLOCK.min(h) {
            for x in bit * BARCODE_BLOCK..((bit + 1) * BARCODE_BLOCK).min(w) {
                let i = (y * w + x) * 4;
                buf[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
}

/// Reads a barcode back (tests; the viewer has its own decoder).
#[cfg(test)]
pub fn read_barcode(buf: &[u8], w: usize) -> Option<u64> {
    let mut value = 0u64;
    for bit in 0..BARCODE_BITS {
        let x = bit * BARCODE_BLOCK + BARCODE_BLOCK / 2;
        let i = ((BARCODE_BLOCK / 2) * w + x) * 4;
        value = (value << 1) | u64::from(buf.get(i + 1)? > &127);
    }
    ((value >> 48) as u8 == BARCODE_SYNC).then_some(value & ((1 << 48) - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barcode_round_trips_the_capture_time() {
        let mut p = TestPattern::new(1280, 720, 100);
        let before = unix_ms();
        let f = p.next_frame(Duration::from_secs(1)).unwrap();
        let stamp = read_barcode(&f.bgra, f.width).expect("barcode");
        assert!(stamp >= (before & ((1 << 48) - 1)) && stamp <= (unix_ms() & ((1 << 48) - 1)));
    }

    #[test]
    fn produces_frames_at_requested_rate_and_size() {
        let mut p = TestPattern::new(64, 37, 100);
        let start = Instant::now();
        let mut frames = Vec::new();
        while frames.len() < 5 {
            if let Some(f) = p.next_frame(Duration::from_secs(1)) {
                frames.push(f);
            }
        }
        assert!(
            start.elapsed() >= Duration::from_millis(35),
            "frames came too fast"
        );
        let f = &frames[0];
        assert_eq!((f.width, f.height), (64, 36), "dims are made even");
        assert_eq!(f.bgra.len(), 64 * 36 * 4);
        assert_ne!(frames[0].bgra, frames[1].bgra, "pattern must move");
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `cargo test -p carson-capture -- --ignored --nocapture render_speed`
    #[test]
    #[ignore]
    fn render_speed() {
        let p = TestPattern::new(1280, 720, 30);
        let t = Instant::now();
        for i in 0..30 {
            std::hint::black_box(p.render(i));
        }
        eprintln!(
            "render: {:.2} ms/frame",
            t.elapsed().as_secs_f64() * 1000.0 / 30.0
        );
    }
}
