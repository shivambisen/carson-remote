//! glenley host agent, shared by two binaries:
//! - `glenley-agent`: the command-line tool (`setup`, `run`, …).
//! - `glenley`: the tray app people double-click (no console window on Windows; a menu-bar
//!   item on macOS). Without arguments it starts the tray; with arguments it is the CLI.

mod abr;
mod api;
mod backoff;
mod codec;
mod config;
mod connection;
mod consent;
mod control;
mod identity;
mod keyframe;
mod keystore;
mod media;
mod peer;
mod service;
mod sessions;
mod signin;
mod status;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod tray;
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
glenley-agent — share and control this computer with glenley

USAGE:
    glenley-agent [OPTIONS] <COMMAND>

COMMANDS:
    app [--server <URL>]                  Start the tray app (what double-clicking `glenley` does)
    setup --server <URL> [--name <NAME>]  One-step install: pair, permissions, codec, start at login
    pair --server <URL> [--name <NAME>]   Enroll this computer with your glenley account
    run [MEDIA OPTIONS]                   Stay online and share this screen with your viewers
    capture-check [MEDIA OPTIONS]         Check screen capture works (permission, frame rate)
    status                                Show pairing state
    pair --server <URL> --code [--once]   Show a code someone else enters to add this computer
                                          (--once: it leaves their account after one session)
    leave                                 Remove this computer from its account and forget it
    unpair                                Forget this computer's identity (local only)
    service install | uninstall | status  Start glenley automatically at login (or stop doing so)
    ask on | off                          Ask \"Allow?\" here before each connection (default off)
    indicator on | off                    Show an on-screen pill while someone is connected
                                          (default off; the tray icon always shows it)
    unattended on --pin | off             Require a 6-10 digit PIN before each connection

MEDIA OPTIONS:
    --fps <N>              Capture/encode frame rate (default 30)
    --max-width <PX>       Downscale wider screens to this width (default 1920)
    --bitrate-kbps <N>     Target video bitrate (default 4000)
    --display <N>          Display index, 0 = main (default 0)
    --test-pattern         Stream a synthetic pattern instead of the screen (implies --input log)
    --input <MODE>         native (default): control this computer; log: only log input; off: view only
    --no-clipboard         Do not sync the text clipboard
    --encoder <KIND>       auto (default: hardware when available) or software

OPTIONS:
    --config-dir <DIR>     State directory (default: OS config dir/carson, or $CARSON_AGENT_DIR)
    --log-format <FMT>     text (default) or json
    --log-file <PATH>      Append logs to a file instead of stderr
    -V, --version          Print version
    -h, --help             Print help

Logging verbosity follows RUST_LOG (default: info).";

#[derive(Debug)]
enum Command {
    Pair {
        server: String,
        name: Option<String>,
        /// Only show a code for someone else to enter (no browser sign-in here).
        code: bool,
        /// Shared just this once.
        once: bool,
    },
    /// Remove this computer from its account (on the server too) and forget it here.
    Leave,
    Run {
        media: media::MediaOptions,
        /// Running under the OS service manager: fatal errors stop the service quietly.
        service: bool,
    },
    Setup {
        server: String,
        name: Option<String>,
        open_browser: bool,
        install_service: bool,
    },
    Service(ServiceAction),
    CaptureCheck(media::MediaOptions),
    Status,
    Unpair,
    Unattended {
        enable: bool,
        pin: bool,
    },
    Ask(bool),
    Indicator(bool),
    /// Hidden: host UI helper process (consent prompt / banner), spawned by `run`.
    Ui(Vec<String>),
    /// The tray app (default for the `glenley` binary). `server`: where "Sign in" goes.
    App {
        server: Option<String>,
        /// Start with the window hidden (the login item).
        background: bool,
    },
    Help,
    Version,
}

#[derive(Debug, PartialEq)]
enum ServiceAction {
    Install,
    Uninstall,
    Status,
}

