//! Synthetic moving pattern: lets CI and headless machines exercise the whole video
//! pipeline (and makes motion/latency easy to eyeball in the viewer).

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

    fn render(&self) -> Vec<u8> {
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
        let frame = Frame {
            width: self.width,
            height: self.height,
            bgra: self.render(),
            captured_at: Instant::now(),
        };
        self.n += 1;
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
