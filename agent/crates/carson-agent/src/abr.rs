//! Adaptive quality: turns the viewer's receive stats into a target bitrate, frame rate and
//! resolution. AIMD on bitrate (cut 30% on congestion, +10% after 3 clean seconds), with a
//! ladder that trades frame rate, then resolution, when the bitrate gets low.
//!
//! Only the *target* drops under congestion, so a static, cheap-to-encode screen keeps full
//! resolution on a good network.

/// What the encoder should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub bitrate_bps: u32,
    pub fps: u32,
    /// Resolution scale in percent of the captured size.
    pub scale_pct: u32,
}

pub struct Controller {
    max_bps: u32,
    min_bps: u32,
    max_fps: u32,
    bitrate: u32,
    baseline_rtt: Option<u32>,
    clean_reports: u32,
    current: Quality,
}

const MIN_BPS: u32 = 300_000;
const CONGESTED_LOSS: f32 = 0.03;
const CLEAN_LOSS: f32 = 0.01;
const RTT_RISE_MS: u32 = 150;
const CLEAN_REPORTS_BEFORE_INCREASE: u32 = 3;

impl Controller {
    pub fn new(max_bps: u32, max_fps: u32) -> Self {
        let mut c = Self {
            max_bps,
            min_bps: MIN_BPS.min(max_bps),
            max_fps,
            bitrate: max_bps,
            baseline_rtt: None,
            clean_reports: 0,
            current: Quality {
                bitrate_bps: max_bps,
                fps: max_fps,
                scale_pct: 100,
            },
        };
        c.current = c.ladder(c.current);
        c
    }

    pub fn current(&self) -> Quality {
        self.current
    }

    /// Feeds one report; returns the new quality if it changed.
    pub fn update(&mut self, rtt_ms: Option<u32>, loss: f32) -> Option<Quality> {
        if let Some(rtt) = rtt_ms {
            // Track the minimum RTT, but let it drift up 1 ms per report so a route change
            // (new, longer path) eventually becomes the new baseline.
            self.baseline_rtt = Some(match self.baseline_rtt {
                None => rtt,
                Some(b) if rtt < b => rtt,
                Some(b) => (b + 1).min(rtt),
            });
        }
        let rtt_rising =
            matches!((rtt_ms, self.baseline_rtt), (Some(r), Some(b)) if r > b + RTT_RISE_MS);
        if loss > CONGESTED_LOSS || rtt_rising {
            self.bitrate = ((self.bitrate as f32 * 0.7) as u32).max(self.min_bps);
            self.clean_reports = 0;
        } else if loss < CLEAN_LOSS {
            self.clean_reports += 1;
            if self.clean_reports >= CLEAN_REPORTS_BEFORE_INCREASE {
                self.bitrate = ((self.bitrate as f32 * 1.1) as u32).min(self.max_bps);
                self.clean_reports = 0;
            }
        }
        let next = self.ladder(self.current);
        (next != self.current).then(|| {
            self.current = next;
            next
        })
    }

    /// Frame rate and resolution for the current bitrate, with ~20% hysteresis on the way up.
    fn ladder(&self, prev: Quality) -> Quality {
        let b = self.bitrate;
        // (minimum bitrate for this rung, fps, scale)
        let rungs = [
            (1_500_000, self.max_fps, 100),
            (800_000, self.max_fps.min(30), 75),
            (500_000, self.max_fps.min(20), 75),
            (0, self.max_fps.min(15), 50),
        ];
        let mut idx = rungs
            .iter()
            .position(|r| b >= r.0)
            .unwrap_or(rungs.len() - 1);
        // Only climb a rung once comfortably above its threshold.
        let prev_idx = rungs
            .iter()
            .position(|r| r.1 == prev.fps && r.2 == prev.scale_pct)
            .unwrap_or(idx);
        if idx < prev_idx && b < rungs[idx].0 + rungs[idx].0 / 5 {
            idx = prev_idx;
        }
        Quality {
            bitrate_bps: b,
            fps: rungs[idx].1,
            scale_pct: rungs[idx].2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_full_quality() {
        let c = Controller::new(4_000_000, 30);
        assert_eq!(
            c.current(),
            Quality {
                bitrate_bps: 4_000_000,
                fps: 30,
                scale_pct: 100
            }
        );
    }

    #[test]
    fn backs_off_on_loss_and_recovers_slowly() {
        let mut c = Controller::new(4_000_000, 30);
        c.update(Some(20), 0.0);
        let q = c.update(Some(20), 0.10).expect("cut on loss");
        assert_eq!(q.bitrate_bps, 2_800_000);
        assert!(
            c.update(Some(20), 0.0).is_none() && c.update(Some(20), 0.0).is_none(),
            "no instant rebound"
        );
        assert_eq!(
            c.update(Some(20), 0.0).unwrap().bitrate_bps,
            3_080_000,
            "+10% after 3 clean reports"
        );
    }

    #[test]
    fn rising_rtt_counts_as_congestion() {
        let mut c = Controller::new(4_000_000, 30);
        c.update(Some(20), 0.0);
        assert!(
            c.update(Some(400), 0.0).is_some(),
            "queueing delay without loss still backs off"
        );
    }

    #[test]
    fn ladder_trades_fps_then_resolution_and_never_goes_below_minimum() {
        let mut c = Controller::new(4_000_000, 30);
        let mut seen = vec![c.current()];
        for _ in 0..20 {
            if let Some(q) = c.update(Some(20), 0.2) {
                seen.push(q);
            }
        }
        let last = *seen.last().unwrap();
        assert_eq!(last.bitrate_bps, MIN_BPS);
        assert_eq!((last.fps, last.scale_pct), (15, 50));
        assert!(
            seen.iter().any(|q| q.scale_pct == 75),
            "passes through the middle rungs"
        );

        // Recovery climbs back up to full quality.
        for _ in 0..200 {
            c.update(Some(20), 0.0);
        }
        assert_eq!(
            c.current(),
            Quality {
                bitrate_bps: 4_000_000,
                fps: 30,
                scale_pct: 100
            }
        );
    }

    #[test]
    fn respects_a_low_configured_maximum() {
        let c = Controller::new(600_000, 60);
        assert_eq!(c.current().scale_pct, 75);
        assert!(c.current().fps <= 20);
    }
}
