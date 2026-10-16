//! Who may connect without someone at the computer, and how that is proven.
//!
//! - **Attended** (default): every session needs a click on "Allow" at the host.
//! - **Unattended**: turned on locally (`glenley-agent unattended on`), optionally with a PIN.
//!   The PIN is checked here, on the host, after the WebRTC connection is up, so it only
//!   ever travels end-to-end encrypted. The server never sees it. Only an Argon2id hash is stored.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

pub const MAX_PIN_ATTEMPTS: u32 = 5;
pub const PIN_LOCKOUT: Duration = Duration::from_secs(15 * 60);
/// How long the person at the computer has to answer the prompt.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(30);

/// How sessions are authorized on this host.
#[derive(Debug, Clone, PartialEq)]
pub enum AccessPolicy {
    Attended,
    Unattended {
        pin_hash: Option<String>,
    },
    /// Test pattern / automated tests: no prompt, no banner (nothing real is shared).
    #[cfg_attr(not(test), allow(dead_code))]
    Automatic,
}

pub fn validate_pin(pin: &str) -> Result<()> {
    if !(6..=10).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_digit()) {
        bail!("PIN must be 6 to 10 digits");
    }
    Ok(())
}

pub fn hash_pin(pin: &str) -> Result<String> {
    validate_pin(pin)?;
    Ok(Argon2::default()
        .hash_password(pin.as_bytes())
        .map_err(|e| anyhow!("hash PIN: {e}"))?
        .to_string())
}

fn verify_pin(pin: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        Argon2::default()
            .verify_password(pin.as_bytes(), &h)
            .is_ok()
    })
}

#[derive(Debug, PartialEq)]
pub enum PinOutcome {
    Accepted,
    Rejected { attempts_left: u32 },
    Locked { retry_after: Duration },
}

/// Counts failed PIN attempts across sessions; locks out after [`MAX_PIN_ATTEMPTS`].
pub struct PinGuard {
    failures: u32,
    locked_until: Option<Instant>,
    now: fn() -> Instant,
}

impl Default for PinGuard {
    fn default() -> Self {
        Self {
            failures: 0,
            locked_until: None,
            now: Instant::now,
        }
    }
}

impl PinGuard {
    /// Remaining lockout, if any (checked before even starting a session).
    pub fn locked_for(&mut self) -> Option<Duration> {
        let now = (self.now)();
        match self.locked_until {
            Some(t) if t > now => Some(t - now),
            Some(_) => {
                self.locked_until = None;
                self.failures = 0;
                None
            }
            None => None,
        }
    }

    /// Attempts left before lockout.
    pub fn attempts_left(&mut self) -> u32 {
        if self.locked_for().is_some() {
            0
        } else {
            MAX_PIN_ATTEMPTS - self.failures
        }
    }

    /// Checks `pin` against `hash` (slow by design: run off the async runtime).
    pub fn check(&mut self, pin: &str, hash: &str) -> PinOutcome {
        if let Some(retry_after) = self.locked_for() {
            return PinOutcome::Locked { retry_after };
        }
        if validate_pin(pin).is_ok() && verify_pin(pin, hash) {
            self.failures = 0;
            return PinOutcome::Accepted;
        }
        self.failures += 1;
        if self.failures >= MAX_PIN_ATTEMPTS {
            self.locked_until = Some((self.now)() + PIN_LOCKOUT);
            return PinOutcome::Locked {
                retry_after: PIN_LOCKOUT,
            };
        }
        PinOutcome::Rejected {
            attempts_left: MAX_PIN_ATTEMPTS - self.failures,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn pin_format() {
        assert!(validate_pin("123456").is_ok());
        assert!(validate_pin("1234567890").is_ok());
        for bad in ["12345", "12345678901", "12a456", "", "١٢٣٤٥٦"] {
            assert!(validate_pin(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn hash_is_argon2id_and_salted() {
        let a = hash_pin("246810").unwrap();
        let b = hash_pin("246810").unwrap();
        assert!(a.starts_with("$argon2id$"), "{a}");
        assert_ne!(a, b, "salt must differ");
        assert!(verify_pin("246810", &a) && verify_pin("246810", &b));
        assert!(!verify_pin("246811", &a));
        assert!(!verify_pin("246810", "not a hash"));
    }

    static NOW: Mutex<Option<Instant>> = Mutex::new(None);
    fn fake_now() -> Instant {
        NOW.lock().unwrap().expect("clock set")
    }

    #[test]
    fn lockout_after_five_failures_then_expires() {
        let hash = hash_pin("135790").unwrap();
        let start = Instant::now();
        *NOW.lock().unwrap() = Some(start);
        let mut g = PinGuard {
            now: fake_now,
            ..Default::default()
        };

        assert_eq!(
            g.check("000000", &hash),
            PinOutcome::Rejected { attempts_left: 4 }
        );
        assert_eq!(
            g.check("135790", &hash),
            PinOutcome::Accepted,
            "success resets the count"
        );
        for left in (1..=4).rev() {
            assert_eq!(
                g.check("000000", &hash),
                PinOutcome::Rejected {
                    attempts_left: left
                }
            );
        }
        assert_eq!(
            g.check("000000", &hash),
            PinOutcome::Locked {
                retry_after: PIN_LOCKOUT
            }
        );
        assert!(
            matches!(g.check("135790", &hash), PinOutcome::Locked { .. }),
            "even the right PIN is refused while locked"
        );

        *NOW.lock().unwrap() = Some(start + PIN_LOCKOUT + Duration::from_secs(1));
        assert_eq!(g.locked_for(), None);
        assert_eq!(g.check("135790", &hash), PinOutcome::Accepted);
    }
}
