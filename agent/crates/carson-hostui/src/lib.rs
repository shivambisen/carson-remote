//! Native host UI for glenley-agent: the consent prompt and the "being controlled" banner.
//! Both block the calling (main) thread; glenley-agent runs them in a helper process.
//!
//! - [`prompt`] returns `Ok(true)` only for an explicit Allow.
//! - [`banner`] shows a small always-on-top pill and returns when Disconnect is clicked
//!   (optional: glenley-agent shows it only when "Show on-screen indicator" is on).

pub use native::{banner, info, prompt};

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

    /// A plain message with an OK button (e.g. the connection code).
    pub fn info(title: &str, message: &str) -> Result<()> {
        let mtm = MainThreadMarker::new().context("info must run on the main thread")?;
        let app = app(mtm);
        app.activate();
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Informational);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(message));
        alert.addButtonWithTitle(&NSString::from_str("OK"));
        alert.window().setLevel(NSStatusWindowLevel);
        alert.runModal();
        Ok(())
    }

    pub fn banner(text: &str) -> Result<()> {
        let mtm = MainThreadMarker::new().context("banner must run on the main thread")?;
        let app = app(mtm);
        // A small pill in the top-right corner (drag it anywhere).
        let (w, h) = (360.0, 30.0);
        let screen = NSScreen::mainScreen(mtm)
            .context("no screen")?
            .visibleFrame();
        let rect = NSRect::new(
            NSPoint::new(
                screen.origin.x + screen.size.width - w - 12.0,
                screen.origin.y + screen.size.height - h - 10.0,
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
        panel.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(0.12, 0.92)));

        let label = NSTextField::labelWithString(&NSString::from_str(&format!("● {text}")), mtm);
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        label.setFrame(NSRect::new(
            NSPoint::new(10.0, 7.0),
            NSSize::new(w - 112.0, 16.0),
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
            NSPoint::new(w - 98.0, 3.0),
            NSSize::new(92.0, 24.0),
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
        GetSystemMetrics, HMENU, IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK,
        MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MSG, MessageBoxW, PostQuitMessage, RegisterClassW,
        SM_CXSCREEN, TranslateMessage, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WNDCLASSW, WS_BORDER,
        WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
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

    /// A plain message with an OK button (e.g. the connection code).
    pub fn info(title: &str, message: &str) -> Result<()> {
        let text = HSTRING::from(message);
        let caption = HSTRING::from(title);
        // SAFETY: valid null-terminated strings; no owner window.
        unsafe {
            MessageBoxW(
                None,
                &text,
                &caption,
                MB_OK | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND,
            );
        }
        Ok(())
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
            // A small pill in the top-right corner.
            let (w, h) = (360, 32);
            let x = GetSystemMetrics(SM_CXSCREEN) - w - 16;
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class,
                w!("glenley"),
                WS_POPUP | WS_VISIBLE | WS_BORDER,
                x,
                16,
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
                10,
                8,
                w - 110,
                16,
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
                w - 96,
                4,
                88,
                24,
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
        bail!("no consent prompt on this OS yet; use `glenley-agent unattended on --pin`")
    }

    pub fn banner(_text: &str) -> Result<()> {
        bail!("no banner on this OS yet")
    }

    pub fn info(title: &str, message: &str) -> Result<()> {
        println!("{title}\n\n{message}");
        Ok(())
    }
}
