//! carson-agent: the host-side daemon. Milestone 2 covers enrollment (`pair`) and
//! presence (`run`); capture, encoding, input and the tray UI arrive in later milestones.

mod api;
mod backoff;
mod config;
mod connection;
mod consent;
mod control;
mod identity;
mod keyframe;
mod keystore;
mod media;
mod peer;
mod sessions;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::api::{Api, PollStatus};
use crate::config::AgentConfig;
use crate::identity::DeviceKey;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
carson-agent — share and control this computer with carson

USAGE:
    carson-agent [OPTIONS] <COMMAND>

COMMANDS:
    pair --server <URL> [--name <NAME>]   Enroll this computer with your carson account
    run [MEDIA OPTIONS]                   Stay online and share this screen with your viewers
    capture-check [MEDIA OPTIONS]         Check screen capture works (permission, frame rate)
    status                                Show pairing state
    unpair                                Forget this computer's identity (local only)
    unattended on [--pin] | off           Allow connections without approval at this computer
                                          (optionally protected by a 6-10 digit PIN)

MEDIA OPTIONS:
    --fps <N>              Capture/encode frame rate (default 30)
    --max-width <PX>       Downscale wider screens to this width (default 1920)
    --bitrate-kbps <N>     Target video bitrate (default 4000)
    --display <N>          Display index, 0 = main (default 0)
    --test-pattern         Stream a synthetic pattern instead of the screen (implies --input log)
    --input <MODE>         native (default): control this computer; log: only log input; off: view only
    --no-clipboard         Do not sync the text clipboard

OPTIONS:
    --config-dir <DIR>     State directory (default: OS config dir/carson, or $CARSON_AGENT_DIR)
    --log-format <FMT>     text (default) or json
    -V, --version          Print version
    -h, --help             Print help

Logging verbosity follows RUST_LOG (default: info).";

#[derive(Debug)]
enum Command {
    Pair {
        server: String,
        name: Option<String>,
    },
    Run(media::MediaOptions),
    CaptureCheck(media::MediaOptions),
    Status,
    Unpair,
    Unattended {
        enable: bool,
        pin: bool,
    },
    /// Hidden: host UI helper process (consent prompt / banner), spawned by `run`.
    Ui(Vec<String>),
    Help,
    Version,
}

#[derive(Debug)]
struct Cli {
    command: Command,
    config_dir: Option<PathBuf>,
    json_logs: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli> {
    let mut args = args.into_iter();
    let mut command = None;
    let (mut server, mut name, mut config_dir, mut json_logs) = (None, None, None, false);
    let mut media = media::MediaOptions::default();
    let mut media_flags = false;
    let mut input_explicit = false;
    let mut unattended: Option<bool> = None;
    let mut pin = false;
    fn number<T>(flag: &str, v: String, min: T, max: T) -> Result<T>
    where
        T: std::str::FromStr + PartialOrd + std::fmt::Display,
    {
        let n: T = v
            .parse()
            .map_err(|_| anyhow::anyhow!("{flag} expects a number, got {v:?}"))?;
        anyhow::ensure!(
            n >= min && n <= max,
            "{flag} must be between {min} and {max}"
        );
        Ok(n)
    }
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                return Ok(Cli {
                    command: Command::Help,
                    config_dir,
                    json_logs,
                });
            }
            "-V" | "--version" => {
                return Ok(Cli {
                    command: Command::Version,
                    config_dir,
                    json_logs,
                });
            }
            "--server" => server = Some(value("--server")?),
            "--name" => name = Some(value("--name")?),
            "--config-dir" => config_dir = Some(PathBuf::from(value("--config-dir")?)),
            "--log-format" => match value("--log-format")?.as_str() {
                "json" => json_logs = true,
                "text" => json_logs = false,
                other => bail!("unknown log format {other:?} (use text or json)"),
            },
            "--fps" => {
                media_flags = true;
                let fps = number("--fps", value("--fps")?, 1u32, 120)?;
                media.capture.fps = fps;
                media.encoder.fps = fps;
            }
            "--max-width" => {
                media_flags = true;
                media.capture.max_width =
                    number("--max-width", value("--max-width")?, 320usize, 7680)?;
            }
            "--bitrate-kbps" => {
                media_flags = true;
                media.encoder.bitrate_bps =
                    number("--bitrate-kbps", value("--bitrate-kbps")?, 100u32, 100_000)? * 1000;
            }
            "--display" => {
                media_flags = true;
                media.capture.display_index = number("--display", value("--display")?, 0usize, 16)?;
            }
            "--test-pattern" => {
                media_flags = true;
                media.source = media::Source::TestPattern;
                if !input_explicit {
                    media.input = media::InputMode::Log;
                }
            }
            "--input" => {
                media_flags = true;
                let mode = match value("--input")?.as_str() {
                    "native" => media::InputMode::Native,
                    "log" => media::InputMode::Log,
                    "off" => media::InputMode::Off,
                    other => bail!("unknown input mode {other:?} (use native, log or off)"),
                };
                input_explicit = true;
                media.input = mode;
            }
            "--no-clipboard" => {
                media_flags = true;
                media.clipboard = false;
            }
            "pair" | "run" | "capture-check" | "status" | "unpair" | "unattended"
                if command.is_none() =>
            {
                command = Some(arg)
            }
            "ui" if command.is_none() => {
                return Ok(Cli {
                    command: Command::Ui(args.collect()),
                    config_dir,
                    json_logs,
                });
            }
            "on" | "off" if command.as_deref() == Some("unattended") && unattended.is_none() => {
                unattended = Some(arg == "on");
            }
            "--pin" => pin = true,
            other => bail!("unexpected argument {other:?} (try --help)"),
        }
    }
    let command = match command.as_deref() {
        None => Command::Help,
        Some("pair") => Command::Pair {
            server: server.context("pair needs --server <URL>")?,
            name,
        },
        Some(c) if server.is_some() || name.is_some() => {
            bail!("--server/--name only apply to pair, not {c}")
        }
        Some("run") => Command::Run(media),
        Some("capture-check") => Command::CaptureCheck(media),
        Some(c) if media_flags => {
            bail!("media options only apply to run and capture-check, not {c}")
        }
        Some("unattended") => {
            let enable = unattended.context("unattended needs on or off")?;
            anyhow::ensure!(enable || !pin, "--pin only applies to unattended on");
            Command::Unattended { enable, pin }
        }
        Some(c) if pin => bail!("--pin only applies to unattended on, not {c}"),
        Some("status") => Command::Status,
        Some("unpair") => Command::Unpair,
        Some(_) => unreachable!(),
    };
    Ok(Cli {
        command,
        config_dir,
        json_logs,
    })
}

