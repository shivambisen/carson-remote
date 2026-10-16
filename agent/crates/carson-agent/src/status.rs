//! Process-wide "who is connected right now", for the tray icon. Sessions publish here; the
//! tray (same process) subscribes. Being in-process, it can't be hidden from or spoofed by
//! another program.

use std::sync::LazyLock;

use tokio::sync::watch;

static LIVE: LazyLock<watch::Sender<Option<String>>> = LazyLock::new(|| watch::channel(None).0);

/// Subscribes to the label of the live session ("ana@x.com is controlling this computer").
#[cfg_attr(not(test), allow(dead_code))]
pub fn subscribe() -> watch::Receiver<Option<String>> {
    LIVE.subscribe()
}

/// Marks a session live until dropped.
pub struct LiveSession(());

impl LiveSession {
    pub fn start(label: &str) -> Self {
        LIVE.send_replace(Some(label.to_owned()));
        Self(())
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        LIVE.send_replace(None);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn live_while_held() {
        let rx = super::subscribe();
        let s = super::LiveSession::start("ana@x.com is viewing this computer");
        assert_eq!(
            rx.borrow().as_deref(),
            Some("ana@x.com is viewing this computer")
        );
        drop(s);
        assert_eq!(*rx.borrow(), None);
    }
}
