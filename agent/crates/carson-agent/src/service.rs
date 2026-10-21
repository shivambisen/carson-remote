//! Runs glenley-agent automatically at login, in the user's session (screen capture and input
//! need the logged-in desktop, so this is a per-user agent, not a system daemon).
//!
//! - macOS: LaunchAgent `~/Library/LaunchAgents/in.glenley.agent.plist`
//! - Windows: Task Scheduler task "glenley-agent" (at logon, normal privileges)
//! - Linux: systemd user unit `~/.config/systemd/user/glenley-agent.service`
//!
//! On macOS and Windows it starts the tray app (`glenley app`) when that binary is installed,
//! otherwise (and on Linux) `glenley-agent run --service`. Errors that a restart cannot fix
//! (device removed, another instance running) and Quit exit with status 0 so the service
//! manager stops instead of restarting in a loop; crashes still restart it.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const LABEL: &str = "in.glenley.agent";

/// A helper program started without a console window. On Windows the app is a GUI program, so
/// every console tool it runs (schtasks, rundll32) would otherwise flash a terminal window.
fn quiet(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Where the service writes its log.
pub fn log_path(dir: &Path) -> PathBuf {
    dir.join("agent.log")
}

/// What the login item starts: the program and its trailing arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

/// The tray app if it is installed (this binary, or `glenley` next to `glenley-agent`), else
/// the headless agent.
fn launch() -> Result<Launch> {
    let exe = std::env::current_exe().context("locate glenley-agent executable")?;
    // The tray app is "<brand>" and the background binary is "<brand>-agent" (the Windows installer
    // can rename both to a chosen name), so derive the pair from this binary's own name rather than
    // a fixed "glenley". Running as the tray app: use it; running as the agent: prefer its tray app.
    let tray_supported = cfg!(any(target_os = "macos", target_os = "windows"));
    let stem = exe.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let (is_app, sibling) = match stem.strip_suffix("-agent") {
        Some(app_stem) => {
            let name = match exe.extension().and_then(|e| e.to_str()) {
                Some(ext) => format!("{app_stem}.{ext}"),
                None => app_stem.to_owned(),
            };
            (false, exe.with_file_name(name))
        }
        None => (true, exe.clone()),
    };
    Ok(match () {
        _ if tray_supported && is_app => Launch {
            exe,
            args: vec!["app".into(), "--background".into()],
        },
        _ if tray_supported && sibling.is_file() => Launch {
            exe: sibling,
            args: vec!["app".into(), "--background".into()],
        },
        _ => Launch {
            exe,
            args: vec!["run".into(), "--service".into()],
        },
    })
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.output().with_context(|| format!("run {cmd:?}"))?;
    if !out.status.success() {
        bail!(
            "{cmd:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home directory")?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

/// XML-escapes a value for the plist.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The LaunchAgent definition (pure; unit-tested on every OS).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn launch_agent_plist(launch: &Launch, config_dir: &Path) -> String {
    let log = log_path(config_dir);
    let args: String = launch
        .args
        .iter()
        .map(|a| format!("\n    <string>{}</string>", xml(a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--config-dir</string><string>{dir}</string>
    <string>--log-file</string><string>{log}</string>{args}
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>15</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
</dict>
</plist>
"#,
        exe = xml(&launch.exe.display().to_string()),
        dir = xml(&config_dir.display().to_string()),
        log = xml(&log.display().to_string()),
    )
}

/// The systemd user unit (pure; unit-tested on every OS).
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
pub fn systemd_unit(launch: &Launch, config_dir: &Path) -> String {
    let q = |p: &Path| format!("\"{}\"", p.display().to_string().replace('"', "\\\""));
    format!(
        "[Unit]\nDescription=glenley remote desktop agent\nAfter=graphical-session.target\n\n[Service]\nExecStart={} --config-dir {} --log-file {} {}\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n",
        q(&launch.exe),
        q(config_dir),
        q(&log_path(config_dir)),
        launch.args.join(" "),
    )
}

/// Whether the login item is installed (any state).
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub fn is_installed() -> bool {
    status().starts_with("installed")
}

/// Login items left by versions released as "carson" (best effort; ignored if absent).
fn remove_legacy() {
    #[cfg(target_os = "macos")]
    if let (Ok(uid), Some(home)) = (unsafe_free_uid(), dirs::home_dir()) {
        let _ = quiet("launchctl")
            .args(["bootout", &format!("gui/{uid}/com.carson.agent")])
            .output();
        let _ = std::fs::remove_file(home.join("Library/LaunchAgents/com.carson.agent.plist"));
    }
    #[cfg(target_os = "windows")]
    {
        let _ = quiet("schtasks")
            .args(["/Delete", "/TN", "carson-agent", "/F"])
            .output();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = quiet("systemctl")
            .args(["--user", "disable", "--now", "carson-agent"])
            .output();
    }
}

pub fn install(config_dir: &Path) -> Result<()> {
    remove_legacy();
    let launch = launch()?;
    #[cfg(target_os = "macos")]
    {
        let path = plist_path()?;
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        let uid = unsafe_free_uid()?;
        // Replace any previous version cleanly.
        let _ = quiet("launchctl")
            .args(["bootout", &format!("gui/{uid}/{LABEL}")])
            .output();
        std::fs::write(&path, launch_agent_plist(&launch, config_dir))?;
        run(quiet("launchctl")
            .args(["bootstrap", &format!("gui/{uid}")])
            .arg(&path))?;
        println!(
            "Installed {} (starts at login; log: {}).",
            path.display(),
            log_path(config_dir).display()
        );
    }
    #[cfg(target_os = "windows")]
    {
        let tr = format!(
            "\"{}\" --config-dir \"{}\" --log-file \"{}\" {}",
            launch.exe.display(),
            config_dir.display(),
            log_path(config_dir).display(),
            launch.args.join(" ")
        );
        run(quiet("schtasks").args([
            "/Create",
            "/TN",
            "glenley-agent",
            "/SC",
            "ONLOGON",
            "/RL",
            "LIMITED",
            "/F",
            "/TR",
            &tr,
        ]))?;
        run(quiet("schtasks").args(["/Run", "/TN", "glenley-agent"]))?;
        println!(
            "Installed scheduled task \"glenley-agent\" (starts at logon; log: {}).",
            log_path(config_dir).display()
        );
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let unit_dir = dirs::config_dir()
            .context("no config dir")?
            .join("systemd/user");
        std::fs::create_dir_all(&unit_dir)?;
        let unit = unit_dir.join("glenley-agent.service");
        std::fs::write(&unit, systemd_unit(&launch, config_dir))?;
        run(quiet("systemctl").args(["--user", "daemon-reload"]))?;
        run(quiet("systemctl").args(["--user", "enable", "--now", "glenley-agent"]))?;
        println!(
            "Installed {} (log: {}).",
            unit.display(),
            log_path(config_dir).display()
        );
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    remove_legacy();
    #[cfg(target_os = "macos")]
    {
        let uid = unsafe_free_uid()?;
        let _ = quiet("launchctl")
            .args(["bootout", &format!("gui/{uid}/{LABEL}")])
            .output();
        let path = plist_path()?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = quiet("schtasks")
            .args(["/End", "/TN", "glenley-agent"])
            .output();
        let _ = quiet("schtasks")
            .args(["/Delete", "/TN", "glenley-agent", "/F"])
            .output();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = quiet("systemctl")
            .args(["--user", "disable", "--now", "glenley-agent"])
            .output();
        if let Some(unit) = dirs::config_dir().map(|d| d.join("systemd/user/glenley-agent.service"))
        {
            let _ = std::fs::remove_file(unit);
        }
    }
    println!(
        "The glenley background service is removed (pairing is kept; `glenley-agent unpair` forgets it)."
    );
    Ok(())
}

/// Whether the service is installed and running, as a one-line description.
pub fn status() -> String {
    #[cfg(target_os = "macos")]
    {
        let Ok(uid) = unsafe_free_uid() else {
            return "unknown".into();
        };
        match quiet("launchctl")
            .args(["print", &format!("gui/{uid}/{LABEL}")])
            .output()
        {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                if text.contains("state = running") {
                    "installed, running".into()
                } else {
                    "installed, not running".into()
                }
            }
            _ => "not installed".into(),
        }
    }
    #[cfg(target_os = "windows")]
    {
        match quiet("schtasks")
            .args(["/Query", "/TN", "glenley-agent"])
            .output()
        {
            Ok(o) if o.status.success() => {
                if String::from_utf8_lossy(&o.stdout).contains("Running") {
                    "installed, running".into()
                } else {
                    "installed, not running".into()
                }
            }
            _ => "not installed".into(),
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match quiet("systemctl")
            .args(["--user", "is-active", "glenley-agent"])
            .output()
        {
            Ok(o) if o.status.success() => "installed, running".into(),
            Ok(_) => "not running (or not installed)".into(),
            Err(_) => "unknown".into(),
        }
    }
}

/// The current user's numeric id, via `id -u` (avoids libc FFI).
#[cfg(target_os = "macos")]
fn unsafe_free_uid() -> Result<String> {
    let out = quiet("id").arg("-u").output().context("run id -u")?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        bail!("could not determine user id");
    }
    Ok(uid)
}

/// Opens a URL in the default browser (best effort).
pub fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let r = quiet("open").arg(url).status();
    #[cfg(target_os = "windows")]
    let r = quiet("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = quiet("xdg-open").arg(url).status();
    r.is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_runs_service_mode_with_escaped_paths() {
        let p = launch_agent_plist(
            &Launch {
                exe: "/Applications/Carson & Co/glenley-agent".into(),
                args: vec!["run".into(), "--service".into()],
            },
            Path::new("/Users/a/Library/Application Support/carson"),
        );
        assert!(p.contains("<string>/Applications/Carson &amp; Co/glenley-agent</string>"));
        assert!(p.contains("<string>run</string>\n    <string>--service</string>"));
        assert!(
            p.contains("<key>SuccessfulExit</key><false/>"),
            "restart on crash, not after a clean stop"
        );
        assert!(
            p.contains("LimitLoadToSessionType</key><string>Aqua"),
            "needs the GUI session"
        );
    }

    #[test]
    fn systemd_unit_quotes_paths() {
        let u = systemd_unit(
            &Launch {
                exe: "/opt/my apps/glenley-agent".into(),
                args: vec!["run".into(), "--service".into()],
            },
            Path::new("/home/a/.config/carson"),
        );
        assert!(u.contains(
            "ExecStart=\"/opt/my apps/glenley-agent\" --config-dir \"/home/a/.config/carson\""
        ));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("glenley.log run --service") || u.contains("agent.log\" run --service"));
    }

    #[test]
    fn launch_agent_can_start_the_tray_app() {
        let p = launch_agent_plist(
            &Launch {
                exe: "/Applications/carson.app/Contents/MacOS/carson".into(),
                args: vec!["app".into()],
            },
            Path::new("/Users/a/Library/Application Support/carson"),
        );
        assert!(p.contains("agent.log</string>\n    <string>app</string>\n  </array>"));
    }
}