fn init_logging(json: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}

fn default_name() -> String {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let host = host.trim_end_matches(".local").trim();
    let name: String = host.chars().filter(|c| !c.is_control()).take(64).collect();
    if name.is_empty() {
        "carson device".into()
    } else {
        name
    }
}

async fn pair(dir: PathBuf, server: &str, name: Option<String>) -> Result<()> {
    if let Some(existing) = config::load(&dir)? {
        bail!(
            "already paired with {} as {} ({}); run `carson-agent unpair` first",
            existing.server_url,
            existing.name,
            existing.device_id
        );
    }
    let server = config::normalize_server_url(server)?;
    let name = name.unwrap_or_else(default_name);
    let api = Api::new(&server)?;
    let store = keystore::default_store(&server, &dir)?;
    let key = DeviceKey::generate()?;
    store.save(&key.seed()).context("save device key")?;

    let start = api
        .pair_start(&key, &name, carson_proto::current_os())
        .await?;
    println!(
        "To pair \"{name}\", sign in and open:\n\n    {}\n",
        start.verification_url
    );
    println!("code: {}", start.user_code);
    println!(
        "\nThe code expires in {} minutes. Waiting for approval…",
        start.expires_in / 60
    );

    let deadline = Instant::now() + Duration::from_secs(start.expires_in);
    let interval = Duration::from_secs(start.interval.max(1));
    while Instant::now() < deadline {
        tokio::time::sleep(interval).await;
        match api.pair_poll(&start.pairing_id).await {
            Ok(PollStatus::Pending) => {}
            Ok(PollStatus::Approved { device_id }) => {
                config::save(
                    &dir,
                    &AgentConfig {
                        server_url: server,
                        device_id: device_id.clone(),
                        name,
                        unattended: false,
                        pin_hash: None,
                    },
                )?;
                println!("Paired as {device_id}. Start the agent with: carson-agent run");
                return Ok(());
            }
            Ok(PollStatus::Expired) => break,
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "poll failed; retrying"),
        }
    }
    let _ = store.delete();
    bail!("pairing code expired; run `carson-agent pair` again")
}

fn load_paired(dir: &std::path::Path) -> Result<(AgentConfig, DeviceKey)> {
    let cfg =
        config::load(dir)?.context("not paired yet; run `carson-agent pair --server <URL>`")?;
    let seed = keystore::default_store(&cfg.server_url, dir)?
        .load()?
        .context(
            "device key missing from key store; run `carson-agent unpair` then `pair` again",
        )?;
    Ok((cfg, DeviceKey::from_seed(&seed)))
}

