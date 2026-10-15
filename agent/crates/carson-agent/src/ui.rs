//! Host-side UI: the consent prompt and the always-on-top "being controlled" banner.
//!
//! Both run in a helper process (`carson-agent ui …`) spawned from `run`, so the GUI owns its
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
    let exe = std::env::current_exe().context("locate carson-agent executable")?;
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
            let allowed = native::prompt(get("--title")?, get("--message")?)?;
            std::process::exit(if allowed { EXIT_ALLOW } else { EXIT_DENY });
        }
        Some("banner") => {
            // If the agent goes away, so does the banner.
            std::thread::spawn(|| {
                let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
                std::process::exit(EXIT_ORPHANED);
            });
            native::banner(get("--text")?)
        }
        _ => bail!("usage: carson-agent ui prompt --title T --message M | ui banner --text T"),
    }
}

#[cfg(target_os = "macos")]
mod native {
    //! AppKit. The helper is an "accessory" app: no Dock icon, no menu bar.
    #![allow(unsafe_code)] // one target/action call; see SAFETY note.
    use anyhow::{Context, Result};
    use objc2::{MainThreadMarker, MainThreadOnly, sel};
    use objc2_app_kit::{
        NSAlert, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication,
        NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSColor, NSFont, NSPanel,
        NSScreen, NSStatusWindowLevel, NSTextField, NSWindowCollectionBehavior, NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    fn app(mtm: MainThreadMarker) -> objc2::rc::Retained<NSApplication> {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app
    }

    pub fn prompt(title: &str, message: &str) -> Result<bool> {
        let mtm = MainThreadMarker::new().context("prompt must run on the main thread")?;
        let app = app(mtm);
        app.activate();
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Warning);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(message));
        // Deny is first, so Return (the default button) denies; Allow needs a deliberate click.
        alert.addButtonWithTitle(&NSString::from_str("Deny"));
        alert.addButtonWithTitle(&NSString::from_str("Allow"));
        alert.window().setLevel(NSStatusWindowLevel);
        Ok(alert.runModal() == NSAlertSecondButtonReturn)
    }

    pub fn banner(text: &str) -> Result<()> {
        let mtm = MainThreadMarker::new().context("banner must run on the main thread")?;
        let app = app(mtm);
        let (w, h) = (560.0, 44.0);
        let screen = NSScreen::mainScreen(mtm)
            .context("no screen")?
            .visibleFrame();
        let rect = NSRect::new(
            NSPoint::new(
                screen.origin.x + (screen.size.width - w) / 2.0,
                screen.origin.y + screen.size.height - h - 8.0,
            ),
            NSSize::new(w, h),
        );
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        // Above normal and full-screen windows, on every Space; cannot be hidden by switching apps.
        panel.setLevel(NSStatusWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setHidesOnDeactivate(false);
        panel.setMovableByWindowBackground(true);
        panel.setBackgroundColor(Some(&NSColor::systemRedColor()));

        let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        label.setFrame(NSRect::new(
            NSPoint::new(14.0, 13.0),
            NSSize::new(w - 150.0, 18.0),
        ));
        // SAFETY: `terminate:` is a valid NSApplication action taking a sender.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Disconnect"),
                Some(&app),
                Some(sel!(terminate:)),
                mtm,
            )
        };
        button.setFrame(NSRect::new(
            NSPoint::new(w - 128.0, 8.0),
            NSSize::new(116.0, 28.0),
        ));
        let content = panel.contentView().context("panel has no content view")?;
        content.addSubview(&label);
        content.addSubview(&button);
        panel.orderFrontRegardless();
        app.run(); // returns only via terminate: (Disconnect), which exits the process
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod native {
    //! Win32. MessageBox for the prompt; a topmost tool window for the banner.
    #![allow(unsafe_code)] // Win32 FFI with stack-owned, fully initialized arguments.
    use anyhow::{Result, bail};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, HBRUSH};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW,
        GetSystemMetrics, HMENU, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND,
        MB_TOPMOST, MB_YESNO, MSG, MessageBoxW, PostQuitMessage, RegisterClassW, SM_CXSCREEN,
        TranslateMessage, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WNDCLASSW, WS_BORDER, WS_CHILD,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    pub fn prompt(title: &str, message: &str) -> Result<bool> {
        let text = HSTRING::from(format!("{message}\n\nAllow this connection?"));
        let caption = HSTRING::from(title);
        // SAFETY: valid null-terminated strings; no owner window.
        let r = unsafe {
            MessageBoxW(
                None,
                &text,
                &caption,
                MB_YESNO | MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND | MB_DEFBUTTON2,
            )
        };
        Ok(r == IDYES)
    }

    const DISCONNECT_ID: usize = 1;

    extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_COMMAND if (wparam.0 & 0xffff) == DISCONNECT_ID => {
                // SAFETY: called on the window's own thread.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            WM_CLOSE => LRESULT(0), // only Disconnect removes the banner
            // SAFETY: default handling for everything else.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    pub fn banner(text: &str) -> Result<()> {
        // SAFETY: standard window creation on this thread; all strings outlive the calls.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("CarsonBanner");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: class,
                hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as isize as *mut _),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                bail!("register banner window class");
            }
            let (w, h) = (560, 44);
            let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class,
                w!("carson"),
                WS_POPUP | WS_VISIBLE | WS_BORDER,
                x,
                8,
                w,
                h,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            let label = HSTRING::from(text);
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                PCWSTR(label.as_ptr()),
                WS_CHILD | WS_VISIBLE,
                12,
                13,
                w - 150,
                20,
                Some(hwnd),
                None,
                Some(instance.into()),
                None,
            )?;
            CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("Disconnect"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                w - 128,
                8,
                116,
                28,
                Some(hwnd),
                Some(HMENU(DISCONNECT_ID as *mut _)),
                Some(instance.into()),
                None,
            )?;
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod native {
    use anyhow::{Result, bail};

    pub fn prompt(_title: &str, _message: &str) -> Result<bool> {
        bail!("no consent prompt on this OS yet; use `carson-agent unattended on --pin`")
    }

    pub fn banner(_text: &str) -> Result<()> {
        bail!("no banner on this OS yet")
    }
}
