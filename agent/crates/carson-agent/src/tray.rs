//! The glenley app: a normal window (opened from the Start menu / Applications) plus a tray /
//! menu-bar icon, keeping this computer online in the background.
//!
//! - The window (tao + the system web view via wry) shows the embedded UI in `assets/app.html`:
//!   set up (sign in, or get a code), status, permissions and settings. It talks to Rust over
//!   the web view's IPC channel and receives state as JSON.
//! - Closing the window keeps glenley running; opening glenley again (second launch, Dock or
//!   tray click) brings it back. Only one copy runs: later launches just ask the first to show.
//! - The agent runs on a Tokio runtime thread; the window/tray own the main thread (AppKit
//!   requires it). The tray icon always changes while someone is connected.

use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::json;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use wry::{WebView, WebViewBuilder};

use crate::{config, service};

/// Server used by "Sign in" when none was given and this computer was never paired. Set at
/// build time (`CARSON_DEFAULT_SERVER=https://… cargo build`), e.g. by the release workflow.
const DEFAULT_SERVER: Option<&str> = option_env!("CARSON_DEFAULT_SERVER");

const APP_HTML: &str = include_str!("../assets/app.html");

/// The product name shown in the window, tray and (on Windows) Task Manager.
const DEFAULT_BRAND: &str = "glenley";

/// The display name for this install, taken from the executable's own file name so a copy the
/// Windows installer renamed (e.g. "Acme Support.exe") shows that name everywhere. The background
/// binary is named "<brand>-agent", so a trailing "-agent" is dropped. Falls back to "glenley".
fn brand() -> String {
    std::env::current_exe()
        .ok()
        .as_deref()
        .map(brand_from)
        .unwrap_or_else(|| DEFAULT_BRAND.to_owned())
}

fn brand_from(exe: &std::path::Path) -> String {
    exe.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.strip_suffix("-agent").unwrap_or(s).trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BRAND)
        .to_owned()
}

/// Loopback port the running copy listens on, so a second launch can ask it to show its window.
const INSTANCE_PORT: u16 = 47533;

/// Window → agent.
enum Cmd {
    SignIn,
    /// Show a code for someone else to enter; `once`: leave their account after one session.
    Code {
        once: bool,
    },
    /// Stop waiting for a sign-in or code.
    Cancel,
    /// Remove this computer from its account (server too) and forget it here.
    Leave,
    Quit,
}

