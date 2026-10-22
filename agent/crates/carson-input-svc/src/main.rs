//! SYSTEM-level Windows service for glenley. It exists for one reason: a normal process
//! (the agent) cannot inject input into another, more-privileged window (UIPI) or the
//! secure desktop (UAC prompts, the lock screen); SYSTEM can. The agent talks to this
//! service over a local, session-checked named pipe instead of calling SendInput itself,
//! whenever the service is installed and running — see `carson_input::native_backend`.
//!
//! Kept small and dependency-light on purpose: this runs as SYSTEM, so its code is worth
//! being easy to read in full.

#[cfg(target_os = "windows")]
mod log;
#[cfg(target_os = "windows")]
mod pipe_server;
#[cfg(target_os = "windows")]
mod service;

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        service::main()
    }
    #[cfg(not(target_os = "windows"))]
    {
        anyhow::bail!("glenley-input-svc only runs on Windows")
    }
}