#[derive(Debug)]
struct Cli {
    command: Command,
    config_dir: Option<PathBuf>,
    json_logs: bool,
    log_file: Option<PathBuf>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli> {
    let mut args = args.into_iter();
    let mut command = None;
    let (mut server, mut name, mut config_dir, mut json_logs) = (None, None, None, false);
    let mut media = media::MediaOptions::default();
    let mut media_flags = false;
    let mut input_explicit = false;
    let mut unattended: Option<bool> = None;
    let mut log_file: Option<PathBuf> = None;
    let (mut service_mode, mut no_browser, mut no_service) = (false, false, false);
    let mut service_action: Option<ServiceAction> = None;
    let mut pin = false;
    let (mut code_only, mut once, mut background) = (false, false, false);
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
                    log_file,
                });
            }
            "-V" | "--version" => {
                return Ok(Cli {
                    command: Command::Version,
                    config_dir,
                    json_logs,
                    log_file,
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
            "--encoder" => {
                media_flags = true;
                media.encoder_pref = match value("--encoder")?.as_str() {
                    "auto" => carson_encode::EncoderPreference::Auto,
                    "software" => carson_encode::EncoderPreference::Software,
                    other => bail!("unknown encoder {other:?} (use auto or software)"),
                };
            }
            "--no-clipboard" => {
                media_flags = true;
                media.clipboard = false;
            }
            "pair" | "setup" | "service" | "run" | "capture-check" | "status" | "unpair"
            | "leave" | "app" | "unattended" | "ask" | "indicator"
                if command.is_none() =>
            {
                command = Some(arg)
            }
            "ui" if command.is_none() => {
                return Ok(Cli {
                    command: Command::Ui(args.collect()),
                    config_dir,
                    json_logs,
                    log_file,
                });
            }
            "on" | "off"
                if matches!(command.as_deref(), Some("unattended" | "ask" | "indicator"))
                    && unattended.is_none() =>
            {
                unattended = Some(arg == "on");
            }
            "--pin" => pin = true,
            "--code" => code_only = true,
            "--background" => background = true,
            "--once" => once = true,
            "--log-file" => log_file = Some(PathBuf::from(value("--log-file")?)),
            "--service" => service_mode = true,
            "--no-browser" => no_browser = true,
            "--no-service" => no_service = true,
            "install" | "uninstall" | "status"
                if command.as_deref() == Some("service") && service_action.is_none() =>
            {
                service_action = Some(match arg.as_str() {
                    "install" => ServiceAction::Install,
                    "uninstall" => ServiceAction::Uninstall,
                    _ => ServiceAction::Status,
                });
            }
            other => bail!("unexpected argument {other:?} (try --help)"),
        }
    }
    let command = match command.as_deref() {
        None => Command::Help,
        Some("pair") => Command::Pair {
            server: server.context("pair needs --server <URL>")?,
            name,
            code: code_only || once,
            once,
        },
        Some(c) if code_only || once => bail!("--code/--once only apply to pair, not {c}"),
        Some("setup") => Command::Setup {
            server: server
                .context("setup needs --server <URL> (the address of your glenley server)")?,
            name,
            open_browser: !no_browser,
            install_service: !no_service,
        },
        Some("app") if name.is_none() && !media_flags => Command::App { server, background },
        Some(c) if server.is_some() || name.is_some() => {
            bail!("--server/--name only apply to pair, not {c}")
        }
        Some("run") => Command::Run {
            media,
            service: service_mode,
        },
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
        Some("ask") => Command::Ask(unattended.context("ask needs on or off")?),
        Some("indicator") => Command::Indicator(unattended.context("indicator needs on or off")?),
        Some("service") => {
            Command::Service(service_action.context("service needs install, uninstall or status")?)
        }
        Some("status") => Command::Status,
        Some("unpair") => Command::Unpair,
        Some("leave") => Command::Leave,
        Some(_) => unreachable!(),
    };
    Ok(Cli {
        command,
        config_dir,
        json_logs,
        log_file,
    })
}

