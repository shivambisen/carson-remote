package ratelimit

import (
	"testing"
	"time"
)

func TestBurstThenRefill(t *testing.T) {
	now := time.Unix(0, 0)
	l := New(1, time.Second, 3).WithClock(func() time.Time { return now })
	for i := range 3 {
		if !l.Allow("ip") {
			t.Fatalf("event %d within burst was denied", i)
		}
	}
	if l.Allow("ip") {
		t.Fatal("event beyond burst was allowed")
	}
	if !l.Allow("other") {
		t.Fatal("keys must be independent")
	}
	now = now.Add(time.Second)
	if !l.Allow("ip") {
		t.Fatal("token should refill after 1s")
	}
	if l.Allow("ip") {
		t.Fatal("only one token should have refilled")
	}
}

func TestIdleBucketsAreCollected(t *testing.T) {
	now := time.Unix(0, 0)
	l := New(1, time.Second, 1).WithClock(func() time.Time { return now })
	l.Allow("a")
	now = now.Add(time.Hour)
	l.Allow("b")
	if _, ok := l.buckets["a"]; ok {
		t.Fatal("idle bucket was not collected")
	}
}
