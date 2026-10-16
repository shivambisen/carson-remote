//! HTTP client for carson-server's agent endpoints (pairing and device auth).

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

pub struct Api {
    http: reqwest::Client,
    base: String,
}

impl Api {
    pub fn new(server_url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("glenley-agent/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base: server_url.trim_end_matches('/').to_owned(),
        })
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: impl Serialize) -> Result<T> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
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

    pub async fn pair_start(&self, key: &DeviceKey, name: &str, os: &str) -> Result<PairStart> {
        self.post(
            "/v1/pair/start",
            json!({"public_key": key.public_key_b64(), "name": name, "os": os}),
        )
        .await
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
    pub fn ws_url(&self) -> String {
        let rest = self
            .base
            .strip_prefix("https://")
            .map(|r| format!("wss://{r}"))
            .or_else(|| {
                self.base
                    .strip_prefix("http://")
                    .map(|r| format!("ws://{r}"))
            })
            .unwrap_or_else(|| self.base.clone());
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
}