/// Agent → window.
enum Note {
    /// Paired as this name (None: not set up).
    Paired(Option<String>),
    /// A one-line problem to show.
    Problem(Option<String>),
    /// Waiting for "signin" or "code" (None: idle).
    Busy(Option<&'static str>),
    /// The code to show, and whether it is a one-time share.
    Code(Option<String>, bool),
}

enum UserEvent {
    Note(Note),
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    /// Another launch asked us to show the window.
    Show,
    /// A message from the window's page.
    Ipc(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Offline,
    Online,
    Live,
}

/// Runs the app. `background`: start with the window hidden (login item).
pub fn run(dir: PathBuf, server: Option<String>, background: bool) -> Result<()> {
    // One copy only: if one is running, ask it to show its window and stop.
    // (GLENLEY_INSTANCE_PORT: tests that run beside an installed copy.)
    let port = std::env::var("GLENLEY_INSTANCE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(INSTANCE_PORT);
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(_) => {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = s.write_all(b"show\n");
                return Ok(());
            }
            anyhow::bail!("port {port} is in use by another program");
        }
    };

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    {
        let p = proxy.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if conn.is_ok() {
                    let _ = p.send_event(UserEvent::Show);
                }
            }
        });
    }
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Menu(e));
        }));
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Tray(e));
        }));
    }

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    {
        let (dir, server, p) = (dir.clone(), server.clone(), proxy.clone());
        std::thread::Builder::new()
            .name("glenley-agent".into())
            .spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("start runtime");
                rt.block_on(controller(dir, server, cmd_rx, p));
            })
            .context("start agent thread")?;
    }

    let brand = brand();
    let window = WindowBuilder::new()
        .with_title(&brand)
        .with_inner_size(LogicalSize::new(440.0, 700.0))
        .with_min_inner_size(LogicalSize::new(380.0, 520.0))
        .with_window_icon(window_icon())
        .with_visible(!background)
        .build(&event_loop)
        .context("open the glenley window")?;
    let webview = {
        let p = proxy.clone();
        WebViewBuilder::new()
            .with_html(APP_HTML.replace("__BRAND__", &brand))
            .with_ipc_handler(move |req| {
                let _ = p.send_event(UserEvent::Ipc(req.body().clone()));
            })
            .build(&window)
    };
    let webview = match webview {
        Ok(w) => Some(w),
        Err(e) => {
            // Windows without the WebView2 runtime: keep running from the tray.
            tracing::error!(error = %e, "cannot show the app window");
            None
        }
    };

    let mut app = App::new(dir, server, window, webview, cmd_tx);
    let mut quitting: Option<Instant> = None;
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));
        match event {
            Event::NewEvents(StartCause::Init) => {
                // macOS: the tray icon must be created once the event loop runs.
                app.tray = build_tray()
                    .map_err(|e| tracing::error!(error = %format!("{e:#}"), "tray icon"))
                    .ok();
                if background {
                    set_dock_icon(target, false);
                } else {
                    app.show(target);
                }
            }
            Event::UserEvent(UserEvent::Note(note)) => app.on_note(note),
            Event::UserEvent(UserEvent::Show) => app.show(target),
            Event::UserEvent(UserEvent::Ipc(msg)) => {
                if app.on_ipc(&msg) && quitting.is_none() {
                    quitting = Some(Instant::now());
                    let _ = app.cmds.send(Cmd::Quit);
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                match app.tray.as_ref().and_then(|t| t.item(&e)) {
                    Some(TrayItem::Open) => app.show(target),
                    Some(TrayItem::Quit) if quitting.is_none() => {
                        quitting = Some(Instant::now());
                        let _ = app.cmds.send(Cmd::Quit);
                    }
                    _ => {}
                }
            }
            Event::UserEvent(UserEvent::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            })) if cfg!(windows) => app.show(target),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => app.hide(target),
            Event::Reopen { .. } => app.show(target),
            _ => {}
        }
        if let Some(since) = quitting {
            // Give the agent a moment to end sessions cleanly.
            if since.elapsed() > Duration::from_millis(300) {
                *control_flow = ControlFlow::Exit;
                return;
            }
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(100));
        }
        app.refresh();
    })
}

