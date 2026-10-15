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
}

/// Where the agent keeps its state. Override with `--config-dir` or `CARSON_AGENT_DIR`.
pub fn default_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CARSON_AGENT_DIR") {
        return Ok(PathBuf::from(dir));
    }
    dirs::config_dir()
        .map(|d| d.join("carson"))
        .context("no per-user config directory on this platform")
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
        };
        save(&dir, &cfg).unwrap();
        assert_eq!(load(&dir).unwrap(), Some(cfg));
        delete(&dir).unwrap();
        assert_eq!(load(&dir).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
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