async fn run(dir: PathBuf, media: media::MediaOptions) -> Result<()> {
    let (cfg, key) = load_paired(&dir)?;
    tracing::info!(server = %cfg.server_url, device_id = %cfg.device_id, version = VERSION, "starting");
    // A synthetic stream with no real input skips the prompt (nothing real is shared), but a
    // configured unattended PIN is still enforced.
    let synthetic = media.source == media::Source::TestPattern
        && !matches!(media.input, media::InputMode::Native);
    let policy = match cfg.access_policy() {
        consent::AccessPolicy::Attended if synthetic => consent::AccessPolicy::Automatic,
        other => other,
    };
    match &policy {
        consent::AccessPolicy::Attended => {
            tracing::info!("attended mode: each connection must be allowed at this computer")
        }
        consent::AccessPolicy::Unattended { pin_hash: Some(_) } => {
            tracing::info!("unattended mode with PIN")
        }
        consent::AccessPolicy::Unattended { pin_hash: None } => {
            tracing::warn!(
                "unattended mode WITHOUT a PIN: anyone signed in to your account can connect"
            )
        }
        consent::AccessPolicy::Automatic => {
            tracing::info!("test pattern: no consent prompt or banner")
        }
    }
    let agent = connection::Agent::new(
        Api::new(&cfg.server_url)?,
        key,
        cfg.device_id,
        media,
        policy,
    );
    tokio::select! {
        res = agent.run() => res,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutting down");
            Ok(())
        }
    }
}

/// Opens the capture source and measures a few seconds of frames (permission diagnostics).
async fn capture_check(media: &media::MediaOptions) -> Result<()> {
    let opts = media.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut cap = media::open_source(&opts).map_err(|e| {
            if e.chain().any(|c| c.is::<carson_capture::PermissionDenied>()) {
                e.context("grant Screen Recording to the app running carson-agent (e.g. Terminal), then restart it")
            } else {
                e
            }
        })?;
        match media::list_displays(&opts) {
            Ok(list) => {
                for d in list {
                    println!(
                        "display {}: {} {}x{} at ({}, {}){}",
                        d.index, d.name, d.width, d.height, d.bounds.0, d.bounds.1,
                        if d.index == opts.capture.display_index { "  ← capturing" } else { "" }
                    );
                }
            }
            Err(e) => println!("could not list displays: {e:#}"),
        }
        println!("capture started; measuring for 3 s (move a window to generate frames)…");
        let start = Instant::now();
        let (mut n, mut size) = (0u32, (0, 0));
        while start.elapsed() < Duration::from_secs(3) {
            if let Some(f) = cap.next_frame(Duration::from_millis(500)) {
                n += 1;
                size = (f.width, f.height);
            }
        }
        if n == 0 {
            bail!("no frames received; the screen may be static or capture is blocked");
        }
        println!("ok: {n} frames at {}x{} ({:.1} fps)", size.0, size.1, f64::from(n) / start.elapsed().as_secs_f64());
        Ok(())
    })
    .await?
}

fn status(dir: PathBuf) -> Result<()> {
    match config::load(&dir)? {
        None => println!("not paired"),
        Some(cfg) => {
            let key = keystore::default_store(&cfg.server_url, &dir)?.load()?;
            let key_id = key.map_or_else(
                || "MISSING".to_owned(),
                |seed| DeviceKey::from_seed(&seed).key_id(),
            );
            let access = match (cfg.unattended, cfg.pin_hash.is_some()) {
                (false, _) => "attended (each connection must be allowed here)",
                (true, true) => "unattended, PIN required",
                (true, false) => "unattended, no PIN",
            };
            println!(
                "paired\n  server:    {}\n  device id: {}\n  name:      {}\n  key id:    {}  (the viewer shows the same value)\n  access:    {}",
                cfg.server_url, cfg.device_id, cfg.name, key_id, access
            );
        }
    }
    Ok(())
}

