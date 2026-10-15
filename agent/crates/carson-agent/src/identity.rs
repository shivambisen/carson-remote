//! The device's Ed25519 identity. The private key never leaves the machine; the server
//! only ever sees the public key and signatures over its challenges.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer, SigningKey};

use crate::keystore::Seed;

pub struct DeviceKey {
    signing: SigningKey,
}

impl DeviceKey {
    /// Generates a new key from the OS CSPRNG.
    pub fn generate() -> anyhow::Result<Self> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("OS random generator: {e}"))?;
        Ok(Self::from_seed(&seed))
    }

    pub fn from_seed(seed: &Seed) -> Self {
        Self {
            signing: SigningKey::from_bytes(seed),
        }
    }

    pub fn seed(&self) -> Seed {
        self.signing.to_bytes()
    }

    /// Public key as base64url without padding (the wire format).
    pub fn public_key_b64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.signing.verifying_key().as_bytes())
    }

    /// Signs the server's challenge for `device_id` (see `proto/device-auth.json`).
    pub fn sign_auth(&self, device_id: &str, nonce: &str) -> String {
        let msg = carson_proto::device_auth_message(device_id, nonce);
        URL_SAFE_NO_PAD.encode(self.signing.sign(&msg).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_shared_fixture() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../../../proto/device-auth.json")).unwrap();
        let hex = v["seed_hex"].as_str().unwrap();
        let mut seed = [0u8; 32];
        for (i, b) in seed.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let key = DeviceKey::from_seed(&seed);
        assert_eq!(key.public_key_b64(), v["public_key"].as_str().unwrap());
        let sig = key.sign_auth(
            v["device_id"].as_str().unwrap(),
            v["nonce"].as_str().unwrap(),
        );
        assert_eq!(sig, v["signature"].as_str().unwrap());
    }

    #[test]
    fn generated_keys_differ_and_round_trip() {
        let a = DeviceKey::generate().unwrap();
        let b = DeviceKey::generate().unwrap();
        assert_ne!(a.public_key_b64(), b.public_key_b64());
        assert_eq!(
            DeviceKey::from_seed(&a.seed()).public_key_b64(),
            a.public_key_b64()
        );
    }
}
