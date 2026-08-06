package engine

import (
	"bytes"
	"io"
	"sync"
)

// lastWords passes a stream through unchanged while remembering the last few
// lines of it.
//
// **Why the supervisor reads the engine's stderr at all.** A supervisor that
// gives up says "the engine keeps failing to run: 5 times in a row, last: exit
// status 1" — a sentence containing everything except the reason. The engine
// said the reason; it went past on the way to the terminal, and nobody kept it.
//
// The cost is that the engine's stderr is a pipe rather than the terminal,
// which can reorder it slightly against the report on stdout. Every byte is
// written through as it arrives, unbuffered, to keep that as small as it can
// be — and the report itself is untouched, because it goes to stdout.
type lastWords struct {
	out  io.Writer
	most int

	mu      sync.Mutex
	lines   []string
	partial []byte
}

// keepLines is how many lines to remember, and howLong bounds each one.
//
// Ten because a Rust panic is a message and a location and sometimes a note
// about a backtrace, and the interesting part of any crash is at the end.
const (
	keepLines = 10
	howLong   = 200
)

func newLastWords(out io.Writer) *lastWords {
	return &lastWords{out: out, most: keepLines}
}

// Write sends everything on and keeps what it can read as lines.
//
// The write-through happens first: a supervisor's bookkeeping must not be able
// to delay or drop what somebody is watching a terminal for.
func (l *lastWords) Write(p []byte) (int, error) {
	n, err := l.out.Write(p)
	l.mu.Lock()
	defer l.mu.Unlock()
	l.partial = append(l.partial, p[:n]...)
	for {
		at := bytes.IndexByte(l.partial, '\n')
		if at < 0 {
			// A process printing without newlines must not be able to make its
			// supervisor allocate for it. The same bound the log relay uses.
			if len(l.partial) > maxLogLine {
				l.partial = l.partial[:maxLogLine]
			}
			break
		}
		l.keep(string(l.partial[:at]))
		l.partial = l.partial[at+1:]
	}
	return n, err
}

// keep records one line, dropping the oldest when full.
func (l *lastWords) keep(line string) {
	line = trimTo(line, howLong)
	if line == "" {
		return
	}
	l.lines = append(l.lines, line)
	if len(l.lines) > l.most {
		l.lines = l.lines[len(l.lines)-l.most:]
	}
}

// said returns the lines remembered, oldest first, including anything written
// without a closing newline — which is where a crash message often ends.
func (l *lastWords) said() []string {
	l.mu.Lock()
	defer l.mu.Unlock()
	out := make([]string, 0, len(l.lines)+1)
	out = append(out, l.lines...)
	if rest := trimTo(string(l.partial), howLong); rest != "" {
		out = append(out, rest)
	}
	return out
}

// last returns the final remembered line, or "" when the engine said nothing.
//
// One line rather than all of them, for a message a person reads: the whole of
// what was said is already on their terminal, and this is the pointer to it.
func (l *lastWords) last() string {
	said := l.said()
	if len(said) == 0 {
		return ""
	}
	return said[len(said)-1]
}

// trimTo shortens s to at most n bytes without splitting a UTF-8 character,
// and drops surrounding blanks.
func trimTo(s string, n int) string {
	s = string(bytes.TrimSpace([]byte(s)))
	if len(s) <= n {
		return s
	}
	// Back up off a continuation byte, so the cut lands where a character
	// starts.
	end := n
	for end > 0 && isContinuation(s[end]) {
		end--
	}
	return s[:end]
}

// isContinuation reports whether b is a 10xxxxxx byte, i.e. not the start of a
// character.
func isContinuation(b byte) bool { return b&0xC0 == 0x80 }
