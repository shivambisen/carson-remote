//! Host-side UI: the consent prompt and the always-on-top "being controlled" banner.
//!
//! Both run in a helper process (`glenley-agent ui …`) spawned from `run`, so the GUI owns its
//! own main thread (AppKit requires it) and a hung or crashed UI cannot take the agent down.
//! The agent fails closed: if the prompt cannot be shown the session is refused, and if the
//! banner cannot be shown (or is dismissed with Disconnect) the session ends.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::{Child, ChildStdin, Command};

/// Exit code of `ui prompt` when the person clicked Allow.
const EXIT_ALLOW: i32 = 0;
/// Exit code of `ui prompt` when they clicked Deny (or closed it).
const EXIT_DENY: i32 = 10;
/// Exit code when the parent agent went away (stdin closed).
const EXIT_ORPHANED: i32 = 3;

#[derive(Debug, PartialEq)]
pub enum Consent {
    Allowed,
    Denied,
    TimedOut,
    /// No prompt could be shown (no GUI session, unsupported OS, helper crashed).
    Unavailable(String),
}

fn helper(args: &[&str]) -> Result<Command> {
    let exe = std::env::current_exe().context("locate glenley-agent executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg("ui")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    Ok(cmd)
}

/// Asks the person at this computer. Anything but an explicit Allow is a no.
pub async fn ask_consent(title: &str, message: &str, timeout: Duration) -> Consent {
    let mut child = match helper(&["prompt", "--title", title, "--message", message])
        .and_then(|mut c| Ok(c.spawn()?))
    {
        Ok(c) => c,
        Err(e) => return Consent::Unavailable(format!("{e:#}")),
    };
    match tokio::time::timeout(timeout, child.wait()).await {
        Err(_) => {
            let _ = child.kill().await;
            Consent::TimedOut
        }
        Ok(Ok(status)) => match status.code() {
            Some(EXIT_ALLOW) => Consent::Allowed,
            Some(EXIT_DENY) => Consent::Denied,
            other => Consent::Unavailable(format!("prompt helper exited with {other:?}")),
        },
        Ok(Err(e)) => Consent::Unavailable(e.to_string()),
    }
}

/// The on-screen "being controlled" banner. Dropping it removes it.
pub struct Banner {
    child: Child,
    /// Held open; the helper exits when it closes (agent crashed or session over).
    _stdin: Option<ChildStdin>,
}

impl Banner {
    pub fn show(text: &str) -> Result<Self> {
        let mut child = helper(&["banner", "--text", text])?
            .spawn()
            .context("start banner helper")?;
        let stdin = child.stdin.take();
        Ok(Self {
            child,
            _stdin: stdin,
        })
    }

    /// Resolves when the banner goes away on its own: the person clicked Disconnect
    /// (or the helper died, which is treated the same: no banner, no session).
    pub async fn closed(&mut self) {
        let _ = self.child.wait().await;
    }
}

/// Shows an informational window (e.g. the connection code) without waiting for it.
pub fn show_info(title: &str, message: &str) {
    let spawned = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(["ui", "info", "--title", title, "--message", message])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not show the info window");
    }
}

/// Entry point of the hidden `ui` subcommand.
pub fn helper_main(args: &[String]) -> Result<()> {
    let get = |flag: &str| -> Result<&str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
            .with_context(|| format!("missing {flag}"))
    };
    match args.first().map(String::as_str) {
        Some("prompt") => {
            let allowed = carson_hostui::prompt(get("--title")?, get("--message")?)?;
            std::process::exit(if allowed { EXIT_ALLOW } else { EXIT_DENY });
        }
        Some("info") => carson_hostui::info(get("--title")?, get("--message")?),
        Some("banner") => {
            // If the agent goes away, so does the banner.
            std::thread::spawn(|| {
                let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
                std::process::exit(EXIT_ORPHANED);
            });
            carson_hostui::banner(get("--text")?)
        }
        _ => bail!(
            "usage: glenley-agent ui prompt --title T --message M | ui info --title T --message M | ui banner --text T"
        ),
    }
}
