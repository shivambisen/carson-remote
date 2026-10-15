//! Keeps the agent connected to carson-server's signaling WebSocket so the device shows
//! as online, and carries session signaling. Reconnects with backoff; stops only on errors
//! that retrying cannot fix. Running sessions survive signaling reconnects (media is P2P).

use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest, http};

use carson_proto::{AgentHello, AgentHelloOk, Envelope, MessageType};

use crate::api::{Api, ApiError};
use crate::backoff::Backoff;
use crate::identity::DeviceKey;
use crate::media::MediaOptions;
use crate::sessions::Sessions;
use tokio::sync::mpsc;

/// Server close code: another agent connected with this device's identity.
const CLOSE_REPLACED: u16 = 4000;
/// How often we ping the server (keeps NAT mappings alive, detects dead links).
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// No frame at all for this long means the connection is dead.
const READ_TIMEOUT: Duration = Duration::from_secs(45);
/// A connection that lasted this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(30);

/// Why a connection attempt ended.
#[derive(Debug)]
enum Ended {
    /// Retry after backoff.
    Transient(anyhow::Error),
    /// Retrying cannot help; surface to the user.
    Fatal(anyhow::Error),
}

pub struct Agent {
    api: Api,
    key: DeviceKey,
    device_id: String,
    sessions: std::sync::Mutex<Sessions>,
    /// Outgoing signaling, kept across reconnects.
    outbox: tokio::sync::Mutex<mpsc::UnboundedReceiver<Envelope>>,
}

impl Agent {
    pub fn new(
        api: Api,
        key: DeviceKey,
        device_id: String,
        media: MediaOptions,
        policy: crate::consent::AccessPolicy,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            api,
            key,
            device_id,
            sessions: std::sync::Mutex::new(Sessions::new(tx, media, policy)),
            outbox: tokio::sync::Mutex::new(rx),
        }
    }

    /// Runs until a fatal error. Cancel by dropping the future (e.g. on Ctrl-C).
    pub async fn run(&self) -> Result<()> {
        let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(10));
        loop {
            let started = Instant::now();
            match self.connect_once().await {
                Ended::Fatal(e) => return Err(e),
                Ended::Transient(e) => {
                    if started.elapsed() >= STABLE_AFTER {
                        backoff.reset();
                    }
                    let delay = backoff.next_delay();
                    tracing::warn!(error = %format!("{e:#}"), retry_in_ms = delay.as_millis() as u64, "disconnected");
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    async fn connect_once(&self) -> Ended {
        let token = match self.api.access_token(&self.device_id, &self.key).await {
            Ok(t) => t,
            Err(e) if matches!(e.downcast_ref::<ApiError>(), Some(ApiError::DeviceRejected)) => {
                return Ended::Fatal(e);
            }
            Err(e) => return Ended::Transient(e.context("authenticate device")),
        };
        let mut req = match self.api.ws_url().into_client_request() {
            Ok(r) => r,
            Err(e) => {
                return Ended::Fatal(anyhow::Error::new(e).context("build WebSocket request"));
            }
        };
        req.headers_mut().insert(
            http::header::AUTHORIZATION,
            match http::HeaderValue::from_str(&format!("Bearer {token}")) {
                Ok(v) => v,
                Err(e) => return Ended::Fatal(e.into()),
            },
        );

        let (mut ws, _) = match tokio_tungstenite::connect_async(req).await {
            Ok(ok) => ok,
            Err(tungstenite::Error::Http(resp))
                if resp.status() == http::StatusCode::UNAUTHORIZED =>
            {
                // Token was just minted, so this means the device was revoked meanwhile.
                return Ended::Fatal(ApiError::DeviceRejected.into());
            }
            Err(e) => return Ended::Transient(anyhow::Error::new(e).context("connect WebSocket")),
        };

        let hello = Envelope::new(
            MessageType::Hello,
            AgentHello {
                agent_version: env!("CARGO_PKG_VERSION").into(),
                os: carson_proto::current_os().into(),
            },
        )
        .and_then(|env| serde_json::to_string(&env));
        let hello = match hello {
            Ok(h) => h,
            Err(e) => return Ended::Fatal(e.into()),
        };
        if let Err(e) = ws.send(Message::text(hello)).await {
            return Ended::Transient(e.into());
        }

        let mut ping = tokio::time::interval(PING_INTERVAL);
        ping.tick().await; // first tick is immediate
        let mut outbox = self.outbox.lock().await;
        loop {
            tokio::select! {
                Some(env) = outbox.recv() => {
                    let text = match serde_json::to_string(&env) {
                        Ok(t) => t,
                        Err(e) => return Ended::Fatal(e.into()),
                    };
                    if let Err(e) = ws.send(Message::text(text)).await {
                        return Ended::Transient(e.into());
                    }
                }
                _ = ping.tick() => {
                    if let Err(e) = ws.send(Message::Ping(Default::default())).await {
                        return Ended::Transient(e.into());
                    }
                }
                frame = tokio::time::timeout(READ_TIMEOUT, ws.next()) => {
                    let msg = match frame {
                        Err(_) => return Ended::Transient(anyhow::anyhow!("no traffic for {READ_TIMEOUT:?}")),
                        Ok(None) => return Ended::Transient(anyhow::anyhow!("connection closed")),
                        Ok(Some(Err(e))) => return Ended::Transient(e.into()),
                        Ok(Some(Ok(m))) => m,
                    };
                    match msg {
                        Message::Text(text) => self.handle_text(&text),
                        Message::Close(frame) => {
                            let code = frame.as_ref().map(|f| u16::from(f.code));
                            if code == Some(CLOSE_REPLACED) {
                                return Ended::Fatal(anyhow::anyhow!(
                                    "another carson-agent connected as this device; only one may run at a time"
                                ));
                            }
                            return Ended::Transient(anyhow::anyhow!("server closed connection: {frame:?}"));
                        }
                        // Pongs to server pings are queued by tungstenite and flushed on the next read/write.
                        Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                    }
                }
            }
        }
    }

    fn handle_text(&self, text: &str) {
        let env: Envelope = match serde_json::from_str(text) {
            Ok(env) => env,
            Err(e) => {
                tracing::warn!(error = %e, "ignoring malformed message");
                return;
            }
        };
        match env.message_type() {
            Some(MessageType::HelloOk) => match serde_json::from_value::<AgentHelloOk>(env.body) {
                Ok(ok) => {
                    tracing::info!(device_id = %ok.device_id, heartbeat_s = ok.heartbeat_interval_s, "online")
                }
                Err(e) => tracing::warn!(error = %e, "bad hello.ok"),
            },
            Some(kind) if Sessions::handles(kind) => {
                self.sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .handle(env);
            }
            _ => tracing::debug!(kind = %env.kind, "ignoring message"),
        }
    }
}
