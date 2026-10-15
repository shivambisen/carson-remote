//! Wire protocol shared by `carson-agent` and the cross-language test vectors in `/proto`.
//!
//! Milestone 1 only pins the protocol version and the signaling message type names
//! (see `docs/carson-design.md` §6). Serialization arrives with milestone 2.

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
    fn unknown_type_is_rejected() {
        assert_eq!(MessageType::parse("sdp.offerx"), None);
        assert_eq!(MessageType::parse(""), None);
    }
}