/// Runs the agent: online while set up, otherwise waits for "Sign in" or "Get a code".
async fn controller(
    dir: PathBuf,
    server: Option<String>,
    mut cmds: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    proxy: EventLoopProxy<UserEvent>,
) {
    let note = |n: Note| {
        let _ = proxy.send_event(UserEvent::Note(n));
    };
    // The software codec is only needed without a hardware encoder; failure is not fatal.
    if let Err(e) = crate::codec::ensure_openh264().await {
        tracing::warn!(error = %format!("{e:#}"), "OpenH264 not installed");
    }
    loop {
        let cfg = match config::load(&dir) {
            Ok(c) => c,
            Err(e) => {
                note(Note::Problem(Some(format!("Settings unreadable: {e:#}"))));
                None
            }
        };
        note(Note::Paired(cfg.as_ref().map(|c| c.name.clone())));
        if cfg.is_some() {
            note(Note::Problem(None));
            tokio::select! {
                r = crate::run(dir.clone(), app_media()) => {
                    let err = r.err();
                    let removed = err.as_ref().is_some_and(|e| {
                        matches!(e.downcast_ref::<crate::api::ApiError>(), Some(crate::api::ApiError::DeviceRejected))
                    });
                    if removed {
                        // Removed on the website, or a one-time share ended: start over here.
                        tracing::info!("this computer was removed from its account");
                        sign_out(&dir);
                        note(Note::Problem(Some("This computer was removed from the account.".into())));
                        continue;
                    }
                    let msg = err.map_or_else(|| "Stopped".to_owned(), |e| format!("{e:#}"));
                    tracing::error!(error = %msg, "agent stopped");
                    note(Note::Problem(Some(short(&msg))));
                    match cmds.recv().await {
                        Some(Cmd::Leave) => leave(&dir, &note).await,
                        Some(Cmd::Quit) | None => return,
                        Some(_) => {}
                    }
                }
                c = cmds.recv() => match c {
                    Some(Cmd::Leave) => leave(&dir, &note).await,
                    Some(Cmd::Quit) | None => return,
                    Some(_) => {}
                },
            }
            continue;
        }
        let mut next = cmds.recv().await;
        // A new choice (e.g. a code instead of signing in) replaces the one in progress.
        while let Some(cmd @ (Cmd::SignIn | Cmd::Code { .. })) = next.take() {
            let Some(server) = server.clone().or_else(|| DEFAULT_SERVER.map(str::to_owned)) else {
                note(Note::Problem(Some(
                    "No server configured: run `glenley-agent setup --server <URL>`".into(),
                )));
                break;
            };
            let opts = match cmd {
                Cmd::Code { once } => {
                    let p = proxy.clone();
                    note(Note::Busy(Some("code")));
                    crate::PairOptions {
                        temporary: once,
                        on_code: Some(Box::new(move |code: &str| {
                            let _ = p.send_event(UserEvent::Note(Note::Code(
                                Some(code.to_owned()),
                                once,
                            )));
                        })),
                        ..Default::default()
                    }
                }
                _ => {
                    note(Note::Busy(Some("signin")));
                    crate::PairOptions {
                        open_browser: true,
                        ..Default::default()
                    }
                }
            };
            note(Note::Problem(None));
            tokio::select! {
                r = crate::pair_with(dir.clone(), &server, None, opts) => {
                    note(Note::Problem(r.err().map(|e| short(&format!("{e:#}")))));
                }
                c = cmds.recv() => match c {
                    Some(Cmd::Quit) | None => return,
                    Some(Cmd::Cancel) => {}
                    other => next = other,
                },
            }
            note(Note::Code(None, false));
            note(Note::Busy(None));
        }
        if matches!(next, Some(Cmd::Quit)) {
            return;
        }
    }
}

/// What the app shares: the screen with real input. `GLENLEY_TEST_MEDIA=1` (tests only) uses
/// the synthetic test pattern and logs input instead of injecting it.
fn app_media() -> crate::media::MediaOptions {
    let mut m = crate::media::MediaOptions::default();
    if std::env::var_os("GLENLEY_TEST_MEDIA").is_some() {
        m.source = crate::media::Source::TestPattern;
        m.input = crate::media::InputMode::Log;
    }
    m
}

fn sign_out(dir: &Path) {
    if let Err(e) = crate::unpair(dir.to_path_buf()) {
        tracing::error!(error = %format!("{e:#}"), "sign out failed");
    }
}

/// Removes this computer from its account, showing a spinner for however long that takes: it
/// calls the server, which can be slow on an unreliable connection.
async fn leave(dir: &Path, note: &impl Fn(Note)) {
    note(Note::Busy(Some("leave")));
    if let Err(e) = crate::leave(dir.to_path_buf()).await {
        tracing::error!(error = %format!("{e:#}"), "remove from account failed");
    }
    note(Note::Busy(None));
}

fn short(msg: &str) -> String {
    let line = msg.lines().next().unwrap_or(msg);
    if line.chars().count() > 100 {
        format!("{}…", line.chars().take(99).collect::<String>())
    } else {
        line.to_owned()
    }
}

enum TrayItem {
    Open,
    Quit,
}

struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    open: MenuItem,
    quit: MenuItem,
    brand: String,
}

impl Tray {
    fn item(&self, e: &MenuEvent) -> Option<TrayItem> {
        if e.id() == self.open.id() {
            Some(TrayItem::Open)
        } else if e.id() == self.quit.id() {
            Some(TrayItem::Quit)
        } else {
            None
        }
    }
}

