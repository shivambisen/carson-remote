//! Jittered exponential backoff for reconnects (design doc §12: cap 10 s).

use std::time::Duration;

pub struct Backoff {
    base: Duration,
    cap: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap,
            attempt: 0,
        }
    }

    /// Next delay: "equal jitter", uniformly in [d/2, d] with d = min(cap, base·2^attempt).
    /// Never near zero (no hot loop), and spreads reconnect storms after a server restart.
    pub fn next_delay(&mut self) -> Duration {
        let exp = self
            .base
            .saturating_mul(1u32.checked_shl(self.attempt).unwrap_or(u32::MAX));
        let d = exp.min(self.cap);
        self.attempt = self.attempt.saturating_add(1);
        let half = d / 2;
        half + half.mul_f64(random_unit())
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

fn random_unit() -> f64 {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() {
        return 0.5;
    }
    (u64::from_le_bytes(b) >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_then_caps_with_jitter_bounds() {
        let base = Duration::from_millis(500);
        let cap = Duration::from_secs(10);
        let mut b = Backoff::new(base, cap);
        for attempt in 0..20u32 {
            let d = base
                .saturating_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
                .min(cap);
            let got = b.next_delay();
            assert!(
                got >= d / 2 && got <= d,
                "attempt {attempt}: {got:?} not in [{:?}, {d:?}]",
                d / 2
            );
        }
    }

    #[test]
    fn reset_restarts_from_base() {
        let mut b = Backoff::new(Duration::from_millis(500), Duration::from_secs(10));
        for _ in 0..10 {
            b.next_delay();
        }
        b.reset();
        assert!(b.next_delay() <= Duration::from_millis(500));
    }
}
