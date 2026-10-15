// Package ratelimit is a small in-memory, per-key token bucket limiter (single node).
package ratelimit

import (
	"sync"
	"time"
)

// Limiter allows `burst` events immediately and refills at `per` events per `interval`.
type Limiter struct {
	mu      sync.Mutex
	rate    float64 // tokens per second
	burst   float64
	buckets map[string]*bucket
	now     func() time.Time
	lastGC  time.Time
	idleTTL time.Duration
}

type bucket struct {
	tokens float64
	last   time.Time
}

// New returns a limiter allowing `per` events every `interval`, with bursts up to `burst`.
func New(per int, interval time.Duration, burst int) *Limiter {
	return &Limiter{
		rate:    float64(per) / interval.Seconds(),
		burst:   float64(burst),
		buckets: map[string]*bucket{},
		now:     time.Now,
		idleTTL: 10 * time.Minute,
	}
}

// WithClock overrides the time source (tests).
func (l *Limiter) WithClock(now func() time.Time) *Limiter {
	l.now = now
	return l
}

// Allow consumes one token for key and reports whether the event may proceed.
func (l *Limiter) Allow(key string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	now := l.now()
	if now.Sub(l.lastGC) > l.idleTTL {
		for k, b := range l.buckets {
			if now.Sub(b.last) > l.idleTTL {
				delete(l.buckets, k)
			}
		}
		l.lastGC = now
	}
	b, ok := l.buckets[key]
	if !ok {
		b = &bucket{tokens: l.burst, last: now}
		l.buckets[key] = b
	}
	b.tokens = min(l.burst, b.tokens+now.Sub(b.last).Seconds()*l.rate)
	b.last = now
	if b.tokens < 1 {
		return false
	}
	b.tokens--
	return true
}
