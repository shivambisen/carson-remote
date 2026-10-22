//! Minimal file logging. A service has no console and no meaningful user profile (it runs
//! before any user logs on), so this writes to a fixed machine-wide path instead of pulling
//! in the full tracing stack the agent uses — this binary stays small and easy to audit,
//! which matters more than usual for something that runs as SYSTEM.

use std::io::Write;
use std::path::PathBuf;

fn log_path() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("glenley").join("input-svc.log")
}

pub fn line(msg: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = writeln!(f, "{now} {msg}");
    }
}
