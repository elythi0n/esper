package main

import (
	"context"
	"errors"
	"net/http"
	"os/exec"
	"path/filepath"
	"sort"
	"sync"
	"time"

	"github.com/wailsapp/wails/v2/pkg/runtime"
)

/*
Dropping a file on the session, which sends it to the machine on the other end.

# Why the paths come from the window and not from the page

Because a webview hands JavaScript a `File`, and a `File` has no path. Sending
one from the page would mean reading the whole thing into the renderer, posting
it back to this process and writing it out again — a copy of somebody's file
through a browser, for a transfer the engine can do from the original.

Wails reports the *paths* of a drop, so this takes them and runs `esper cp`
against the file where it already is. The page's part is to say it is a target
and to show what is happening.

# What the far end has to agree to

Nothing here decides that. A transfer is its own session with its own
permission — `files-in` at the host — and `esper cp` negotiates it. A drop on
a machine that has not granted it fails with the host's own sentence, which is
what gets shown.
*/

// How long a transfer may take before this stops waiting for it.
//
// Generous: a file is as big as it is and a link is as fast as it is. It is
// here so a transfer that hangs cannot hold a slot for ever, not to bound
// anything a person would notice.
const transferPatience = 30 * time.Minute

// How long a finished transfer stays in the list the page reads.
//
// Long enough that a window which was in the background still shows what
// happened, short enough that a session's history does not grow without end.
const transferShownFor = 2 * time.Minute

// A file on its way to the far machine, as the page shows it.
type transfer struct {
	// Monotonic within this window, so the page can key a list on it.
	ID int `json:"id"`
	// The file's name, never its path: a path is this machine's business and
	// the page has no use for it.
	Name string `json:"name"`
	// "sending", "sent" or "failed".
	State string `json:"state"`
	// The reason, when it failed. The host's sentence where there is one.
	Trouble string `json:"trouble,omitempty"`
	started time.Time
	done    time.Time
}

// transfers is what this window has sent, and what it is sending.
type transfers struct {
	mu   sync.Mutex
	next int
	all  map[int]*transfer
}

func (t *transfers) begin(name string) *transfer {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.all == nil {
		t.all = map[int]*transfer{}
	}
	t.next++
	item := &transfer{ID: t.next, Name: name, State: "sending", started: time.Now()}
	t.all[item.ID] = item
	return item
}

func (t *transfers) finish(item *transfer, err error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	item.done = time.Now()
	if err != nil {
		item.State = "failed"
		item.Trouble = err.Error()
		return
	}
	item.State = "sent"
}

// list is what the page reads, oldest first, with the long-finished dropped.
func (t *transfers) list() []transfer {
	t.mu.Lock()
	defer t.mu.Unlock()
	out := make([]transfer, 0, len(t.all))
	for id, item := range t.all {
		if !item.done.IsZero() && time.Since(item.done) > transferShownFor {
			delete(t.all, id)
			continue
		}
		out = append(out, *item)
	}
	sort.Slice(out, func(a, b int) bool { return out[a].ID < out[b].ID })
	return out
}

// send copies one file to the device this window is showing.
func (t *transfers) send(front, device, path string) {
	item := t.begin(filepath.Base(path))
	// `<device>:` with no path after it means the far machine's own file
	// directory, which is the only place a transfer may land — see the
	// engine's `cp`, where the confinement is enforced rather than trusted.
	cmd := exec.Command(front, "cp", path, device+":") // #nosec G204 -- device is a fingerprint matched against aFingerprint, path comes from the window manager's drop, and front is resolved beside this binary
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.finish(item, errors.New(lastSentence(string(out))))
		return
	}
	t.finish(item, nil)
}

// lastSentence is the most useful line of a command's output.
//
// The engine says what went wrong on its last line — the host's own refusal
// where there is one — and the lines before it are what it was doing. A
// screen has room for the answer, not the transcript.
func lastSentence(said string) string {
	lines := []string{}
	for _, line := range splitLines(said) {
		if line != "" {
			lines = append(lines, line)
		}
	}
	if len(lines) == 0 {
		return "the transfer failed and said nothing"
	}
	return lines[len(lines)-1]
}

func splitLines(s string) []string {
	out := []string{}
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == '\n' {
			out = append(out, trimSpace(s[start:i]))
			start = i + 1
		}
	}
	out = append(out, trimSpace(s[start:]))
	return out
}

func trimSpace(s string) string {
	for len(s) > 0 && (s[0] == ' ' || s[0] == '\t' || s[0] == '\r') {
		s = s[1:]
	}
	for len(s) > 0 && (s[len(s)-1] == ' ' || s[len(s)-1] == '\t' || s[len(s)-1] == '\r') {
		s = s[:len(s)-1]
	}
	return s
}

// watchDrops sends anything dropped on the window to the session it is showing.
func (v *viewer) watchDrops(ctx context.Context) {
	runtime.OnFileDrop(ctx, func(_, _ int, paths []string) {
		device := v.sessions.showing()
		if device == "" {
			// Dropped on a window with no session. Nothing to send it to, and
			// nothing worth saying: the page only marks the *surface* as a
			// target, and there is no surface without a session.
			return
		}
		front, err := frontDoor()
		if err != nil {
			return
		}
		for _, path := range paths {
			go v.drops.send(front, device, path)
		}
	})
}

// routeTransfers lets the page read what this window is sending.
func (v *viewer) routeTransfers(mux *http.ServeMux) {
	mux.HandleFunc("GET /shell/transfers", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, map[string]any{"transfers": v.drops.list()})
	})
}