fn build_tray() -> Result<Tray> {
    let brand = brand();
    let status = MenuItem::new("Starting…", false, None);
    let open = MenuItem::new(format!("Open {brand}"), true, None);
    let quit = MenuItem::new(format!("Quit {brand}"), true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &status, &PredefinedMenuItem::separator(), &quit])?;
    let builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(&brand);
    // macOS: a monochrome template icon matches the menu bar (light and dark).
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon(Look::Offline));
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon(Look::Offline));
    Ok(Tray {
        icon: builder.build().context("create tray icon")?,
        status,
        open,
        quit,
        brand,
    })
}

struct App {
    dir: PathBuf,
    server: Option<String>,
    window: Window,
    webview: Option<WebView>,
    cmds: tokio::sync::mpsc::UnboundedSender<Cmd>,
    tray: Option<Tray>,
    page_ready: bool,
    paired: Option<String>,
    problem: Option<String>,
    busy: Option<&'static str>,
    code: Option<String>,
    code_once: bool,
    look: Look,
    last_state: String,
    last_status: String,
    /// Cached, refreshed only when they can change: checking them runs programs (schtasks,
    /// launchctl) and reads files, which must not happen on every UI tick.
    login: bool,
    ask: bool,
    indicator: bool,
}

impl App {
    fn new(
        dir: PathBuf,
        server: Option<String>,
        window: Window,
        webview: Option<WebView>,
        cmds: tokio::sync::mpsc::UnboundedSender<Cmd>,
    ) -> Self {
        let server = server.or_else(|| config::load(&dir).ok().flatten().map(|c| c.server_url));
        let mut app = Self {
            dir,
            server,
            window,
            webview,
            cmds,
            tray: None,
            page_ready: false,
            paired: None,
            problem: None,
            busy: None,
            code: None,
            code_once: true,
            look: Look::Offline,
            last_state: String::new(),
            last_status: String::new(),
            login: false,
            ask: false,
            indicator: false,
        };
        app.reload_settings();
        app
    }

    /// Re-reads the settings file and the login-item state (runs a helper program).
    fn reload_settings(&mut self) {
        let cfg = config::load(&self.dir).ok().flatten();
        self.ask = cfg.as_ref().is_some_and(|c| c.ask_before_connect);
        self.indicator = cfg.as_ref().is_some_and(|c| c.show_indicator);
        self.login = service::is_installed();
    }

    fn show(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        if self.webview.is_none() {
            // No web view on this system: the website is the next best thing.
            if let Some(origin) = self.website_origin() {
                std::thread::spawn(move || {
                    let origin = crate::api::reachable_base(&origin);
                    service::open_browser(&format!("{origin}/#/devices"));
                });
            }
            return;
        }
        set_dock_icon(target, true);
        self.window.set_visible(true);
        self.window.set_minimized(false);
        self.window.set_focus();
    }

    fn hide(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        self.window.set_visible(false);
        set_dock_icon(target, false);
    }

    /// The configured server's origin (no path), for `/#/devices`.
    fn website_origin(&self) -> Option<String> {
        self.server
            .clone()
            .or_else(|| DEFAULT_SERVER.map(str::to_owned))
    }

    fn on_note(&mut self, note: Note) {
        match note {
            Note::Paired(name) => {
                if name.is_some() {
                    self.server = config::load(&self.dir)
                        .ok()
                        .flatten()
                        .map(|c| c.server_url)
                        .or(self.server.take());
                    // Just set up from this window: start at login from now on (untick to opt out).
                    if self.busy.is_some()
                        && !self.login
                        && let Err(e) = service::install(&self.dir)
                    {
                        self.problem = Some(short(&format!("Start at login: {e:#}")));
                    }
                }
                self.paired = name;
                self.reload_settings();
            }
            Note::Problem(p) => self.problem = p,
            Note::Busy(b) => self.busy = b,
            Note::Code(c, once) => {
                self.code = c;
                self.code_once = once;
            }
        }
    }

