package main

import (
	"bufio"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// lockName is the socket that says a window is already open.
const lockName = "app.lock"

// longestSocketPath is what a unix socket's path may be.
//
// `sockaddr_un.sun_path` is 108 bytes on Linux and 104 on the BSDs including
// macOS, including the terminator — so the smaller of the two, with room to
// spare, is the number to hold to. Nothing in Go says this: `net.Listen`
// hands the path to the kernel and reports back `invalid argument`, which is
// the least informative true thing it could say.
const longestSocketPath = 100

// lockPath is where the socket for a home lives.
//
// # Why it is not always inside the home
//
// It should be: one window per Esper home is exactly what the lock means, and
// a file in the home says so without needing a convention. But a socket path
// is bounded at about a hundred bytes, and a home can be anywhere — a deeply
// nested directory, a container mount, a path with a long project name in it.
// Found the hard way: run the window with a home a hundred and twenty
// characters deep and it **refuses to start**, reporting
// `bind: invalid argument`, which sends whoever reads it looking at
// permissions.
//
// So when the obvious path is too long, the socket moves somewhere short and
// takes its meaning with it: the name is a hash of the home, so "one window
// per home" still holds and two homes still get two locks. The runtime
// directory first, because it is per-user and cleaned on logout; `/tmp`
// after, because a machine without one still has to work.
func lockPath(home string) string {
	return socketPath(home, lockName)
}

// socketPath is where a socket for a home lives, with the same fallback.
//
// Written once and used twice: the single-instance lock, and the socket a
// session's pictures arrive on. The second one found the same wall from the
// other side — an engine dialling a path under a long home reports
// `path must be shorter than SUN_LEN`, which is the same failure wearing a
// clearer message.
func socketPath(home, name string) string {
	inside := filepath.Join(home, name)
	if len(inside) <= longestSocketPath {
		return inside
	}
	sum := sha256.Sum256([]byte(home))
	short := "esper-" + hex.EncodeToString(sum[:8]) + "-" + name
	if run := os.Getenv("XDG_RUNTIME_DIR"); run != "" {
		if elsewhere := filepath.Join(run, short); len(elsewhere) <= longestSocketPath {
			return elsewhere
		}
	}
	return filepath.Join(os.TempDir(), short)
}

// onlyOne holds a lock for as long as this process runs.
type onlyOne struct {
	listener net.Listener
	path     string
}

// single takes the single-instance lock, or says who has it.
//
// # Why a unix socket and not a pid file
//
// A pid file is a claim about a process that may have died; checking it means
// reading a number and asking whether that process is still alive and is
// still this program, and the answer is racy on every platform. **A listening
// socket cannot lie**: the bind succeeds or it does not, and when the holder
// exits — cleanly, killed, or crashed — the kernel closes it and the next
// bind succeeds.
//
// The stale-file case is the one thing that needs handling: a socket file
// left behind by a crash refuses a bind with "address already in use" even
// though nobody is listening. So a failed bind is followed by a dial, and
// only a dial that *connects* means somebody is really there.
func single(home string) (*onlyOne, error) {
	path := lockPath(home)

	listener, err := net.Listen("unix", path)
	if err == nil {
		return &onlyOne{listener: listener, path: path}, nil
	}

	// Somebody may be there, or a crash may have left the file. Ask.
	conn, dialErr := net.Dial("unix", path)
	if dialErr == nil {
		_ = conn.Close()
		return nil, errAlreadyOpen
	}

	// Nobody answered, so the file is a leftover. Remove it and try once more.
	if removeErr := os.Remove(path); removeErr != nil && !errors.Is(removeErr, fs.ErrNotExist) {
		return nil, fmt.Errorf("clearing a stale %s: %w", path, removeErr)
	}
	listener, err = net.Listen("unix", path)
	if err != nil {
		return nil, fmt.Errorf("taking the single-instance lock at %s (%d characters): %w",
			path, len(path), err)
	}
	return &onlyOne{listener: listener, path: path}, nil
}

// errAlreadyOpen is a window already running, which is not a failure.
var errAlreadyOpen = errors.New("an Esper window is already open")

// release drops the lock. Safe to call twice.
func (o *onlyOne) release() {
	if o == nil || o.listener == nil {
		return
	}
	_ = o.listener.Close()
	o.listener = nil
	// Go's unix listener unlinks the socket on close; this is for the case
	// where it could not.
	_ = os.Remove(o.path)
}

// accept answers the second launches, and carries what they were asked to open.
//
// # Why the lock is also the letterbox
//
// A desktop entry that registers `esper://` is telling the OS to run this
// program with a URL. When a window is already open, the right behaviour is
// to raise *that* window on the link — not to start a second one holding the
// same token — and the socket that already proves a window is open is the
// obvious thing to say it through. One connection, one line, no protocol.
//
// `deliver` is called on this goroutine, so it must not block for long.
// Anything that has to touch the window is queued to the window's own
// runtime, which is what the callers here do.
func (o *onlyOne) accept(deliver func(route string)) {
	for {
		conn, err := o.listener.Accept()
		if err != nil {
			return
		}
		// A line, bounded, with a deadline: this is a local socket and the
		// only thing on the other end is another copy of this program, but a
		// reader with no limits is a reader something can sit on.
		_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
		line, _ := bufio.NewReader(io.LimitReader(conn, 4096)).ReadString('\n')
		_ = conn.Close()
		if route := strings.TrimSpace(line); route != "" && deliver != nil {
			deliver(route)
		}
	}
}

// handTo gives an already-open window a route to show.
//
// A best effort by design: if the window has gone between the failed bind
// and this dial, the caller has nothing to raise and should open its own.
func handTo(home, route string) error {
	// The same resolution as the listener, or a second launch would dial a
	// path nothing is listening on and open a window of its own.
	conn, err := net.DialTimeout("unix", lockPath(home), 2*time.Second)
	if err != nil {
		return err
	}
	defer conn.Close()
	_ = conn.SetWriteDeadline(time.Now().Add(2 * time.Second))
	_, err = conn.Write([]byte(route + "\n"))
	return err
}
