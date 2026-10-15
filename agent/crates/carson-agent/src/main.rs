//! carson-agent: the host-side daemon. Milestone 2 covers enrollment (`pair`) and
//! presence (`run`); capture, encoding, input and the tray UI arrive in later milestones.

mod api;
mod backoff;
mod config;
mod connection;
mod identity;
mod keystore;

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
    run                                   Stay online so your other devices can see this one
    status                                Show pairing state
    unpair                                Forget this computer's identity (local only)

OPTIONS:
    --config-dir <DIR>     State directory (default: OS config dir/carson, or $CARSON_AGENT_DIR)
    --log-format <FMT>     text (default) or json
    -V, --version          Print version
    -h, --help             Print help

Logging verbosity follows RUST_LOG (default: info).";

#[derive(Debug, PartialEq)]
enum Command {
    Pair {
        server: String,
        name: Option<String>,
    },
    Run,
    Status,
    Unpair,
    Help,
    Version,
}

#[derive(Debug, PartialEq)]
struct Cli {
    command: Command,
    config_dir: Option<PathBuf>,
    json_logs: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli> {
    let mut args = args.into_iter();
    let mut command = None;
    let (mut server, mut name, mut config_dir, mut json_logs) = (None, None, None, false);
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
            "pair" | "run" | "status" | "unpair" if command.is_none() => command = Some(arg),
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
        Some("run") => Command::Run,
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

async fn run(dir: PathBuf) -> Result<()> {
    let (cfg, key) = load_paired(&dir)?;
    tracing::info!(server = %cfg.server_url, device_id = %cfg.device_id, version = VERSION, "starting");
    let agent = connection::Agent {
        api: Api::new(&cfg.server_url)?,
        key,
        device_id: cfg.device_id,
    };
    tokio::select! {
        res = agent.run() => res,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutting down");
            Ok(())
        }
    }
}

fn status(dir: PathBuf) -> Result<()> {
    match config::load(&dir)? {
        None => println!("not paired"),
        Some(cfg) => {
            let has_key = keystore::default_store(&cfg.server_url, &dir)?
                .load()?
                .is_some();
            println!(
                "paired\n  server:    {}\n  device id: {}\n  name:      {}\n  key:       {}",
                cfg.server_url,
                cfg.device_id,
                cfg.name,
                if has_key { "present" } else { "MISSING" }
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
        Command::Run => run(dir).await,
        Command::Status => status(dir),
        Command::Unpair => unpair(dir),
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
        assert_eq!(
            parse(&["pair", "--server", "https://c.example", "--name", "Mac"])
                .unwrap()
                .command,
            Command::Pair {
                server: "https://c.example".into(),
                name: Some("Mac".into())
            }
        );
        let cli = parse(&["--config-dir", "/tmp/x", "--log-format", "json", "run"]).unwrap();
        assert_eq!(cli.command, Command::Run);
        assert_eq!(cli.config_dir, Some(PathBuf::from("/tmp/x")));
        assert!(cli.json_logs);
        assert_eq!(parse(&[]).unwrap().command, Command::Help);
        assert_eq!(parse(&["-V"]).unwrap().command, Command::Version);
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
    }

    #[test]
    fn default_name_is_printable() {
        let n = default_name();
        assert!(!n.is_empty() && n.chars().count() <= 64 && !n.chars().any(char::is_control));
    }
}