    /// Handles a message from the page. Returns true to quit.
    fn on_ipc(&mut self, msg: &str) -> bool {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) else {
            return false;
        };
        let flag = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str);
        match text("cmd").unwrap_or("") {
            "ready" => {
                self.page_ready = true;
                self.last_state.clear(); // push the full state now
            }
            "signin" => {
                let _ = self.cmds.send(Cmd::SignIn);
            }
            "code" => {
                let _ = self.cmds.send(Cmd::Code { once: flag("once") });
            }
            "cancel" => {
                let _ = self.cmds.send(Cmd::Cancel);
            }
            "leave" => {
                let _ = self.cmds.send(Cmd::Leave);
            }
            "quit" => return true,
            "website" => {
                if let Some(origin) = self.website_origin() {
                    // A quick TCP check off the UI thread: on networks that block the main
                    // domain outright, open the working address instead of a dead link.
                    std::thread::spawn(move || {
                        let origin = crate::api::reachable_base(&origin);
                        service::open_browser(&format!("{origin}/#/devices"));
                    });
                }
            }
            "copy" => {
                if let Some(t) = text("text")
                    && let Err(e) =
                        arboard::Clipboard::new().and_then(|mut c| c.set_text(t.to_owned()))
                {
                    self.problem = Some(format!("Copy failed: {e}"));
                }
            }
            "setting" => {
                let on = flag("value");
                let r = match text("key") {
                    Some("ask") => crate::update_setting(&self.dir, |c| c.ask_before_connect = on),
                    Some("indicator") => {
                        crate::update_setting(&self.dir, |c| c.show_indicator = on)
                    }
                    Some("login") if on => service::install(&self.dir),
                    Some("login") => service::uninstall(),
                    _ => Ok(()),
                };
                if let Err(e) = r {
                    self.problem = Some(short(&format!("{e:#}")));
                }
                self.reload_settings();
            }
            "permission" => open_permission_settings(text("which") == Some("screen")),
            "restart" => {
                // macOS re-checks permissions only for a new process: start a fresh copy, then
                // quit this one (the new copy starts once this one is gone).
                restart_self();
                return true;
            }
            _ => {}
        }
        false
    }

    /// Pushes changed state to the page and the tray icon.
    fn refresh(&mut self) {
        let live = crate::status::live();
        let online = crate::status::online();
        let permissions = if cfg!(target_os = "macos") {
            json!({
                "screen": carson_capture::screen_recording_allowed(false),
                "accessibility": carson_input::accessibility_allowed(false),
            })
        } else {
            serde_json::Value::Null
        };
        let state = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "paired": self.paired,
            "server": self.server,
            "online": online,
            "live": live,
            "problem": self.problem,
            "busy": self.busy,
            "code": self.code,
            "codeOnce": self.code_once,
            "permissions": permissions,
            "settings": {
                "ask": self.ask,
                "indicator": self.indicator,
                "login": self.login,
            },
        })
        .to_string();
        if self.page_ready
            && state != self.last_state
            && let Some(w) = &self.webview
        {
            let _ = w.evaluate_script(&format!("window.glenley && window.glenley.update({state})"));
            self.last_state = state;
        }

        let look = match (&live, online) {
            (Some(_), _) => Look::Live,
            (None, true) => Look::Online,
            _ => Look::Offline,
        };
        let status = match (&live, &self.paired, &self.code) {
            (Some(label), _, _) => label.clone(),
            (None, None, Some(code)) => format!("Your code: {code}"),
            (None, None, None) => "Not set up".to_owned(),
            (None, Some(name), _) if online => format!("{name} · online"),
            (None, Some(name), _) => format!("{name} · connecting…"),
        };
        let Some(tray) = &self.tray else { return };
        if status != self.last_status {
            tray.status.set_text(&status);
            let _ = tray
                .icon
                .set_tooltip(Some(format!("{}: {status}", tray.brand)));
            self.last_status = status;
        }
        if look != self.look {
            // macOS: monochrome template while idle (matches the menu bar); colour when live.
            #[cfg(target_os = "macos")]
            let _ = if look == Look::Live {
                tray.icon.set_icon(Some(icon(look)))
            } else {
                tray.icon.set_icon_templated(Some(icon(look)))
            };
            #[cfg(not(target_os = "macos"))]
            let _ = tray.icon.set_icon(Some(icon(look)));
            self.look = look;
        }
    }
}

