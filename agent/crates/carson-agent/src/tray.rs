//! The tray app (`glenley`): a menu-bar / notification-area icon that keeps this computer
//! online and holds its settings, so nobody needs the command line.
//!
//! The tray owns the main thread (AppKit requires it, and Windows delivers tray messages to the
//! thread that created the icon). The agent runs on a Tokio runtime thread; the two talk over
//! channels. The icon always shows when someone is connected: that is the one sign of a session
//! that cannot be turned off (the on-screen pill and the Allow prompt are optional).

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::{config, service};

/// Server used by "Sign in" when none was given and this computer was never paired. Set at
/// build time (`CARSON_DEFAULT_SERVER=https://… cargo build`), e.g. by the release workflow.
const DEFAULT_SERVER: Option<&str> = option_env!("CARSON_DEFAULT_SERVER");

/// Tray → agent.
enum Cmd {
    SignIn,
    SignOut,
    Quit,
}

/// Agent → tray.
enum Note {
    /// Paired as this name (None: not signed in).
    Paired(Option<String>),
    /// A one-line problem to show in the menu.
    Problem(Option<String>),
    /// The agent has stopped; the app can exit.
    Stopped,
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Offline,
    Online,
    Live,
}

pub fn run(dir: PathBuf, server: Option<String>) -> Result<()> {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (note_tx, note_rx) = mpsc::channel();
    let agent_dir = dir.clone();
    let agent_server = server.clone();
    let agent = std::thread::Builder::new()
        .name("glenley-agent".into())
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("start runtime");
            rt.block_on(controller(agent_dir, agent_server, cmd_rx, note_tx.clone()));
            let _ = note_tx.send(Note::Stopped);
        })
        .context("start agent thread")?;

    let mut ui = Ui::new(&dir, server.clone())?;
    // First launch: go straight to the browser sign-in instead of waiting for a click.
    let unpaired = matches!(config::load(&dir), Ok(None));
    if unpaired && (server.is_some() || DEFAULT_SERVER.is_some()) {
        ui.signing_in = true;
        let _ = cmd_tx.send(Cmd::SignIn);
    }
    let result = event_loop(|| ui.tick(&dir, &cmd_tx, &note_rx));
    let _ = cmd_tx.send(Cmd::Quit);
    let _ = agent.join();
    result
}

/// Runs the agent: online while signed in, otherwise waits for "Sign in".
async fn controller(
    dir: PathBuf,
    server: Option<String>,
    mut cmds: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    notes: mpsc::Sender<Note>,
) {
    // The software codec is only needed without a hardware encoder; failure is not fatal.
    if let Err(e) = crate::codec::ensure_openh264().await {
        tracing::warn!(error = %format!("{e:#}"), "OpenH264 not installed");
    }
    loop {
        let cfg = match config::load(&dir) {
            Ok(c) => c,
            Err(e) => {
                let _ = notes.send(Note::Problem(Some(format!("Settings unreadable: {e:#}"))));
                None
            }
        };
        let _ = notes.send(Note::Paired(cfg.as_ref().map(|c| c.name.clone())));
        if cfg.is_some() {
            let _ = notes.send(Note::Problem(None));
            tokio::select! {
                r = crate::run(dir.clone(), crate::media::MediaOptions::default()) => {
                    let msg = r.err().map_or_else(|| "Stopped".to_owned(), |e| format!("{e:#}"));
                    tracing::error!(error = %msg, "agent stopped");
                    let _ = notes.send(Note::Problem(Some(short(&msg))));
                    // Wait for the user (sign out / quit) rather than retrying a fatal error.
                    match cmds.recv().await {
                        Some(Cmd::SignOut) => sign_out(&dir),
                        Some(Cmd::Quit) | None => return,
                        Some(Cmd::SignIn) => {}
                    }
                }
                c = cmds.recv() => match c {
                    Some(Cmd::SignOut) => sign_out(&dir),
                    Some(Cmd::Quit) | None => return,
                    Some(Cmd::SignIn) => {}
                },
            }
            continue;
        }
        match cmds.recv().await {
            Some(Cmd::SignIn) => {
                let Some(server) = server.clone().or_else(|| DEFAULT_SERVER.map(str::to_owned))
                else {
                    let _ = notes.send(Note::Problem(Some(
                        "No server configured: run `glenley-agent setup --server <URL>`".into(),
                    )));
                    continue;
                };
                let _ = notes.send(Note::Problem(Some(
                    "Finish signing in in your browser…".into(),
                )));
                tokio::select! {
                    r = crate::pair(dir.clone(), &server, None, true) => {
                        let _ = notes.send(Note::Problem(r.err().map(|e| short(&format!("{e:#}")))));
                    }
                    c = cmds.recv() => {
                        let _ = notes.send(Note::Problem(None));
                        if matches!(c, Some(Cmd::Quit) | None) { return; }
                    }
                }
            }
            Some(Cmd::Quit) | None => return,
            Some(Cmd::SignOut) => {}
        }
    }
}

