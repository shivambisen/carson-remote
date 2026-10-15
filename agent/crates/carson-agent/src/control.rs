//! Remote control over the session's data channels: input injection, clipboard sync and
//! display switching.
//!
//! - `ptr` (unordered, no retransmits): pointer moves.
//! - `ctl` (reliable): buttons, wheel, keys, clipboard chunks (binary) and JSON control text.
//!
//! Injection runs on its own thread that owns the [`Injector`]; when the session ends the
//! channel closes, the injector drops, and every held key/button is released.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::Duration;

use bytes::BytesMut;
use carson_capture::DisplayInfo;
use carson_input::{Injector, Rect};
use carson_proto::input::{
    ClipboardAssembler, ControlMessage, DisplayDescriptor, InputMessage, clipboard_chunks,
};
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent};

use crate::media::{InputMode, MediaOptions, PipelineControl, list_displays};

enum InputCmd {
    Input(InputMessage),
    SetDisplay(Rect),
}

fn rect(d: &DisplayInfo) -> Rect {
    Rect {
        x: d.bounds.0,
        y: d.bounds.1,
        w: d.bounds.2,
        h: d.bounds.3,
    }
}

/// Starts the input thread. Returns the command sender and the input status for the viewer.
fn start_input_thread(
    mode: &InputMode,
    display: Rect,
) -> (Option<std_mpsc::Sender<InputCmd>>, ControlMessage) {
    let backend: anyhow::Result<Box<dyn carson_input::Backend>> = match mode {
        InputMode::Off => {
            return (
                None,
                ControlMessage::InputStatus {
                    available: false,
                    message: "this computer is shared view-only".into(),
                },
            );
        }
        InputMode::Log => Ok(Box::new(LogBackend)),
        InputMode::Native => carson_input::native_backend(),
        #[cfg(test)]
        InputMode::Record(events) => Ok(Box::new(carson_input::RecordingBackend {
            events: Arc::clone(events),
        })),
    };
    let backend = match backend {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "input injection unavailable");
            return (
                None,
                ControlMessage::InputStatus {
                    available: false,
                    message: format!("{e:#}"),
                },
            );
        }
    };
    let (tx, rx) = std_mpsc::channel::<InputCmd>();
    std::thread::Builder::new()
        .name("carson-input".into())
        .spawn(move || {
            let mut injector = Injector::new(backend, display);
            for cmd in rx {
                let result = match cmd {
                    InputCmd::Input(msg) => injector.handle(&msg),
                    InputCmd::SetDisplay(r) => {
                        injector.set_display(r);
                        Ok(())
                    }
                };
                if let Err(e) = result {
                    tracing::warn!(error = %format!("{e:#}"), "input injection failed");
                }
            }
            tracing::debug!("input thread exiting; releasing held input");
        })
        .expect("spawn input thread");
    (
        Some(tx),
        ControlMessage::InputStatus {
            available: true,
            message: String::new(),
        },
    )
}

/// Logs input instead of injecting it (`--input log`).
struct LogBackend;

impl carson_input::Backend for LogBackend {
    fn move_to(&mut self, x: f64, y: f64, held: u8) -> anyhow::Result<()> {
        tracing::trace!(x, y, held, "input: move");
        Ok(())
    }
    fn button(
        &mut self,
        button: u8,
        down: bool,
        x: f64,
        y: f64,
        clicks: u32,
    ) -> anyhow::Result<()> {
        tracing::info!(button, down, x, y, clicks, "input: button");
        Ok(())
    }
    fn scroll(&mut self, dx: i32, dy: i32) -> anyhow::Result<()> {
        tracing::info!(dx, dy, "input: scroll");
        Ok(())
    }
    fn key(&mut self, code: &str, down: bool) -> anyhow::Result<bool> {
        tracing::info!(code, down, "input: key");
        Ok(true)
    }
}

/// Text most recently written by either side, so changes are not echoed back.
type LastClipboard = Arc<Mutex<Option<String>>>;

/// Polls the host clipboard and forwards changes to the viewer. Existing contents at
/// session start are NOT sent; only later changes are.
fn start_clipboard_watcher(
    last: LastClipboard,
    stop: Arc<AtomicBool>,
    out: mpsc::UnboundedSender<String>,
) {
    std::thread::Builder::new()
        .name("carson-clipboard".into())
        .spawn(move || {
            let Ok(mut cb) = arboard::Clipboard::new() else {
                tracing::warn!("clipboard unavailable; sync disabled");
                return;
            };
            *last.lock().unwrap_or_else(|e| e.into_inner()) = cb.get_text().ok();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                let Ok(text) = cb.get_text() else { continue };
                let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
                if last.as_deref() != Some(text.as_str()) {
                    *last = Some(text.clone());
                    if out.send(text).is_err() {
                        break;
                    }
                }
            }
        })
        .expect("spawn clipboard thread");
}

