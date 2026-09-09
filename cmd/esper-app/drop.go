package main

import (
	"context"
	"errors"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
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

// How much of the engine's output a window keeps, for the sentence a failure
// ends with and for the progress the page shows.
const tailKept = 4096

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
	// How far along, in bytes, as the engine reports it.
	//
	// **Read from `cp`'s own progress rather than by watching the file.** The
	// engine is what knows how much has been acknowledged at the far end, and
	// a window counting bytes off a disk would be reporting how fast it can
	// read rather than how fast the session can send.
	Bytes int64 `json:"bytes"`
	Total int64 `json:"total"`
	// The reason, when it failed. The host's sentence where there is one.
	Trouble string `json:"trouble,omitempty"`
	started time.Time
	done    time.Time
}

// progressOf reads `N of M bytes` out of whatever the engine has printed.
//
// The window starts `cp --progress`, which prints one plain line per report
// because a bar drawn with carriage returns is invisible through a pipe. The
// **last** pair in what has arrived is the current one. Returns false when
// there is no pair at all, which is every line before the first block goes
// out.
func progressOf(said string) (sent, total int64, ok bool) {
	for _, m := range progressLine.FindAllStringSubmatch(said, -1) {
		a, err1 := strconv.ParseInt(m[1], 10, 64)
		b, err2 := strconv.ParseInt(m[2], 10, 64)
		if err1 != nil || err2 != nil {
			continue
		}
		sent, total, ok = a, b, true
	}
	return sent, total, ok
}

// What a progress line carries. See `Progress` in `cp_cmd.rs`, which writes
// this shape both plainly and inside the bar a terminal gets.
var progressLine = regexp.MustCompile(`(\d+) of (\d+) bytes`)

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

// progress records how far along a transfer is.
func (t *transfers) progress(item *transfer, sent, total int64) {
	t.mu.Lock()
	defer t.mu.Unlock()
	item.Bytes = sent
	item.Total = total
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
	// A finished transfer is *all* of it, whatever the last progress line
	// happened to say: the engine stops drawing when the far end has it, and a
	// queue showing "sent" beside 98% would be a queue nobody believes.
	if item.Total > 0 {
		item.Bytes = item.Total
	}
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
	// **Bounded**, because a transfer that hangs holds this slot for as long
	// as the window lives and the list the page reads would show it sending
	// for ever. Generous on purpose: a file is as big as it is and a link is
	// as fast as it is, so this is a bound on a *stuck* transfer rather than
	// on a slow one.
	ctx, done := context.WithTimeout(context.Background(), transferPatience)
	defer done()
	cmd := exec.CommandContext(ctx, front, "cp", "--progress", path, device+":") // #nosec G204 -- device is a fingerprint matched against aFingerprint, path comes from the window manager's drop, and front is resolved beside this binary

	/*
		Read as it goes rather than collected at the end.

		`CombinedOutput` waits for the process, so a window using it knows
		nothing until the transfer is over — which for a large file is the
		whole time somebody is looking at it. The engine draws its progress to
		stderr as it goes, and reading that is how a window can say how far
		along a file is without counting bytes off a disk itself.
	*/
	// **One pipe for both streams**, which is what `CombinedOutput` gave. The
	// engine says everything on standard error so a script's standard output
	// is its own, but a window that read only one of them would be a window
	// that loses whichever sentence moves.
	said, writes, err := os.Pipe()
	if err != nil {
		t.finish(item, err)
		return
	}
	cmd.Stdout = writes
	cmd.Stderr = writes
	if err := cmd.Start(); err != nil {
		_ = writes.Close()
		_ = said.Close()
		t.finish(item, err)
		return
	}
	// Closed here, or the read below never reaches the end: the child holds
	// the only other copy once it has started.
	_ = writes.Close()
	defer func() { _ = said.Close() }()

	// The tail, for the sentence a failure ends with. Bounded: a transfer that
	// printed for an hour must not be held in memory by a window.
	//
	// Progress is read out of the tail rather than out of the chunk that just
	// arrived, because a read boundary falls wherever the pipe says it does —
	// `12345 of 67` in one read and `890 bytes` in the next matches nothing in
	// either half, and the tail is where the two are next to each other again.
	tail := make([]byte, 0, tailKept)
	buf := make([]byte, 1024)
	for {
		n, readErr := said.Read(buf)
		if n > 0 {
			tail = append(tail, buf[:n]...)
			if len(tail) > tailKept {
				tail = tail[len(tail)-tailKept:]
			}
			if sent, total, ok := progressOf(string(tail)); ok {
				t.progress(item, sent, total)
			}
		}
		if readErr != nil {
			break
		}
	}

	err = cmd.Wait()
	if ctx.Err() != nil {
		t.finish(item, errors.New("the transfer was still going after "+transferPatience.String()+", so it was stopped"))
		return
	}
	if err != nil {
		t.finish(item, errors.New(lastSentence(string(tail))))
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
