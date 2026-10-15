// Package signaling runs the /v1/ws WebSocket: agent presence (heartbeats) and, from
// milestone 3, session negotiation. All state is in memory on a single node.
package signaling

import (
	"sync"
)

// client is one WebSocket connection. Writes go through send so the hub never blocks
// on a slow socket; a full buffer drops the client.
type client struct {
	userID   string
	deviceID string // empty for viewers
	send     chan []byte
	closeFn  func(reason string)
	once     sync.Once
}

func newClient(userID, deviceID string, closeFn func(string)) *client {
	return &client{userID: userID, deviceID: deviceID, send: make(chan []byte, 32), closeFn: closeFn}
}

// enqueue queues msg for writing, or closes the client if it is not keeping up.
func (c *client) enqueue(msg []byte) {
	select {
	case c.send <- msg:
	default:
		c.close("slow consumer")
	}
}

func (c *client) close(reason string) {
	c.once.Do(func() { c.closeFn(reason) })
}

// Hub tracks live viewers (per user) and agents (per device).
type Hub struct {
	mu      sync.Mutex
	viewers map[string]map[*client]struct{} // user ID → viewers
	agents  map[string]*client              // device ID → agent
}

func NewHub() *Hub {
	return &Hub{viewers: map[string]map[*client]struct{}{}, agents: map[string]*client{}}
}

// Online reports whether the device has a live agent connection.
func (h *Hub) Online(deviceID string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	_, ok := h.agents[deviceID]
	return ok
}

func (h *Hub) addViewer(c *client) {
	h.mu.Lock()
	defer h.mu.Unlock()
	set := h.viewers[c.userID]
	if set == nil {
		set = map[*client]struct{}{}
		h.viewers[c.userID] = set
	}
	set[c] = struct{}{}
}

func (h *Hub) removeViewer(c *client) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if set := h.viewers[c.userID]; set != nil {
		delete(set, c)
		if len(set) == 0 {
			delete(h.viewers, c.userID)
		}
	}
}

// addAgent registers c as the device's agent and returns any connection it replaced.
func (h *Hub) addAgent(c *client) (replaced *client) {
	h.mu.Lock()
	defer h.mu.Unlock()
	replaced = h.agents[c.deviceID]
	h.agents[c.deviceID] = c
	return replaced
}

// removeAgent unregisters c, reporting whether it was still the device's current agent.
func (h *Hub) removeAgent(c *client) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.agents[c.deviceID] != c {
		return false
	}
	delete(h.agents, c.deviceID)
	return true
}

// toViewers sends msg to every viewer signed in as userID. Never crosses accounts.
func (h *Hub) toViewers(userID string, msg []byte) {
	h.mu.Lock()
	targets := make([]*client, 0, len(h.viewers[userID]))
	for c := range h.viewers[userID] {
		targets = append(targets, c)
	}
	h.mu.Unlock()
	for _, c := range targets {
		c.enqueue(msg)
	}
}
