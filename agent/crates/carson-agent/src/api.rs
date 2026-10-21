//! HTTP client for carson-server's agent endpoints (pairing and device auth).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;

use crate::identity::DeviceKey;

/// Errors the caller must treat differently from transient failures.
#[derive(Debug)]
pub enum ApiError {
    /// The server rejected this device (unknown, revoked, or wrong key). Re-pairing is required.
    DeviceRejected,
    Status {
        status: u16,
        code: String,
        message: String,
    },
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::DeviceRejected => write!(
                f,
                "the server no longer accepts this device; run `glenley-agent unpair` then `pair` again"
            ),
            ApiError::Status {
                status,
                code,
                message,
            } => write!(f, "server returned {status}: {code}: {message}"),
        }
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Deserialize)]
pub struct PairStart {
    pub pairing_id: String,
    pub user_code: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, PartialEq)]
pub enum PollStatus {
    Pending,
    Approved { device_id: String },
    Expired,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

/// Other addresses of known servers, tried when the main one can't be reached: some campus and
/// office networks block the glenley.in name while the host's own onrender.com name works.
const ALTERNATES: &[(&str, &str)] =
    &[("https://app.glenley.in", "https://carson-tgnu.onrender.com")];

pub struct Api {
    http: reqwest::Client,
    /// The configured server first, then its alternates.
    bases: Vec<String>,
    /// Index into `bases` of the address that last answered.
    active: AtomicUsize,
}

impl Api {
    pub fn new(server_url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("glenley-agent/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let main = server_url.trim_end_matches('/').to_owned();
        let mut bases = vec![main.clone()];
        bases.extend(
            ALTERNATES
                .iter()
                .filter(|(m, _)| *m == main)
                .map(|(_, alt)| (*alt).to_owned()),
        );
        Ok(Self {
            http,
            bases,
            active: AtomicUsize::new(0),
        })
    }

    /// The address currently in use (the configured server unless it was unreachable).
    pub fn base(&self) -> &str {
        &self.bases[self.active.load(Ordering::Relaxed)]
    }

    /// Sends a POST, starting with the address that last worked and moving to the next one only when
    /// the connection itself fails (an HTTP error answer means the server was reached).
    async fn send(
        &self,
        path: &str,
        build: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let first = self.active.load(Ordering::Relaxed);
        let mut last = None;
        for i in 0..self.bases.len() {
            let idx = (first + i) % self.bases.len();
            let req = build(self.http.post(format!("{}{path}", self.bases[idx])));
            match req.send().await {
                Ok(resp) => {
                    if idx != first {
                        tracing::info!(server = %self.bases[idx], "using alternate server address");
                    }
                    self.active.store(idx, Ordering::Relaxed);
                    return Ok(resp);
                }
                Err(e) if e.is_connect() || e.is_timeout() => last = Some(e),
                Err(e) => return Err(anyhow::Error::new(e).context(format!("POST {path}"))),
            }
        }
        Err(anyhow::Error::new(last.expect("at least one address")).context(format!("POST {path}")))
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: impl Serialize) -> Result<T> {
        let body = serde_json::to_value(body)?;
        let resp = self.send(path, |r| r.json(&body)).await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json().await?);
        }
        let (code, message) = match resp.json::<ErrorBody>().await {
            Ok(b) => (b.error.code, b.error.message),
            Err(_) => ("unknown".into(), status.to_string()),
        };
        Err(ApiError::Status {
            status: status.as_u16(),
            code,
            message,
        }
        .into())
    }

    /// `temporary`: shared "just this once" (the server removes it after one session).
    pub async fn pair_start(
        &self,
        key: &DeviceKey,
        name: &str,
        os: &str,
        temporary: bool,
    ) -> Result<PairStart> {
        self.post(
            "/v1/pair/start",
            json!({"public_key": key.public_key_b64(), "name": name, "os": os, "temporary": temporary}),
        )
        .await
    }

    /// Removes this computer from its account (proven with the device key).
    pub async fn leave(&self, device_id: &str, key: &DeviceKey) -> Result<()> {
        let token = self.access_token(device_id, key).await?;
        let resp = self
            .send("/v1/device/leave", |r| r.bearer_auth(&token))
            .await?;
        anyhow::ensure!(
            resp.status().is_success(),
            "server answered {}",
            resp.status()
        );
        Ok(())
    }