/// macOS: show a Dock icon only while the window is open.
fn set_dock_icon(target: &EventLoopWindowTarget<UserEvent>, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopWindowTargetExtMacOS};
        target.set_activation_policy_at_runtime(if visible {
            ActivationPolicy::Regular
        } else {
            ActivationPolicy::Accessory
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (target, visible);
}

/// Starts a new copy of the app shortly after this one quits.
fn restart_self() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    #[cfg(target_os = "macos")]
    let spawned = {
        // Relaunch through the .app bundle, which is what macOS grants permissions to.
        let bundle = exe
            .ancestors()
            .nth(3)
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .map(Path::to_path_buf);
        let (script, arg) = match bundle {
            Some(app) => ("sleep 1; open -n \"$0\"", app),
            None => ("sleep 1; \"$0\" app", exe.clone()),
        };
        std::process::Command::new("/bin/sh")
            .args(["-c", script, &arg.display().to_string()])
            .spawn()
    };
    #[cfg(not(target_os = "macos"))]
    let spawned = std::process::Command::new(&exe).arg("app").spawn();
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not restart glenley");
    }
}

/// macOS: ask for the permission (the system prompt appears once) and open its settings pane.
fn open_permission_settings(screen: bool) {
    if screen {
        carson_capture::screen_recording_allowed(true);
        service::open_browser(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        );
    } else {
        carson_input::accessibility_allowed(true);
        service::open_browser(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        );
    }
}

/// The window icon (Windows title bar and taskbar): the mark in ink.
fn window_icon() -> Option<tao::window::Icon> {
    const N: usize = 32;
    const MASK: &[u8; N * N] = include_bytes!("../assets/tray-mask-32.alpha");
    let px = MASK.iter().flat_map(|&a| [0x0E, 0x12, 0x29, a]).collect();
    tao::window::Icon::from_rgba(px, N as u32, N as u32).ok()
}

/// The glenley mark at 32×32 (alpha mask), tinted per state. Live: orange; online: teal;
/// offline: grey (Windows) or the template outline (macOS).
fn icon(look: Look) -> Icon {
    const N: usize = 32;
    const MASK: &[u8; N * N] = include_bytes!("../assets/tray-mask-32.alpha");
    let ([r, g, b], opacity): ([u8; 3], u16) = match look {
        Look::Live => ([0xF9, 0x73, 0x16], 255),
        Look::Online if cfg!(target_os = "macos") => ([0, 0, 0], 255),
        Look::Online => ([0x14, 0xB8, 0xA6], 255),
        Look::Offline if cfg!(target_os = "macos") => ([0, 0, 0], 128),
        Look::Offline => ([0x9C, 0xA3, 0xAF], 255),
    };
    let px = MASK
        .iter()
        .flat_map(|&a| [r, g, b, (u16::from(a) * opacity / 255) as u8])
        .collect();
    Icon::from_rgba(px, N as u32, N as u32).expect("valid icon")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn brand_follows_the_executable_name() {
        assert_eq!(brand_from(Path::new("/x/glenley.exe")), "glenley");
        assert_eq!(brand_from(Path::new("/x/glenley-agent.exe")), "glenley");
        assert_eq!(brand_from(Path::new("/x/Acme Support.exe")), "Acme Support");
        assert_eq!(
            brand_from(Path::new("/x/Acme Support-agent.exe")),
            "Acme Support"
        );
        assert_eq!(brand_from(Path::new("/x/glenley")), "glenley");
        assert_eq!(brand_from(Path::new("/x/-agent.exe")), DEFAULT_BRAND);
    }
}
