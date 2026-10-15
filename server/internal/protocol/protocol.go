// Package protocol defines the signaling wire format shared with carson-agent and carson-viewer.
// See docs/carson-design.md §6 and /proto for the language-neutral fixtures.
package protocol

// Version is the signaling envelope version ("v" field).
const Version = 1

// MaxMessageBytes caps a single signaling message.
const MaxMessageBytes = 64 << 10

// MessageType is the envelope "type" field.
type MessageType string

const (
	TypeHello             MessageType = "hello"
	TypeHelloOK           MessageType = "hello.ok"
	TypePresence          MessageType = "presence"
	TypeSessionRequest    MessageType = "session.request"
	TypeSessionAccept     MessageType = "session.accept"
	TypeSessionReject     MessageType = "session.reject"
	TypeSDPOffer          MessageType = "sdp.offer"
	TypeSDPAnswer         MessageType = "sdp.answer"
	TypeICECandidate      MessageType = "ice.candidate"
	TypeSessionICERestart MessageType = "session.ice_restart"
	TypeSessionResume     MessageType = "session.resume"
	TypeSessionEnd        MessageType = "session.end"
	TypeTURNCreds         MessageType = "turn.creds"
)

// AllTypes lists every message type in canonical (fixture) order.
var AllTypes = []MessageType{
	TypeHello, TypeHelloOK, TypePresence, TypeSessionRequest, TypeSessionAccept,
	TypeSessionReject, TypeSDPOffer, TypeSDPAnswer, TypeICECandidate,
	TypeSessionICERestart, TypeSessionResume, TypeSessionEnd, TypeTURNCreds,
}
