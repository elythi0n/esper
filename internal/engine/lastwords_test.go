package engine

import (
	"bytes"
	"strings"
	"testing"
)

// Everything written goes through, byte for byte.
//
// The first property, because this sits in front of a terminal somebody is
// watching: a supervisor's bookkeeping must not change, delay or drop what the
// engine printed.
func TestEverythingWrittenGoesThrough(t *testing.T) {
	var out bytes.Buffer
	l := newLastWords(&out)

	for _, chunk := range []string{"host: bind", "ing 0.0.0.0", ":7645\nready\n"} {
		if _, err := l.Write([]byte(chunk)); err != nil {
			t.Fatalf("Write: %v", err)
		}
	}
	if got := out.String(); got != "host: binding 0.0.0.0:7645\nready\n" {
		t.Errorf("wrote %q, want it unchanged", got)
	}
}

// Only the last few lines are kept, so a process that prints for a month
// cannot make its supervisor grow.
func TestOnlyTheLastFewLinesAreKept(t *testing.T) {
	l := newLastWords(&bytes.Buffer{})
	for i := range 100 {
		if _, err := l.Write([]byte(strings.Repeat("x", 3) + string(rune('0'+i%10)) + "\n")); err != nil {
			t.Fatalf("Write: %v", err)
		}
	}
	if got := len(l.said()); got != keepLines {
		t.Errorf("kept %d lines, want %d", got, keepLines)
	}
	if last := l.last(); last != "xxx9" {
		t.Errorf("last() = %q, want the most recent line", last)
	}
}

// A crash message with no closing newline is still the last thing it said.
//
// This is the case that matters: a process dying mid-line is exactly when
// somebody wants to know what it was saying.
func TestAnUnfinishedLineIsStillWhatItSaid(t *testing.T) {
	l := newLastWords(&bytes.Buffer{})
	if _, err := l.Write([]byte("panic: capture backend went away")); err != nil {
		t.Fatalf("Write: %v", err)
	}
	if got := l.last(); got != "panic: capture backend went away" {
		t.Errorf("last() = %q, want the unfinished line", got)
	}
}

// One line cannot grow without bound either.
func TestOneVeryLongLineIsCutAtACharacterBoundary(t *testing.T) {
	l := newLastWords(&bytes.Buffer{})
	// Three-byte characters, so a naive cut lands in the middle of one.
	if _, err := l.Write([]byte(strings.Repeat("→", 500) + "\n")); err != nil {
		t.Fatalf("Write: %v", err)
	}
	got := l.last()
	if len(got) > howLong {
		t.Errorf("kept %d bytes, want at most %d", len(got), howLong)
	}
	if !isValidUTF8(got) {
		t.Errorf("the cut landed inside a character: %q", got)
	}
}

// isValidUTF8 without importing unicode/utf8 for one call.
func isValidUTF8(s string) bool {
	for _, r := range s {
		if r == '�' {
			return false
		}
	}
	return true
}

// Nothing is kept for a process that printed only blank lines, so a message
// does not end in a dangling dash.
func TestBlankLinesAreNotWhatItSaid(t *testing.T) {
	l := newLastWords(&bytes.Buffer{})
	if _, err := l.Write([]byte("\n\n   \n")); err != nil {
		t.Fatalf("Write: %v", err)
	}
	if got := l.last(); got != "" {
		t.Errorf("last() = %q, want nothing", got)
	}
	if got := quoted(l.last()); got != "" {
		t.Errorf("quoted() = %q, want nothing", got)
	}
}