fn init_logging(json: bool, file: Option<&std::path::Path>) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,rtc_dtls=error,rtc_ice=warn"));
    let file = file.and_then(|p| {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match (file, json) {
        (Some(f), true) => builder.json().with_writer(std::sync::Mutex::new(f)).init(),
        (Some(f), false) => builder
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
        (None, true) => builder.json().with_writer(std::io::stderr).init(),
        (None, false) => builder.with_writer(std::io::stderr).init(),
    }
}

fn default_name() -> String {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let host = host.trim_end_matches(".local").trim();
    let name: String = host.chars().filter(|c| !c.is_control()).take(64).collect();
    if name.is_empty() {
        "glenley device".into()
    } else {
        name
    }
}

pub(crate) type CodeCallback = Box<dyn Fn(&str) + Send>;

/// How a computer joins an account.
#[derive(Default)]
pub(crate) struct PairOptions {
    /// Open the browser sign-in page (otherwise only print the link and code).
    pub open_browser: bool,
    /// Shared "just this once": removed from the account after one session.
    pub temporary: bool,
    /// Called with the code to read out ("BCDF-GHJK") as soon as it exists.
    pub on_code: Option<CodeCallback>,
}

async fn pair(dir: PathBuf, server: &str, name: Option<String>, open_browser: bool) -> Result<()> {
    pair_with(
        dir,
        server,
        name,
        PairOptions {
            open_browser,
            ..Default::default()
        },
    )
    .await
}

pub(crate) async fn pair_with(
    dir: PathBuf,
    server: &str,
    name: Option<String>,
    opts: PairOptions,
) -> Result<()> {
    let open_browser = opts.open_browser;
    if let Some(existing) = config::load(&dir)? {
        bail!(
            "already paired with {} as {} ({}); run `glenley-agent unpair` first",
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
        .pair_start(&key, &name, carson_proto::current_os(), opts.temporary)
        .await?;
    if let Some(f) = &opts.on_code {
        f(&start.user_code);
    }
    // Browser sign-in (no code): the page hands a one-time grant back to this loopback port.
    let loopback = signin::Loopback::bind().await?;
    // The address that answered pair_start: an alternate when the main one is blocked here.
    let link = loopback.url(api.base(), &start.pairing_id);
    println!("To add \"{name}\" to your glenley account, sign in at:\n\n    {link}\n");
    if open_browser && service::open_browser(&link) {
        println!("(opened in your browser)");
    }
    println!(
        "Waiting for you to sign in (expires in {} minutes)…\nOr connect from another computer: choose \"Enter a code\" there and type {}.",
        start.expires_in / 60,
        start.user_code
    );
    let done_url = format!("{}/#/connected?name={}", api.base(), url_escape(&name));
    let signed_in = loopback.grant(&done_url);
    tokio::pin!(signed_in);

    let deadline = Instant::now() + Duration::from_secs(start.expires_in);
    let interval = Duration::from_secs(start.interval.max(1));
    while Instant::now() < deadline {
        let outcome = tokio::select! {
            grant = &mut signed_in => match grant {
                Ok(grant) => match api.pair_complete(&start.pairing_id, &grant).await {
                    Ok(device_id) => Ok(PollStatus::Approved { device_id }),
                    Err(e) => {
                        // Rebind and keep waiting; the code path still works meanwhile.
                        tracing::warn!(error = %format!("{e:#}"), "browser sign-in failed");
                        signed_in.set(loopback.grant(&done_url));
                        Ok(PollStatus::Pending)
                    }
                },
                Err(e) => Err(e),
            },
            () = tokio::time::sleep(interval) => api.pair_poll(&start.pairing_id).await,
        };
        match outcome {
            Ok(PollStatus::Pending) => {}
            Ok(PollStatus::Approved { device_id }) => {
                config::save(
                    &dir,
                    &AgentConfig {
                        server_url: server,
                        device_id: device_id.clone(),
                        name: name.clone(),
                        unattended: false,
                        pin_hash: None,
                        ask_before_connect: false,
                        show_indicator: false,
                    },
                )?;
                println!("Added \"{name}\" to your account ({device_id}).");
                return Ok(());
            }
            Ok(PollStatus::Expired) => break,
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "poll failed; retrying"),
        }
    }
    let _ = store.delete();
    bail!("sign-in expired; run `glenley-agent setup` again")
}

/// Percent-encodes a query value.
fn url_escape(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn load_paired(dir: &std::path::Path) -> Result<(AgentConfig, DeviceKey)> {
    let cfg =
        config::load(dir)?.context("not paired yet; run `glenley-agent pair --server <URL>`")?;
    let seed = keystore::default_store(&cfg.server_url, dir)?
        .load()?
        .context(
            "device key missing from key store; run `glenley-agent unpair` then `pair` again",
        )?;
    Ok((cfg, DeviceKey::from_seed(&seed)))
}

async fn run(dir: PathBuf, media: media::MediaOptions) -> Result<()> {
    let (cfg, key) = load_paired(&dir)?;
    tracing::info!(server = %cfg.server_url, device_id = %cfg.device_id, version = VERSION, "starting");
    // A synthetic stream with no real input skips the prompt (nothing real is shared), but a
    // configured PIN is still enforced.
    let synthetic = media.source == media::Source::TestPattern
        && !matches!(media.input, media::InputMode::Native);
    let settings = sessions::SettingsSource::File {
        dir: dir.clone(),
        synthetic,
    };
    match settings.current().policy {
        consent::AccessPolicy::Attended => {
            tracing::info!("each connection must be allowed at this computer")
        }
        consent::AccessPolicy::Unattended { pin_hash: Some(_) } => {
            tracing::info!("connections need the PIN")
        }
        consent::AccessPolicy::Unattended { pin_hash: None } => {
            tracing::info!(
                "your account connects without asking (`glenley-agent ask on` to change)"
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
        settings,
    );
    tokio::select! {
        res = agent.run() => res,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutting down");
            Ok(())
        }
    }
}

/// One step from download to "it just works": pair, permissions, codec, background service.
async fn setup(
    dir: PathBuf,
    server: &str,
    name: Option<String>,
    open_browser: bool,
    install_service: bool,
) -> Result<()> {
    println!("glenley setup\n");
    println!("1/4  Pairing");
    match config::load(&dir)? {
        Some(cfg) => println!(
            "     already paired with {} as \"{}\"",
            cfg.server_url, cfg.name
        ),
        None => pair(dir.clone(), server, name, open_browser).await?,
    }

    println!("\n2/4  Permissions");
    #[cfg(target_os = "macos")]
    {
        let screen = tokio::task::spawn_blocking(|| {
            media::open_source(&media::MediaOptions::default()).map(drop)
        })
        .await?;
        match screen {
            Ok(()) => println!("     Screen Recording: allowed"),
            Err(_) => {
                println!(
                    "     Screen Recording: NOT allowed yet. In System Settings > Privacy & Security >"
                );
                println!(
                    "     Screen & System Audio Recording, turn on glenley-agent, then run setup again."
                );
                let _ = service::open_browser(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
                );
            }
        }
        match carson_input::native_backend() {
            Ok(_) => println!("     Accessibility (remote control): allowed"),
            Err(_) => println!(
                "     Accessibility: NOT allowed yet (a macOS prompt is open). Without it, the computer is view-only."
            ),
        }
    }
    #[cfg(not(target_os = "macos"))]
    println!("     nothing to grant on this OS");

    println!("\n3/4  Video codec");
    #[cfg(target_os = "macos")]
    println!("     using the Mac's hardware H.264 encoder (VideoToolbox)");
    match codec::ensure_openh264().await {
        Ok(Some(path)) => println!("     OpenH264 fallback ready ({})", path.display()),
        Ok(None) => println!("     no OpenH264 build for this platform"),
        Err(e) if cfg!(target_os = "macos") => {
            println!("     OpenH264 fallback not installed ({e:#}); hardware encoding still works")
        }
        Err(e) => return Err(e.context("this platform needs OpenH264 to stream")),
    }

    println!("\n4/4  Start automatically");
    if install_service {
        service::install(&dir)?;
    } else {
        println!("     skipped (--no-service); start manually with `glenley-agent run`");
    }

    let cfg = config::load(&dir)?.context("pairing did not complete")?;
    println!(
        "\nDone. This computer appears under \"My devices\" at {}.",
        cfg.server_url
    );
    println!(
        "Your account can now connect from the website. To be asked first: `glenley-agent ask on`."
    );
    Ok(())
}

/// Opens the capture source and measures a few seconds of frames (permission diagnostics).
async fn capture_check(media: &media::MediaOptions) -> Result<()> {
    let opts = media.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut cap = media::open_source(&opts).map_err(|e| {
            if e.chain().any(|c| c.is::<carson_capture::PermissionDenied>()) {
                e.context("grant Screen Recording to the app running glenley-agent (e.g. Terminal), then restart it")
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
            let access = match cfg.access_policy() {
                consent::AccessPolicy::Attended => "asks before each connection",
                consent::AccessPolicy::Unattended { pin_hash: Some(_) } => "PIN required",
                _ => "your account connects without asking",
            };
            let access = format!(
                "{access}; on-screen pill {}",
                if cfg.show_indicator { "on" } else { "off" }
            );
            println!(
                "paired\n  server:    {}\n  device id: {}\n  name:      {}\n  key id:    {}  (the viewer shows the same value)\n  access:    {}",
                cfg.server_url, cfg.device_id, cfg.name, key_id, access
            );
        }
    }
    Ok(())
}

/// Changes a local setting. Like every access setting, it is never remotely settable; a running
/// agent applies it to the next connection.
fn update_setting(
    dir: &std::path::Path,
    change: impl FnOnce(&mut config::AgentConfig),
) -> Result<()> {
    let mut cfg =
        config::load(dir)?.context("not paired yet; run `glenley-agent setup --server <URL>`")?;
    change(&mut cfg);
    config::save(dir, &cfg)
}

/// Turns PIN-protected access on or off. Must be run at this computer: it is never remotely settable.
fn set_unattended(dir: PathBuf, enable: bool, with_pin: bool) -> Result<()> {
    let mut cfg =
        config::load(&dir)?.context("not paired yet; run `glenley-agent pair --server <URL>`")?;
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
            // Scripted setup: `echo 123456 | glenley-agent unattended on --pin`.
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
            println!("PIN access is off.")
        }
        (true, true) => println!(
            "PIN access is on: viewers must enter the PIN (applies to the next connection)."
        ),
        (true, false) => {
            println!("No PIN set; use `glenley-agent unattended on --pin` to require one.");
        }
    }
    Ok(())
}

/// Removes this computer from its account on the server, then forgets it here. If the server
/// can't be reached it is still forgotten here (remove it on the website too).
pub(crate) async fn leave(dir: PathBuf) -> Result<()> {
    if let Ok((cfg, key)) = load_paired(&dir)
        && let Err(e) = Api::new(&cfg.server_url)?.leave(&cfg.device_id, &key).await
    {
        tracing::warn!(error = %format!("{e:#}"), "could not reach the server; forgetting this computer locally");
    }
    unpair(dir)
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

/// Dependencies enable both of rustls' crypto backends (reqwest: aws-lc-rs, webrtc: ring), so
/// rustls can't choose a process default and panics on the first TLS connection that relies on
/// it (the signaling WebSocket to an https server). Pick one explicitly, before anything else.
fn install_tls_provider() {
    // Err only means a provider is already installed, which is fine.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Entry point of both binaries. With no arguments, `app_by_default` starts the tray app
/// (the `glenley` binary) instead of printing help.
pub fn main_entry(app_by_default: bool) -> ExitCode {
    install_tls_provider();
    carson_input::enable_dpi_awareness();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = parse_args(args.clone()).and_then(|cli| {
        // `glenley` with only options (e.g. --config-dir) still means the tray app.
        let asked_for_help = args.iter().any(|a| a == "-h" || a == "--help");
        if app_by_default && matches!(cli.command, Command::Help) && !asked_for_help {
            parse_args(args.into_iter().chain(["app".to_owned()]))
        } else {
            Ok(cli)
        }
    });
    let cli = match parsed {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("glenley-agent: {e:#}");
            return ExitCode::from(2);
        }
    };
    let dir = match cli.config_dir.clone().map_or_else(config::default_dir, Ok) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("glenley-agent: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    if let Command::App { server, background } = &cli.command {
        let (server, background) = (server.clone(), *background);
        // The tray owns the main thread (AppKit requires it); the agent runs on a runtime thread.
        let log = cli
            .log_file
            .clone()
            .unwrap_or_else(|| service::log_path(&dir));
        init_logging(cli.json_logs, Some(&log));
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let result = tray::run(dir, server, background);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let result: Result<()> = {
            let _ = (server, background);
            Err(anyhow::anyhow!(
                "the tray app is available on macOS and Windows; use `glenley-agent run`"
            ))
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "glenley app failed");
                eprintln!("glenley: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    init_logging(cli.json_logs, cli.log_file.as_deref());
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(cli_main(cli, dir)),
        Err(e) => {
            eprintln!("glenley-agent: start runtime: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn cli_main(cli: Cli, dir: PathBuf) -> ExitCode {
    let result = match cli.command {
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Command::Version => {
            println!(
                "glenley-agent {VERSION} (protocol v{})",
                carson_proto::PROTOCOL_VERSION
            );
            Ok(())
        }
        Command::Pair {
            server,
            name,
            code: false,
            ..
        } => pair(dir, &server, name, false).await,
        Command::Pair {
            server, name, once, ..
        } => {
            pair_with(
                dir,
                &server,
                name,
                PairOptions {
                    temporary: once,
                    on_code: Some(Box::new(move |code| {
                        println!(
                            "\n    Connection code:  {code}\n\nGive it to the person who will connect: they enter it at their glenley website under \"Enter a code\".{}\n",
                            if once { " This computer leaves their account after the session." } else { "" }
                        )
                    })),
                    ..Default::default()
                },
            )
            .await
        }
        Command::Leave => leave(dir).await,
        Command::Run {
            media,
            service: true,
        } => match run(dir, media).await {
            // Under a service manager, a fatal error (device removed, duplicate instance)
            // stops the service instead of triggering endless restarts.
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "stopping the glenley service");
                Ok(())
            }
            ok => ok,
        },
        Command::Run {
            media,
            service: false,
        } => run(dir, media).await,
        Command::Setup {
            server,
            name,
            open_browser,
            install_service,
        } => setup(dir, &server, name, open_browser, install_service).await,
        Command::Service(ServiceAction::Install) => service::install(&dir),
        Command::Service(ServiceAction::Uninstall) => service::uninstall(),
        Command::Service(ServiceAction::Status) => {
            println!("{}", service::status());
            Ok(())
        }
        Command::CaptureCheck(media) => capture_check(&media).await,
        Command::Status => status(dir),
        Command::Unpair => unpair(dir),
        Command::Unattended { enable, pin } => set_unattended(dir, enable, pin),
        Command::Ask(on) => update_setting(&dir, |c| c.ask_before_connect = on).map(|()| {
            println!(
                "{}",
                if on {
                    "Each connection now asks \"Allow?\" at this computer."
                } else {
                    "Connections from your account no longer ask at this computer."
                }
            )
        }),
        Command::Indicator(on) => update_setting(&dir, |c| c.show_indicator = on).map(|()| {
            println!(
                "{}",
                if on {
                    "An on-screen pill now shows while someone is connected."
                } else {
                    "No on-screen pill; the tray icon still shows when someone is connected."
                }
            )
        }),
        Command::Ui(args) => ui::helper_main(&args),
        Command::App { .. } => unreachable!("handled before the runtime starts"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glenley-agent: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_provider_is_installed_for_https_connections() {
        install_tls_provider();
        install_tls_provider(); // idempotent
        // Panics without a process default when both backends are compiled in.
        let _ = rustls::ClientConfig::builder();
    }

    fn parse(args: &[&str]) -> Result<Cli> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_commands() {
        assert!(matches!(
            parse(&["pair", "--server", "https://c.example", "--name", "Mac"]).unwrap().command,
            Command::Pair { server, name: Some(name), .. } if server == "https://c.example" && name == "Mac"
        ));
        let cli = parse(&["--config-dir", "/tmp/x", "--log-format", "json", "run"]).unwrap();
        assert!(matches!(cli.command, Command::Run { service: false, .. }));
        assert_eq!(cli.config_dir, Some(PathBuf::from("/tmp/x")));
        assert!(cli.json_logs);
        assert!(matches!(parse(&[]).unwrap().command, Command::Help));
        assert!(matches!(parse(&["-V"]).unwrap().command, Command::Version));
        let Command::Run { media: m, .. } = parse(&[
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
        let Command::Run { media: m, .. } = parse(&[
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
        assert!(parse(&["run", "--input", "telepathy"]).is_err());
        assert!(parse(&["unattended"]).is_err(), "needs on/off");
        assert!(parse(&["unattended", "off", "--pin"]).is_err());
        assert!(parse(&["status", "--pin"]).is_err());
        assert!(matches!(
            parse(&["unattended", "on", "--pin"]).unwrap().command,
            Command::Unattended {
                enable: true,
                pin: true
            }
        ));
        assert!(matches!(
            parse(&["setup", "--server", "https://c.example", "--no-browser"])
                .unwrap()
                .command,
            Command::Setup {
                open_browser: false,
                install_service: true,
                ..
            }
        ));
        assert!(parse(&["setup"]).is_err(), "setup needs --server");
        assert!(matches!(
            parse(&["service", "install"]).unwrap().command,
            Command::Service(ServiceAction::Install)
        ));
        assert!(parse(&["service"]).is_err());
        assert!(matches!(
            parse(&["run", "--service"]).unwrap().command,
            Command::Run { service: true, .. }
        ));
        assert_eq!(
            parse(&["--log-file", "/tmp/a.log", "status"])
                .unwrap()
                .log_file,
            Some(PathBuf::from("/tmp/a.log"))
        );
    }

    #[test]
    fn default_name_is_printable() {
        let n = default_name();
        assert!(!n.is_empty() && n.chars().count() <= 64 && !n.chars().any(char::is_control));
    }
}
