//! Persistent agent settings (`agent.json`). Contains no secrets: the private key lives
//! in the key store.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Server origin, e.g. `https://carson.example.com`.
    pub server_url: String,
    pub device_id: String,
    pub name: String,
    /// Require a PIN (set with `glenley-agent unattended on --pin`) before a session starts.
    #[serde(default)]
    pub unattended: bool,
    /// Argon2id hash of the access PIN, if one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_hash: Option<String>,
    /// Ask "Allow?" at this computer before each connection. Off by default: only the account
    /// that owns this device can connect at all.
    #[serde(default)]
    pub ask_before_connect: bool,
    /// Show the small on-screen "being viewed" pill during a session. The tray icon always
    /// shows when someone is connected.
    #[serde(default)]
    pub show_indicator: bool,
}

impl AgentConfig {
    pub fn access_policy(&self) -> crate::consent::AccessPolicy {
        use crate::consent::AccessPolicy;
        match (&self.pin_hash, self.unattended) {
            (Some(hash), true) => AccessPolicy::Unattended {
                pin_hash: Some(hash.clone()),
            },
            _ if self.ask_before_connect => AccessPolicy::Attended,
            _ => AccessPolicy::Unattended { pin_hash: None },
        }
    }
}

/// Where the agent keeps its state. Override with `--config-dir` or `CARSON_AGENT_DIR`.
pub fn default_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CARSON_AGENT_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::config_dir().context("no per-user config directory on this platform")?;
    let dir = base.join("glenley");
    // The product used to be called carson: move its settings over once.
    let legacy = base.join("carson");
    if !dir.exists()
        && legacy.join("agent.json").is_file()
        && let Err(e) = std::fs::rename(&legacy, &dir)
    {
        tracing::warn!(error = %e, "could not move the old carson settings; using them in place");
        return Ok(legacy);
    }
    Ok(dir)
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join("agent.json")
}

pub fn load(dir: &Path) -> Result<Option<AgentConfig>> {
    let path = config_path(dir);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub fn save(dir: &Path, cfg: &AgentConfig) -> Result<()> {
    write_private(&config_path(dir), &serde_json::to_vec_pretty(cfg)?)
}

pub fn delete(dir: &Path) -> Result<()> {
    match std::fs::remove_file(config_path(dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Atomically writes `data` to `path`, readable only by the current user on Unix.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("write {}", tmp.display()))?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Normalizes a user-supplied server URL to its origin (`scheme://host[:port]`).
pub fn normalize_server_url(raw: &str) -> Result<String> {
    let url = url::Url::parse(raw.trim()).with_context(|| format!("invalid server URL {raw:?}"))?;
    match url.scheme() {
        "https" => {}
        "http" if is_local(&url) => {}
        "http" => anyhow::bail!("server URL must use https (http is only allowed for localhost)"),
        other => anyhow::bail!("unsupported URL scheme {other:?}"),
    }
    Ok(url.origin().ascii_serialization())
}

fn is_local(url: &url::Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_delete() {
        let dir = std::env::temp_dir().join(format!("carson-cfg-{}", std::process::id()));
        assert_eq!(load(&dir).unwrap(), None);
        let cfg = AgentConfig {
            server_url: "https://c.example".into(),
            device_id: "dev_1".into(),
            name: "Mac".into(),
            unattended: false,
            pin_hash: None,
            ask_before_connect: false,
            show_indicator: false,
        };
        save(&dir, &cfg).unwrap();
        assert_eq!(load(&dir).unwrap(), Some(cfg));
        delete(&dir).unwrap();
        assert_eq!(load(&dir).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn older_configs_connect_without_a_prompt_or_pill() {
        let old: AgentConfig =
            serde_json::from_str(r#"{"server_url":"https://c","device_id":"d","name":"n"}"#)
                .unwrap();
        assert_eq!(
            old.access_policy(),
            crate::consent::AccessPolicy::Unattended { pin_hash: None }
        );
        assert!(!old.show_indicator);
    }

    #[test]
    fn policy_follows_settings() {
        use crate::consent::AccessPolicy;
        let mut c: AgentConfig =
            serde_json::from_str(r#"{"server_url":"https://c","device_id":"d","name":"n"}"#)
                .unwrap();
        c.ask_before_connect = true;
        assert_eq!(c.access_policy(), AccessPolicy::Attended);
        c.unattended = true;
        c.pin_hash = Some("h".into());
        assert_eq!(
            c.access_policy(),
            AccessPolicy::Unattended {
                pin_hash: Some("h".into())
            },
            "a PIN wins over the prompt"
        );
    }

    #[test]
    fn server_url_normalization() {
        assert_eq!(
            normalize_server_url("https://c.example.com/some/path").unwrap(),
            "https://c.example.com"
        );
        assert_eq!(
            normalize_server_url("http://localhost:8080/").unwrap(),
            "http://localhost:8080"
        );
        assert!(normalize_server_url("http://c.example.com").is_err());
        assert!(normalize_server_url("ftp://c.example.com").is_err());
        assert!(normalize_server_url("not a url").is_err());
    }
}
