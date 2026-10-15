// Package protocol defines the signaling wire format shared with carson-agent and carson-viewer.
// See docs/carson-design.md §6 and /proto for the language-neutral fixtures.
package protocol

import (
	"encoding/json"
	"time"
)

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

// Envelope is the JSON frame for every signaling message.
type Envelope struct {
	V    int             `json:"v"`
	Type MessageType     `json:"type"`
	ID   string          `json:"id,omitempty"`
	SID  string          `json:"sid,omitempty"`
	Body json.RawMessage `json:"body,omitempty"`
}

// AgentHello is the body an agent sends right after connecting.
type AgentHello struct {
	AgentVersion string `json:"agent_version"`
	OS           string `json:"os"`
}

// AgentHelloOK is the server's reply to an agent hello.
type AgentHelloOK struct {
	DeviceID          string `json:"device_id"`
	HeartbeatInterval int    `json:"heartbeat_interval_s"`
}

// PresenceBody announces that a device came online or went offline.
type PresenceBody struct {
	DeviceID   string    `json:"device_id"`
	Online     bool      `json:"online"`
	LastSeenAt time.Time `json:"last_seen_at"`
}

// NewEnvelope marshals body into a v1 envelope.
func NewEnvelope(t MessageType, body any) ([]byte, error) {
	raw, err := json.Marshal(body)
	if err != nil {
		return nil, err
	}
	return json.Marshal(Envelope{V: Version, Type: t, Body: raw})
}
