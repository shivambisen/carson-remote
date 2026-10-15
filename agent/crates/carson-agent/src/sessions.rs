//! Routes session signaling from the WebSocket to per-session actors (see `peer`).

use std::collections::HashMap;

use carson_proto::{AgentSessionRequest, Envelope, MessageType, SessionReason, TurnCreds};
use tokio::sync::mpsc;
use webrtc::peer_connection::{RTCIceCandidateInit, RTCIceServer, RTCSessionDescription};

use std::sync::{Arc, Mutex};

use carson_proto::ViewerInfo;

use crate::consent::{AccessPolicy, CONSENT_TIMEOUT, PinGuard};
use crate::control::PinGate;
use crate::media::{InputMode, MediaOptions, Source, open_source};
use crate::peer::{Outbox, SessionConfig, SessionEvent, run_session, send};
use crate::ui::{self, Consent};

pub struct Sessions {
    out: Outbox,
    media: MediaOptions,
    policy: AccessPolicy,
    /// Failed-PIN counter shared by all sessions (lockout survives reconnects).
    pins: Arc<Mutex<PinGuard>>,
    /// ICE servers announced via `turn.creds`, keyed by session, until the request arrives.
    pending_ice: HashMap<String, Vec<RTCIceServer>>,
    active: HashMap<String, mpsc::UnboundedSender<SessionEvent>>,
}

impl Sessions {
    pub fn new(out: Outbox, media: MediaOptions, policy: AccessPolicy) -> Self {
        Self {
            out,
            media,
            policy,
            pins: Arc::default(),
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
            .map(|r| r.viewer)
            .unwrap_or(ViewerInfo {
                email: "someone".into(),
                ip: "unknown".into(),
                user_agent: String::new(),
            });
        if !self.active.is_empty() {
            self.reject(&sid, "busy", "this computer is already in a session".into());
            return;
        }
        let gate = match &self.policy {
            AccessPolicy::Unattended {
                pin_hash: Some(hash),
            } => {
                if let Some(wait) = self
                    .pins
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .locked_for()
                {
                    let mins = wait.as_secs().div_ceil(60);
                    self.reject(
                        &sid,
                        "pin_locked",
                        format!("too many wrong PINs; try again in {mins} min"),
                    );
                    return;
                }
                Some(PinGate {
                    hash: hash.clone(),
                    guard: Arc::clone(&self.pins),
                })
            }
            _ => None,
        };
        let controlling = !matches!(self.media.input, InputMode::Off);
        let verb = if controlling { "control" } else { "view" };
        tracing::warn!(sid = %sid, viewer = %viewer.email, ip = %viewer.ip, "remote {verb} requested");

        let (tx, rx) = mpsc::unbounded_channel();
        self.active.insert(sid.clone(), tx.clone());
        let (out, media, policy) = (self.out.clone(), self.media.clone(), self.policy.clone());
        tokio::spawn(async move {
            if policy == AccessPolicy::Attended {
                let title = format!("Allow remote {verb}?");
                let message = consent_message(&viewer, verb);
                let decision = ui::ask_consent(&title, &message, CONSENT_TIMEOUT).await;
                tracing::warn!(sid = %sid, viewer = %viewer.email, ?decision, "consent decision");
                if let Some((reason, msg)) = rejection_for(&decision) {
                    send(
                        &out,
                        MessageType::SessionReject,
                        &sid,
                        SessionReason {
                            reason: reason.into(),
                            message: msg,
                        },
                    );
                    return;
                }
            }
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
                serde_json::json!({ "pin_required": gate.is_some() }),
            );
            // The banner is skipped only for a synthetic stream with no real input.
            let synthetic =
                media.source == Source::TestPattern && !matches!(media.input, InputMode::Native);
            let banner = (!synthetic).then(|| banner_text(&viewer, controlling));
            let cfg = SessionConfig {
                sid,
                ice_servers: ice,
                media,
                gate,
                banner,
            };
            run_session(cfg, capturer, out, tx, rx).await;
        });
    }
}

fn consent_message(viewer: &ViewerInfo, verb: &str) -> String {
    let browser: String = viewer.user_agent.chars().take(80).collect();
    format!(
        "{} wants to {verb} this computer.\n\nConnecting from {}{}.\n\nOnly allow this if you are expecting it.",
        viewer.email,
        viewer.ip,
        if browser.is_empty() {
            String::new()
        } else {
            format!("\n{browser}")
        },
    )
}

fn banner_text(viewer: &ViewerInfo, controlling: bool) -> String {
    format!(
        "{} is {} this computer",
        viewer.email,
        if controlling {
            "controlling"
        } else {
            "viewing"
        }
    )
}

/// Maps a consent decision to a session rejection (None = allowed).
fn rejection_for(decision: &Consent) -> Option<(&'static str, String)> {
    match decision {
        Consent::Allowed => None,
        Consent::Denied => Some(("declined", "the person at the computer declined".into())),
        Consent::TimedOut => Some(("consent_timeout", "no one answered at the computer".into())),
        Consent::Unavailable(e) => Some((
            "consent_unavailable",
            format!(
                "could not ask at the computer ({e}); set up `carson-agent unattended on --pin` for unattended access"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_allow_proceeds() {
        assert_eq!(rejection_for(&Consent::Allowed), None);
        assert_eq!(rejection_for(&Consent::Denied).unwrap().0, "declined");
        assert_eq!(
            rejection_for(&Consent::TimedOut).unwrap().0,
            "consent_timeout"
        );
        assert_eq!(
            rejection_for(&Consent::Unavailable("no GUI".into()))
                .unwrap()
                .0,
            "consent_unavailable"
        );
    }

    #[test]
    fn prompt_and_banner_name_the_viewer() {
        let v = ViewerInfo {
            email: "ana@x.com".into(),
            ip: "203.0.113.9".into(),
            user_agent: "Chrome".into(),
        };
        let m = consent_message(&v, "control");
        assert!(
            m.starts_with("ana@x.com wants to control this computer.")
                && m.contains("203.0.113.9")
                && m.contains("Chrome")
        );
        assert_eq!(
            banner_text(&v, true),
            "ana@x.com is controlling this computer"
        );
        assert_eq!(banner_text(&v, false), "ana@x.com is viewing this computer");
    }
}
