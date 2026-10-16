//! Wire protocol shared by `carson-agent` and the cross-language test vectors in `/proto`.
//!
//! See `docs/carson-design.md` §6 for the signaling protocol.

use serde::{Deserialize, Serialize};

pub mod input;

/// Signaling envelope version (`"v"` field). Bump on breaking changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum size of a single signaling message, enforced by the server.
pub const MAX_SIGNALING_MESSAGE_BYTES: usize = 64 * 1024;

/// Signaling message types exchanged over `/v1/ws`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageType {
    Hello,
    HelloOk,
    Presence,
    SessionRequest,
    SessionAccept,
    SessionReject,
    SdpOffer,
    SdpAnswer,
    IceCandidate,
    SessionIceRestart,
    SessionResume,
    SessionEnd,
    TurnCreds,
}

impl MessageType {
    pub const ALL: [MessageType; 13] = [
        Self::Hello,
        Self::HelloOk,
        Self::Presence,
        Self::SessionRequest,
        Self::SessionAccept,
        Self::SessionReject,
        Self::SdpOffer,
        Self::SdpAnswer,
        Self::IceCandidate,
        Self::SessionIceRestart,
        Self::SessionResume,
        Self::SessionEnd,
        Self::TurnCreds,
    ];

    /// The `"type"` string used on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::HelloOk => "hello.ok",
            Self::Presence => "presence",
            Self::SessionRequest => "session.request",
            Self::SessionAccept => "session.accept",
            Self::SessionReject => "session.reject",
            Self::SdpOffer => "sdp.offer",
            Self::SdpAnswer => "sdp.answer",
            Self::IceCandidate => "ice.candidate",
            Self::SessionIceRestart => "session.ice_restart",
            Self::SessionResume => "session.resume",
            Self::SessionEnd => "session.end",
            Self::TurnCreds => "turn.creds",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// Signaling frame: `{"v":1,"type":"…","id":"…","sid":"…","body":{…}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub body: serde_json::Value,
}

impl Envelope {
    pub fn new(kind: MessageType, body: impl Serialize) -> serde_json::Result<Self> {
        Ok(Self {
            v: PROTOCOL_VERSION,
            kind: kind.as_str().to_owned(),
            id: None,
            sid: None,
            body: serde_json::to_value(body)?,
        })
    }

    /// The parsed message type, or `None` for types this build does not know.
    pub fn message_type(&self) -> Option<MessageType> {
        MessageType::parse(&self.kind)
    }
}

/// Body of the agent's `hello`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentHello {
    pub agent_version: String,
    pub os: String,
}

/// Body of the server's `hello.ok` to an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentHelloOk {
    pub device_id: String,
    pub heartbeat_interval_s: u64,
}

/// One ICE server, as in the browser's `RTCIceServer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential: String,
}

/// Body of `turn.creds`: ICE servers for one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCreds {
    pub ice_servers: Vec<IceServer>,
    /// Lets this agent re-attach to the session after a signaling drop.
    #[serde(default)]
    pub resume_token: String,
}

/// Who is asking to connect (body of `session.request` sent to the agent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewerInfo {
    pub email: String,
    pub ip: String,
    pub user_agent: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionRequest {
    pub viewer: ViewerInfo,
}

/// Body of `session.reject` and `session.end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReason {
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// The exact bytes an agent signs with its device key to obtain an access token.
/// Must match `AuthMessage` in the Go server (checked against `proto/device-auth.json`).
pub fn device_auth_message(device_id: &str, nonce: &str) -> Vec<u8> {
    format!("carson-device-auth:v1\n{device_id}\n{nonce}").into_bytes()
}

/// First `a=fingerprint` of an SDP, normalized: lowercase algorithm, space, uppercase hex
/// (`sha-256 AB:CD:…`). The viewer extracts it the same way (see `proto/dtls-binding.json`).
pub fn sdp_fingerprint(sdp: &str) -> Option<String> {
    let value = sdp
        .lines()
        .find_map(|l| l.trim().strip_prefix("a=fingerprint:"))?;
    let (algo, hex) = value.trim().split_once(' ')?;
    Some(format!(
        "{} {}",
        algo.to_ascii_lowercase(),
        hex.trim().to_ascii_uppercase()
    ))
}

/// The bytes an agent signs to bind its DTLS certificate (by fingerprint) to its device key
/// for one session, so a viewer can tell the media really terminates at that device.
pub fn dtls_binding_message(sid: &str, fingerprint: &str) -> Vec<u8> {
    format!("carson-dtls:v1\n{sid}\n{fingerprint}").into_bytes()
}

/// The platform name used on the wire (`macos`, `windows`, `linux`).
pub const fn current_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn wire_names_round_trip() {
        for t in MessageType::ALL {
            assert_eq!(MessageType::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn wire_names_are_unique() {
        let names: HashSet<_> = MessageType::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(names.len(), MessageType::ALL.len());
    }

    #[test]
    fn matches_shared_fixture() {
        let fixture = include_str!("../../../../proto/message-types.txt");
        let names: Vec<&str> = fixture.lines().filter(|l| !l.is_empty()).collect();
        let ours: Vec<&str> = MessageType::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(
            ours, names,
            "update proto/message-types.txt and all implementations together"
        );
    }

    #[test]
    fn device_auth_message_matches_fixture() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../../proto/device-auth.json")).unwrap();
        let msg = device_auth_message(
            fixture["device_id"].as_str().unwrap(),
            fixture["nonce"].as_str().unwrap(),
        );
        assert_eq!(msg, fixture["message"].as_str().unwrap().as_bytes());
    }

    #[test]
    fn envelope_round_trip_matches_go_shape() {
        let env = Envelope::new(
            MessageType::Hello,
            AgentHello {
                agent_version: "0.1.0".into(),
                os: "macos".into(),
            },
        )
        .unwrap();
        let json = serde_json::to_string(&env).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"hello","body":{"agent_version":"0.1.0","os":"macos"}}"#
        );
        let back: Envelope = serde_json::from_str(json.as_str()).unwrap();
        assert_eq!(back.message_type(), Some(MessageType::Hello));

        // Unknown future types still parse; callers decide to ignore them.
        let future: Envelope = serde_json::from_str(r#"{"v":1,"type":"x.new"}"#).unwrap();
        assert_eq!(future.message_type(), None);
    }

    #[test]
    fn dtls_binding_matches_fixture() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../../../proto/dtls-binding.json")).unwrap();
        let fp = sdp_fingerprint(v["sdp"].as_str().unwrap()).unwrap();
        assert_eq!(fp, v["fingerprint"].as_str().unwrap());
        assert_eq!(
            dtls_binding_message(v["sid"].as_str().unwrap(), &fp),
            v["message"].as_str().unwrap().as_bytes()
        );
        assert_eq!(sdp_fingerprint("v=0\r\ns=-\r\n"), None);
    }

    #[test]
    fn unknown_type_is_rejected() {
        assert_eq!(MessageType::parse("sdp.offerx"), None);
        assert_eq!(MessageType::parse(""), None);
    }
}
