package protocol

import (
	"os"
	"slices"
	"strings"
	"testing"
)

func TestMatchesSharedFixture(t *testing.T) {
	raw, err := os.ReadFile("../../../proto/message-types.txt")
	if err != nil {
		t.Fatal(err)
	}
	var want []MessageType
	for _, line := range strings.Split(string(raw), "\n") {
		if line != "" {
			want = append(want, MessageType(line))
		}
	}
	if !slices.Equal(AllTypes, want) {
		t.Fatalf("AllTypes = %q\nfixture  = %q\nupdate proto/message-types.txt and all implementations together", AllTypes, want)
	}
}