    /// Finishes a browser sign-in with the grant the browser handed to our loopback listener.
    pub async fn pair_complete(&self, pairing_id: &str, grant: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Resp {
            device_id: String,
        }
        let r: Resp = self
            .post(
                "/v1/pair/complete",
                json!({"pairing_id": pairing_id, "grant": grant}),
            )
            .await?;
        Ok(r.device_id)
    }

    pub async fn pair_poll(&self, pairing_id: &str) -> Result<PollStatus> {
        #[derive(Deserialize)]
        struct Resp {
            status: String,
            device_id: Option<String>,
        }
        match self
            .post::<Resp>("/v1/pair/poll", json!({"pairing_id": pairing_id}))
            .await
        {
            Ok(r) if r.status == "approved" => Ok(PollStatus::Approved {
                device_id: r.device_id.context("approved without device_id")?,
            }),
            Ok(_) => Ok(PollStatus::Pending),
            Err(e) if is_status(&e, 404) => Ok(PollStatus::Expired),
            Err(e) => Err(e),
        }
    }

    /// Proves possession of the device key and returns a short-lived access token.
    pub async fn access_token(&self, device_id: &str, key: &DeviceKey) -> Result<String> {
        #[derive(Deserialize)]
        struct Challenge {
            nonce: String,
        }
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
        }
        let ch: Challenge = self
            .post("/v1/device/challenge", json!({"device_id": device_id}))
            .await?;
        let sig = key.sign_auth(device_id, &ch.nonce);
        match self
            .post::<Token>(
                "/v1/device/token",
                json!({"device_id": device_id, "nonce": ch.nonce, "signature": sig}),
            )
            .await
        {
            Ok(t) => Ok(t.access_token),
            Err(e) if is_status(&e, 401) => Err(ApiError::DeviceRejected.into()),
            Err(e) => Err(e),
        }
    }

    /// The signaling WebSocket URL (`ws(s)://host/v1/ws`).
    /// Uses the address that last answered, so call it after an HTTP request (e.g. `access_token`).
    pub fn ws_url(&self) -> String {
        let base = self.base();
        let rest = base
            .strip_prefix("https://")
            .map(|r| format!("wss://{r}"))
            .or_else(|| base.strip_prefix("http://").map(|r| format!("ws://{r}")))
            .unwrap_or_else(|| base.to_owned());
        format!("{rest}/v1/ws")
    }
}

fn is_status(err: &anyhow::Error, want: u16) -> bool {
    matches!(err.downcast_ref::<ApiError>(), Some(ApiError::Status { status, .. }) if *status == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_maps_scheme() {
        assert_eq!(
            Api::new("https://c.example").unwrap().ws_url(),
            "wss://c.example/v1/ws"
        );
        assert_eq!(
            Api::new("http://localhost:8080/").unwrap().ws_url(),
            "ws://localhost:8080/v1/ws"
        );
    }

    #[test]
    fn known_servers_get_their_alternate() {
        let api = Api::new("https://app.glenley.in/").unwrap();
        assert_eq!(
            api.bases,
            ["https://app.glenley.in", "https://carson-tgnu.onrender.com"]
        );
        assert_eq!(api.base(), "https://app.glenley.in");
        assert_eq!(Api::new("https://c.example").unwrap().bases.len(), 1);
    }

    #[tokio::test]
    async fn falls_back_when_the_main_address_is_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = r#"{"nonce":"n"}"#;
            let _ = sock
                .write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes())
                .await;
        });
        // Port 9 on localhost: nothing listens, so the connection is refused.
        let mut api = Api::new("http://127.0.0.1:9").unwrap();
        api.bases.push(good.clone());
        #[derive(Deserialize)]
        struct Challenge {
            nonce: String,
        }
        let ch: Challenge = api.post("/v1/device/challenge", json!({})).await.unwrap();
        assert_eq!(ch.nonce, "n");
        assert_eq!(api.base(), good);
        assert!(api.ws_url().starts_with("ws://127.0.0.1:"));
    }
}
