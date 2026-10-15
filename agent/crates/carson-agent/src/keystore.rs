//! Storage for the device's Ed25519 private key (a 32-byte seed).
//!
//! macOS: Keychain. Windows: Credential Manager. Linux: a 0600 file next to the agent
//! config (Secret Service needs a desktop session; revisit when packaging in M8).

use anyhow::Result;

pub type Seed = [u8; 32];

pub trait KeyStore: Send + Sync {
    fn load(&self) -> Result<Option<Seed>>;
    fn save(&self, seed: &Seed) -> Result<()>;
    fn delete(&self) -> Result<()>;
}

fn to_seed(bytes: &[u8]) -> Result<Seed> {
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("stored device key has wrong length {}", bytes.len()))
}

/// OS keychain entry, keyed per server so one machine can pair with several servers.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct OsKeychain {
    account: String,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl OsKeychain {
    const SERVICE: &'static str = "carson-agent";

    pub fn new(server_origin: &str) -> Result<Self> {
        static INIT: std::sync::OnceLock<std::result::Result<(), String>> =
            std::sync::OnceLock::new();
        INIT.get_or_init(|| {
            #[cfg(target_os = "macos")]
            let store = apple_native_keyring_store::keychain::Store::new();
            #[cfg(target_os = "windows")]
            let store = windows_native_keyring_store::Store::new();
            store
                .map(|s| keyring_core::set_default_store(s))
                .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|e| anyhow::anyhow!("open OS keychain: {e}"))?;
        Ok(Self {
            account: format!("device-key:{server_origin}"),
        })
    }

    fn entry(&self) -> Result<keyring_core::Entry> {
        Ok(keyring_core::Entry::new(Self::SERVICE, &self.account)?)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl KeyStore for OsKeychain {
    fn load(&self) -> Result<Option<Seed>> {
        match self.entry()?.get_secret() {
            Ok(bytes) => Ok(Some(to_seed(&bytes)?)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn save(&self, seed: &Seed) -> Result<()> {
        Ok(self.entry()?.set_secret(seed)?)
    }
    fn delete(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Key file readable only by the current user.
pub struct FileStore {
    path: std::path::PathBuf,
}

impl FileStore {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl KeyStore for FileStore {
    fn load(&self) -> Result<Option<Seed>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(Some(to_seed(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn save(&self, seed: &Seed) -> Result<()> {
        crate::config::write_private(&self.path, seed)
    }
    fn delete(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// The platform's default key store for a given server.
/// `CARSON_AGENT_KEYSTORE=file` forces the file store (headless CI, end-to-end tests).
pub fn default_store(
    server_origin: &str,
    config_dir: &std::path::Path,
) -> Result<Box<dyn KeyStore>> {
    let file = || Box::new(FileStore::new(config_dir.join("device.key")));
    if std::env::var("CARSON_AGENT_KEYSTORE").as_deref() == Ok("file") {
        return Ok(file());
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Ok(Box::new(OsKeychain::new(server_origin)?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = server_origin;
        Ok(file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trip() {
        let dir = std::env::temp_dir().join(format!("carson-ks-{}", std::process::id()));
        let store = FileStore::new(dir.join("device.key"));
        assert_eq!(store.load().unwrap(), None);
        store.save(&[7; 32]).unwrap();
        assert_eq!(store.load().unwrap(), Some([7; 32]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("device.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "key file must be private");
        }
        store.delete().unwrap();
        store.delete().unwrap(); // idempotent
        assert_eq!(store.load().unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_corrupt_key() {
        assert!(to_seed(&[1, 2, 3]).is_err());
    }
}