fn set_host_clipboard(text: String, last: &LastClipboard) {
    *last.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.clone());
    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
        Ok(()) => tracing::info!("clipboard updated from viewer"),
        Err(e) => tracing::warn!(error = %e, "set clipboard failed"),
    }
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

async fn send_control(ctl: &Arc<dyn DataChannel>, msg: &ControlMessage) {
    match serde_json::to_string(msg) {
        Ok(text) => {
            if let Err(e) = ctl.send_text(&text).await {
                tracing::debug!(error = %e, "send control message");
            }
        }
        Err(e) => tracing::error!(error = %e, "serialize control message"),
    }
}

fn descriptors(displays: &[DisplayInfo]) -> Vec<DisplayDescriptor> {
    displays
        .iter()
        .map(|d| DisplayDescriptor {
            index: d.index,
            name: d.name.clone(),
            width: d.width,
            height: d.height,
            primary: d.primary,
        })
        .collect()
}

/// Runs remote control for one session until its data channels close.
pub async fn run(
    ptr: Arc<dyn DataChannel>,
    ctl: Arc<dyn DataChannel>,
    media: MediaOptions,
    pipeline: PipelineControl,
) {
    let opts = media.clone();
    let displays = tokio::task::spawn_blocking(move || list_displays(&opts))
        .await
        .ok()
        .and_then(|r| {
            r.map_err(|e| tracing::warn!(error = %format!("{e:#}"), "list displays"))
                .ok()
        })
        .unwrap_or_default();
    let mut current = media
        .capture
        .display_index
        .min(displays.len().saturating_sub(1));
    let display = displays.get(current).map(rect).unwrap_or(Rect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    });

    let (input_tx, status) = start_input_thread(&media.input, display);

    // Pointer moves on the unreliable channel.
    let ptr_task = {
        let input_tx = input_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = ptr.poll().await {
                let DataChannelEvent::OnMessage(m) = ev else {
                    continue;
                };
                if m.is_string {
                    continue;
                }
                if let (Ok(msg @ InputMessage::PointerMove { .. }), Some(tx)) =
                    (InputMessage::decode(&m.data), &input_tx)
                {
                    let _ = tx.send(InputCmd::Input(msg));
                }
            }
        })
    };

    let last_clip: LastClipboard = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_guard = StopOnDrop(Arc::clone(&stop));
    let (clip_tx, mut clip_rx) = mpsc::unbounded_channel::<String>();
    let clipboard = media.clipboard && matches!(media.input, InputMode::Native);
    if clipboard {
        start_clipboard_watcher(Arc::clone(&last_clip), Arc::clone(&stop), clip_tx);
    } else {
        drop(clip_tx);
    }
    let mut assembler = ClipboardAssembler::default();

    loop {
        tokio::select! {
            Some(text) = clip_rx.recv() => {
                for chunk in clipboard_chunks(&text) {
                    if let Err(e) = ctl.send(BytesMut::from(&chunk.encode()[..])).await {
                        tracing::debug!(error = %e, "send clipboard");
                        break;
                    }
                }
            }
            ev = ctl.poll() => {
                let Some(ev) = ev else { break };
                match ev {
                    DataChannelEvent::OnOpen => {
                        send_control(&ctl, &ControlMessage::Displays { displays: descriptors(&displays), current }).await;
                        send_control(&ctl, &status).await;
                    }
                    DataChannelEvent::OnClose => break,
                    DataChannelEvent::OnMessage(m) if m.is_string => {
                        match serde_json::from_slice::<ControlMessage>(&m.data) {
                            Ok(ControlMessage::SelectDisplay { index }) => {
                                if let Some(d) = displays.get(index) {
                                    current = index;
                                    *pipeline.switch_display.lock().unwrap_or_else(|e| e.into_inner()) = Some(index);
                                    if let Some(tx) = &input_tx {
                                        let _ = tx.send(InputCmd::SetDisplay(rect(d)));
                                    }
                                    send_control(&ctl, &ControlMessage::Displays { displays: descriptors(&displays), current }).await;
                                }
                            }
                            Ok(other) => tracing::debug!(?other, "unexpected control message"),
                            Err(e) => tracing::debug!(error = %e, "bad control message"),
                        }
                    }
                    DataChannelEvent::OnMessage(m) => match InputMessage::decode(&m.data) {
                        Ok(InputMessage::ClipboardChunk { last, data }) => {
                            if !clipboard {
                                continue;
                            }
                            if let Some(text) = assembler.push(last, &data) {
                                let last_clip = Arc::clone(&last_clip);
                                tokio::task::spawn_blocking(move || set_host_clipboard(text, &last_clip));
                            }
                        }
                        Ok(msg) => {
                            if let Some(tx) = &input_tx {
                                let _ = tx.send(InputCmd::Input(msg));
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "bad input message"),
                    },
                    _ => {}
                }
            }
        }
    }
    ptr_task.abort();
    // Dropping input_tx ends the input thread, which releases anything still held.
}