/// Turns unattended access on or off. Must be run at this computer: it is never remotely settable.
fn set_unattended(dir: PathBuf, enable: bool, with_pin: bool) -> Result<()> {
    let mut cfg =
        config::load(&dir)?.context("not paired yet; run `carson-agent pair --server <URL>`")?;
    cfg.unattended = enable;
    cfg.pin_hash = None;
    if enable && with_pin {
        use std::io::IsTerminal;
        let pin = if std::io::stdin().is_terminal() {
            let pin = rpassword::prompt_password("New PIN (6-10 digits): ")?;
            consent::validate_pin(&pin)?;
            let again = rpassword::prompt_password("Repeat PIN: ")?;
            anyhow::ensure!(pin == again, "PINs do not match");
            pin
        } else {
            // Scripted setup: `echo 123456 | carson-agent unattended on --pin`.
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_owned()
        };
        consent::validate_pin(&pin)?;
        cfg.pin_hash = Some(consent::hash_pin(&pin)?);
    }
    config::save(&dir, &cfg)?;
    match (enable, cfg.pin_hash.is_some()) {
        (false, _) => {
            println!("Unattended access is off: every connection must be allowed at this computer.")
        }
        (true, true) => println!(
            "Unattended access is on. Viewers must enter the PIN. Restart `carson-agent run` to apply."
        ),
        (true, false) => {
            println!(
                "Unattended access is on WITHOUT a PIN: anyone signed in to your account can connect."
            );
            println!(
                "Consider `carson-agent unattended on --pin`. Restart `carson-agent run` to apply."
            );
        }
    }
    Ok(())
}

fn unpair(dir: PathBuf) -> Result<()> {
    if let Some(cfg) = config::load(&dir)? {
        keystore::default_store(&cfg.server_url, &dir)?.delete()?;
        config::delete(&dir)?;
        println!(
            "Forgot device {}. Also remove it from \"My devices\" in your account.",
            cfg.device_id
        );
    } else {
        println!("not paired");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match parse_args(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("carson-agent: {e:#}");
            return ExitCode::from(2);
        }
    };
    init_logging(cli.json_logs);
    let dir = match cli.config_dir.map_or_else(config::default_dir, Ok) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("carson-agent: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let result = match cli.command {
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Command::Version => {
            println!(
                "carson-agent {VERSION} (protocol v{})",
                carson_proto::PROTOCOL_VERSION
            );
            Ok(())
        }
        Command::Pair { server, name } => pair(dir, &server, name).await,
        Command::Run(media) => run(dir, media).await,
        Command::CaptureCheck(media) => capture_check(&media).await,
        Command::Status => status(dir),
        Command::Unpair => unpair(dir),
        Command::Unattended { enable, pin } => set_unattended(dir, enable, pin),
        Command::Ui(args) => ui::helper_main(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("carson-agent: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_commands() {
        assert!(matches!(
            parse(&["pair", "--server", "https://c.example", "--name", "Mac"]).unwrap().command,
            Command::Pair { server, name: Some(name) } if server == "https://c.example" && name == "Mac"
        ));
        let cli = parse(&["--config-dir", "/tmp/x", "--log-format", "json", "run"]).unwrap();
        assert!(matches!(cli.command, Command::Run(_)));
        assert_eq!(cli.config_dir, Some(PathBuf::from("/tmp/x")));
        assert!(cli.json_logs);
        assert!(matches!(parse(&[]).unwrap().command, Command::Help));
        assert!(matches!(parse(&["-V"]).unwrap().command, Command::Version));
        let Command::Run(m) = parse(&[
            "run",
            "--fps",
            "60",
            "--bitrate-kbps",
            "8000",
            "--test-pattern",
        ])
        .unwrap()
        .command
        else {
            panic!("expected run");
        };
        assert_eq!(
            (m.capture.fps, m.encoder.fps, m.encoder.bitrate_bps),
            (60, 60, 8_000_000)
        );
        assert_eq!(m.source, media::Source::TestPattern);
        assert!(
            matches!(m.input, media::InputMode::Log),
            "test pattern never injects by default"
        );
        let Command::Run(m) = parse(&[
            "run",
            "--input",
            "native",
            "--test-pattern",
            "--no-clipboard",
        ])
        .unwrap()
        .command
        else {
            panic!("expected run");
        };
        assert!(
            matches!(m.input, media::InputMode::Native),
            "explicit --input wins"
        );
        assert!(!m.clipboard);
    }

    #[test]
    fn rejects_bad_args() {
        assert!(parse(&["pair"]).is_err(), "pair without --server");
        assert!(
            parse(&["run", "--server", "x"]).is_err(),
            "--server outside pair"
        );
        assert!(parse(&["run", "run"]).is_err(), "two commands");
        assert!(parse(&["--log-format", "xml", "run"]).is_err());
        assert!(parse(&["--name"]).is_err(), "flag without value");
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&["run", "--fps", "0"]).is_err(), "fps out of range");
        assert!(parse(&["run", "--fps", "fast"]).is_err());
        assert!(
            parse(&["status", "--test-pattern"]).is_err(),
            "media flag on wrong command"
        );
    }

    #[test]
    fn default_name_is_printable() {
        let n = default_name();
        assert!(!n.is_empty() && n.chars().count() <= 64 && !n.chars().any(char::is_control));
    }
}