fn sign_out(dir: &std::path::Path) {
    if let Err(e) = crate::unpair(dir.to_path_buf()) {
        tracing::error!(error = %format!("{e:#}"), "sign out failed");
    }
}

fn short(msg: &str) -> String {
    let line = msg.lines().next().unwrap_or(msg);
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(79).collect::<String>())
    } else {
        line.to_owned()
    }
}

struct Ui {
    tray: TrayIcon,
    status: MenuItem,
    sign_in: MenuItem,
    website: MenuItem,
    ask: CheckMenuItem,
    indicator: CheckMenuItem,
    login: CheckMenuItem,
    sign_out: MenuItem,
    quit: MenuItem,
    server: Option<String>,
    paired: Option<String>,
    problem: Option<String>,
    look: Look,
    last_status: String,
    /// "Sign in" was clicked and has not completed yet.
    signing_in: bool,
}

impl Ui {
    fn new(dir: &std::path::Path, server: Option<String>) -> Result<Self> {
        let cfg = config::load(dir).ok().flatten();
        let status = MenuItem::new("Starting…", false, None);
        let sign_in = MenuItem::new("Sign in…", true, None);
        let website = MenuItem::new("Open glenley website", true, None);
        let ask = CheckMenuItem::new(
            "Ask before each connection",
            cfg.is_some(),
            cfg.as_ref().is_some_and(|c| c.ask_before_connect),
            None,
        );
        let indicator = CheckMenuItem::new(
            "Show on-screen indicator",
            cfg.is_some(),
            cfg.as_ref().is_some_and(|c| c.show_indicator),
            None,
        );
        let login = CheckMenuItem::new("Start at login", true, service::is_installed(), None);
        let sign_out = MenuItem::new("Sign out of this computer", cfg.is_some(), None);
        let quit = MenuItem::new("Quit glenley", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &sign_in,
            &website,
            &PredefinedMenuItem::separator(),
            &ask,
            &indicator,
            &login,
            &PredefinedMenuItem::separator(),
            &sign_out,
            &quit,
        ])?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("glenley");
        // macOS: a monochrome template icon matches the menu bar (light and dark).
        #[cfg(target_os = "macos")]
        let tray = tray.with_icon_templated(icon(Look::Offline));
        #[cfg(not(target_os = "macos"))]
        let tray = tray.with_icon(icon(Look::Offline));
        let tray = tray.build().context("create tray icon")?;
        Ok(Self {
            tray,
            status,
            sign_in,
            website,
            ask,
            indicator,
            login,
            sign_out,
            quit,
            server: server.or_else(|| cfg.map(|c| c.server_url)),
            paired: None,
            problem: None,
            look: Look::Offline,
            last_status: String::new(),
            signing_in: false,
        })
    }

    /// Called every ~200 ms from the event loop. Returns false to quit.
    fn tick(
        &mut self,
        dir: &std::path::Path,
        cmds: &tokio::sync::mpsc::UnboundedSender<Cmd>,
        notes: &mpsc::Receiver<Note>,
    ) -> bool {
        while let Ok(note) = notes.try_recv() {
            match note {
                Note::Paired(name) => {
                    // Just signed in from this menu: start at login from now on, so the
                    // computer stays reachable after a restart (untick to opt out).
                    if self.signing_in && name.is_some() && !service::is_installed() {
                        if let Err(e) = service::install(dir) {
                            self.problem = Some(short(&format!("Start at login: {e:#}")));
                        }
                        self.login.set_checked(service::is_installed());
                    }
                    if name.is_some() {
                        self.signing_in = false;
                        self.server = config::load(dir)
                            .ok()
                            .flatten()
                            .map(|c| c.server_url)
                            .or(self.server.take());
                    }
                    self.paired = name;
                }
                Note::Problem(p) => self.problem = p,
                Note::Stopped => return false,
            }
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = ev.id();
            if id == self.quit.id() {
                return false;
            } else if id == self.sign_in.id() {
                self.signing_in = true;
                let _ = cmds.send(Cmd::SignIn);
            } else if id == self.sign_out.id() {
                let _ = cmds.send(Cmd::SignOut);
            } else if id == self.website.id() {
                let url = self
                    .server
                    .clone()
                    .or_else(|| DEFAULT_SERVER.map(str::to_owned));
                if let Some(url) = url {
                    service::open_browser(&format!("{url}/#/devices"));
                }
            } else if id == self.ask.id() {
                let on = self.ask.is_checked();
                self.save(dir, |c| c.ask_before_connect = on);
            } else if id == self.indicator.id() {
                let on = self.indicator.is_checked();
                self.save(dir, |c| c.show_indicator = on);
            } else if id == self.login.id() {
                let want = self.login.is_checked();
                let r = if want {
                    service::install(dir)
                } else {
                    service::uninstall()
                };
                if let Err(e) = r {
                    self.problem = Some(short(&format!("Start at login: {e:#}")));
                }
                self.login.set_checked(service::is_installed());
            }
        }
        self.refresh();
        true
    }

    fn save(&mut self, dir: &std::path::Path, change: impl FnOnce(&mut config::AgentConfig)) {
        if let Err(e) = crate::update_setting(dir, change) {
            self.problem = Some(short(&format!("{e:#}")));
        }
        if let Ok(Some(c)) = config::load(dir) {
            self.ask.set_checked(c.ask_before_connect);
            self.indicator.set_checked(c.show_indicator);
        }
    }

    fn refresh(&mut self) {
        let live = crate::status::live();
        let online = crate::status::online();
        let look = match (&live, online) {
            (Some(_), _) => Look::Live,
            (None, true) => Look::Online,
            _ => Look::Offline,
        };
        let status = match (&live, &self.paired) {
            (Some(label), _) => label.clone(),
            (None, None) => self
                .problem
                .clone()
                .unwrap_or_else(|| "Not signed in".to_owned()),
            (None, Some(name)) => match (&self.problem, online) {
                (Some(p), _) => p.clone(),
                (None, true) => format!("{name} · online"),
                (None, false) => format!("{name} · connecting…"),
            },
        };
        if status != self.last_status {
            self.status.set_text(&status);
            let _ = self.tray.set_tooltip(Some(format!("glenley: {status}")));
            self.last_status = status;
        }
        let signed_in = self.paired.is_some();
        self.sign_in.set_enabled(!signed_in);
        self.sign_out.set_enabled(signed_in);
        self.ask.set_enabled(signed_in);
        self.indicator.set_enabled(signed_in);
        if look != self.look {
            // macOS: monochrome template while idle (matches the menu bar); colour when live.
            #[cfg(target_os = "macos")]
            let _ = if look == Look::Live {
                self.tray.set_icon(Some(icon(look)))
            } else {
                self.tray.set_icon_templated(Some(icon(look)))
            };
            #[cfg(not(target_os = "macos"))]
            let _ = self.tray.set_icon(Some(icon(look)));
            self.look = look;
        }
    }
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

