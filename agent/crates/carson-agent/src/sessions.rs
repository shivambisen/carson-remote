//! Routes session signaling from the WebSocket to per-session actors (see `peer`).

use std::collections::HashMap;

use carson_proto::{AgentSessionRequest, Envelope, MessageType, SessionReason, TurnCreds};
use tokio::sync::mpsc;
use webrtc::peer_connection::{RTCIceCandidateInit, RTCIceServer, RTCSessionDescription};

use crate::media::{MediaOptions, open_source};
use crate::peer::{Outbox, SessionEvent, run_session, send};

pub struct Sessions {
    out: Outbox,
    media: MediaOptions,
    /// ICE servers announced via `turn.creds`, keyed by session, until the request arrives.
    pending_ice: HashMap<String, Vec<RTCIceServer>>,
    active: HashMap<String, mpsc::UnboundedSender<SessionEvent>>,
}

impl Sessions {
    pub fn new(out: Outbox, media: MediaOptions) -> Self {
        Self {
            out,
            media,
            pending_ice: HashMap::new(),
            active: HashMap::new(),
        }
    }

    /// Whether this envelope type belongs to a session.
    pub fn handles(kind: MessageType) -> bool {
        matches!(
            kind,
            MessageType::TurnCreds
                | MessageType::SessionRequest
                | MessageType::SdpAnswer
                | MessageType::IceCandidate
                | MessageType::SessionEnd
        )
    }

    pub fn handle(&mut self, env: Envelope) {
        self.active.retain(|_, tx| !tx.is_closed());
        let (Some(kind), Some(sid)) = (env.message_type(), env.sid.clone()) else {
            tracing::debug!(kind = %env.kind, "session message without sid ignored");
            return;
        };
        match kind {
            MessageType::TurnCreds => match serde_json::from_value::<TurnCreds>(env.body) {
                Ok(creds) => {
                    let servers = creds
                        .ice_servers
                        .into_iter()
                        .map(|s| RTCIceServer {
                            urls: s.urls,
                            username: s.username,
                            credential: s.credential,
                        })
                        .collect();
                    self.pending_ice.insert(sid, servers);
                }
                Err(e) => tracing::warn!(error = %e, "bad turn.creds"),
            },
            MessageType::SessionRequest => self.start(sid, env.body),
            MessageType::SdpAnswer => {
                match serde_json::from_value::<RTCSessionDescription>(env.body) {
                    Ok(answer) => self.dispatch(&sid, SessionEvent::Answer(Box::new(answer))),
                    Err(e) => tracing::warn!(error = %e, "bad sdp.answer"),
                }
            }
            MessageType::IceCandidate => {
                match serde_json::from_value::<RTCIceCandidateInit>(env.body) {
                    Ok(c) => self.dispatch(&sid, SessionEvent::Candidate(c)),
                    Err(e) => tracing::warn!(error = %e, "bad ice.candidate"),
                }
            }
            MessageType::SessionEnd => {
                self.dispatch(&sid, SessionEvent::RemoteEnd);
                self.active.remove(&sid);
            }
            _ => {}
        }
    }

    fn dispatch(&self, sid: &str, event: SessionEvent) {
        match self.active.get(sid) {
            Some(tx) => {
                let _ = tx.send(event);
            }
            None => tracing::debug!(sid, "event for unknown session ignored"),
        }
    }

    fn reject(&self, sid: &str, reason: &str, message: String) {
        send(
            &self.out,
            MessageType::SessionReject,
            sid,
            SessionReason {
                reason: reason.into(),
                message,
            },
        );
    }

    fn start(&mut self, sid: String, body: serde_json::Value) {
        let ice = self.pending_ice.remove(&sid).unwrap_or_default();
        let viewer = serde_json::from_value::<AgentSessionRequest>(body)
            .ok()
            .map(|r| r.viewer);
        if !self.active.is_empty() {
            self.reject(&sid, "busy", "this computer is already being viewed".into());
            return;
        }
        // M6 adds the consent prompt and "being viewed" banner; until then the agent logs it.
        match &viewer {
            Some(v) => {
                tracing::warn!(sid = %sid, viewer = %v.email, ip = %v.ip, "viewer connecting to this screen")
            }
            None => tracing::warn!(sid = %sid, "viewer connecting to this screen"),
        }

        let (tx, rx) = mpsc::unbounded_channel();
        self.active.insert(sid.clone(), tx.clone());
        let (out, media) = (self.out.clone(), self.media.clone());
        tokio::spawn(async move {
            // Opening the source checks OS permission; do it before accepting.
            let opts = media.clone();
            let source = tokio::task::spawn_blocking(move || open_source(&opts)).await;
            let capturer = match source {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    let denied = e
                        .chain()
                        .any(|c| c.is::<carson_capture::PermissionDenied>());
                    let (reason, message) = if denied {
                        (
                            "permission_denied",
                            carson_capture::PermissionDenied.to_string(),
                        )
                    } else {
                        ("capture_failed", format!("{e:#}"))
                    };
                    tracing::error!(sid = %sid, reason, error = %format!("{e:#}"), "cannot capture screen");
                    send(
                        &out,
                        MessageType::SessionReject,
                        &sid,
                        SessionReason {
                            reason: reason.into(),
                            message,
                        },
                    );
                    return;
                }
                Err(e) => {
                    send(
                        &out,
                        MessageType::SessionReject,
                        &sid,
                        SessionReason {
                            reason: "capture_failed".into(),
                            message: e.to_string(),
                        },
                    );
                    return;
                }
            };
            send(
                &out,
                MessageType::SessionAccept,
                &sid,
                serde_json::json!({}),
            );
            run_session(sid, ice, capturer, media, out, tx, rx).await;
        });
    }
}