/// Pumps native events, calling `tick` about five times a second until it returns false.
#[cfg(target_os = "macos")]
fn event_loop(mut tick: impl FnMut() -> bool) -> Result<()> {
    #![allow(unsafe_code)] // reading one AppKit constant
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

    let mtm = MainThreadMarker::new().context("the tray must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    // Menu-bar only: no Dock icon, no app menu.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    loop {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.2);
        // SAFETY: NSDefaultRunLoopMode is an immutable framework constant.
        let mode = unsafe { NSDefaultRunLoopMode };
        while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            mode,
            true,
        ) {
            app.sendEvent(&event);
            if until.timeIntervalSinceNow() <= 0.0 {
                break;
            }
        }
        app.updateWindows();
        if !tick() {
            return Ok(());
        }
    }
}

#[cfg(target_os = "windows")]
fn event_loop(mut tick: impl FnMut() -> bool) -> Result<()> {
    #![allow(unsafe_code)] // Win32 message pump.
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
        TranslateMessage, WM_QUIT,
    };
    loop {
        let mut msg = MSG::default();
        // SAFETY: standard message pump on the thread that owns the tray icon's window.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = MsgWaitForMultipleObjects(None, false, 200, QS_ALLINPUT);
        }
        if !tick() {
            return Ok(());
        }
    }
}
