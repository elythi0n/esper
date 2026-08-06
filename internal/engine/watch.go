package engine

import (
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/elythi0n/esper/internal/proto"
)

// A supervised engine answers on a channel of its own.
//
// # Why there is one at all
//
// `esper host` is supervised rather than handed over, because it is the verb
// that runs for weeks on a machine nobody is looking at. Until this channel
// existed, the only thing the supervisor could observe was whether the process
// still existed — and a host wedged holding a session it cannot end looks
// exactly like one serving happily. That is the failure worth catching: a
// crash comes back on its own, and a wedge does not.
//
// # Why a socket rather than a pipe
//
// The engine's stdio is the person's: `esper host` prints a live report there,
// and pushing it through a supervisor would turn a layout into a transcript. An
// inherited file descriptor would need `from_raw_fd` on the Rust side, and that
// workspace forbids unsafe code. So the supervisor listens and the child
// connects back.
//
// # Who may connect
//
// The socket sits in a directory only its owner can enter, and the process
// that connects has to be the child we started. Both halves matter: the
// directory keeps other people out, and the pid check keeps another program
// belonging to the same person from answering a health check and an audit
// extract on the engine's behalf.

// ErrUnresponsive means the engine stopped answering its supervisor.
//
// Distinct from a crash on purpose: the fix is the same — restart it — but what
// happened is not, and a log that called them the same thing would hide the
// only failure a supervisor exists to notice.
var ErrUnresponsive = errors.New("the engine stopped answering")

// watchChannel is the supervisor's end of that channel.
//
// The connection is behind a mutex because two goroutines reach it: the
// heartbeat asking questions, and whatever ends the run closing it. Without
// one, a closed channel is a nil dereference in the middle of a question.
type watchChannel struct {
	dir  string
	path string
	ln   net.Listener

	mu   sync.Mutex
	conn net.Conn
	// hello is what the engine said about itself, kept so a caller can report
	// which build it is watching.
	hello proto.Hello
}

// talking returns the live connection, or nil once it has been closed.
func (w *watchChannel) talking() net.Conn {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.conn
}

// socketName is the file inside the private directory. Short on purpose: a
// Unix socket path has about a hundred bytes to live in, and the temporary
// directory is already using some of them.
const socketName = "watch.sock"

// openWatchChannel creates a private directory with a listening socket in it.
//
// The directory is temporary rather than in the Esper home: a home that does
// not exist yet is a machine nobody has paired, and a supervisor that refused
// to run there would refuse for a reason that has nothing to do with
// supervising.
func openWatchChannel() (*watchChannel, error) {
	dir, err := os.MkdirTemp("", "esper-watch-")
	if err != nil {
		return nil, fmt.Errorf("making a place for the supervision socket: %w", err)
	}
	path := filepath.Join(dir, socketName)
	// Refused early and by name. A path over the platform's limit fails inside
	// bind with "invalid argument", which says nothing about which argument.
	if len(path) > maxSocketPath {
		_ = os.RemoveAll(dir)
		return nil, fmt.Errorf("the supervision socket path is %d bytes, over the %d a "+
			"unix socket allows — set TMPDIR somewhere shorter", len(path), maxSocketPath)
	}
	ln, err := net.Listen("unix", path)
	if err != nil {
		_ = os.RemoveAll(dir)
		return nil, fmt.Errorf("listening on %s: %w", path, err)
	}
	// The directory is already 0700, so this is the second lock on the same
	// door. Kept because the two are independent: a umask, a bind, or a future
	// change of directory could each move one of them.
	if err := os.Chmod(path, 0o600); err != nil {
		_ = ln.Close()
		_ = os.RemoveAll(dir)
		return nil, fmt.Errorf("tightening %s: %w", path, err)
	}
	return &watchChannel{dir: dir, path: path, ln: ln}, nil
}

// maxSocketPath is how long a Unix socket path may be, less room for the
// terminating byte the kernel wants.
const maxSocketPath = 100

// accept waits for the engine to connect, checks it is the process we started,
// and exchanges Hello.
//
// A connection from anything else is closed and the wait continues: an
// impostor must not be able to stop the real engine from being watched by
// getting there first.
func (w *watchChannel) accept(pid int, local proto.Hello, within time.Duration, log func(string, ...any)) error {
	if d, ok := w.ln.(interface{ SetDeadline(time.Time) error }); ok {
		_ = d.SetDeadline(time.Now().Add(within))
		defer func() { _ = d.SetDeadline(time.Time{}) }()
	}
	for {
		conn, err := w.ln.Accept()
		if err != nil {
			return fmt.Errorf("waiting for the engine to connect: %w", err)
		}
		who, err := whoConnected(conn)
		switch {
		case errors.Is(err, errPeerUnknowable):
			// Said once, out loud. A platform that cannot identify the peer
			// still has the private directory, and pretending the check
			// happened would be worse than saying it did not.
			log("this platform cannot say which process connected to the " +
				"supervision socket, so only its directory is guarding it")
		case err != nil:
			_ = conn.Close()
			return fmt.Errorf("checking who connected to %s: %w", w.path, err)
		case who != pid:
			_ = conn.Close()
			log("something other than the engine connected to the supervision "+
				"socket (pid %d, not %d) and was refused", who, pid)
			continue
		}
		hello, err := greet(conn, local, within)
		if err != nil {
			_ = conn.Close()
			return err
		}
		w.mu.Lock()
		w.conn, w.hello = conn, hello
		w.mu.Unlock()
		return nil
	}
}

// greet exchanges Hello and refuses an engine speaking another protocol.
//
// **The check the supervised path did not have.** The seam has done this since
// it existed; the verb that runs for weeks would restart five times and give
// up without ever saying that one of the two binaries was from a different
// install.
func greet(conn net.Conn, local proto.Hello, within time.Duration) (proto.Hello, error) {
	var remote proto.Hello
	_ = conn.SetDeadline(time.Now().Add(within))
	defer func() { _ = conn.SetDeadline(time.Time{}) }()

	if err := proto.ReadFrame(conn, &remote); err != nil {
		return remote, fmt.Errorf("reading the engine's greeting: %w", err)
	}
	if err := proto.WriteFrame(conn, local); err != nil {
		return remote, fmt.Errorf("greeting the engine: %w", err)
	}
	if !remote.VersionMatches() {
		return remote, fmt.Errorf(
			"%w: this supervisor speaks %d, engine %s speaks %d — the two ship "+
				"together, so one of them is from a different install",
			ErrVersionMismatch, proto.ProtocolVersion, remote.Engine, remote.Proto)
	}
	if !remote.NameIsAcceptable() {
		return remote, errors.New("the engine sent an unacceptable name")
	}
	return remote, nil
}

// alive asks the engine whether it is working, and gives it `within` to answer.
//
// One question, one answer, one connection, one asker: the deadline is the
// whole mechanism, because an engine that has stopped answering is exactly
// what this exists to find and a read with no deadline would wait for it for
// ever.
func (w *watchChannel) alive(within time.Duration) (*proto.Alive, error) {
	conn := w.talking()
	if conn == nil {
		return nil, ErrNotRunning
	}
	_ = conn.SetDeadline(time.Now().Add(within))
	defer func() { _ = conn.SetDeadline(time.Time{}) }()

	if err := proto.WriteFrame(conn, proto.CommandAlive); err != nil {
		return nil, fmt.Errorf("asking the engine whether it is working: %w", err)
	}
	var report proto.Report
	if err := proto.ReadFrame(conn, &report); err != nil {
		return nil, fmt.Errorf("waiting for the engine's answer: %w", err)
	}
	if report.Failed != nil {
		return nil, fmt.Errorf("the engine refused: %s", report.Failed.Detail)
	}
	if report.Alive == nil {
		return nil, errors.New("the engine answered something other than whether it is working")
	}
	return report.Alive, nil
}

// close hangs up and removes everything this channel made.
//
// The whole directory goes, not just the socket: one call to remove, and no
// stale-file logic anywhere, because nothing outside this process ever knew
// the name.
func (w *watchChannel) close() {
	w.mu.Lock()
	if w.conn != nil {
		_ = w.conn.Close()
		w.conn = nil
	}
	w.mu.Unlock()
	if w.ln != nil {
		_ = w.ln.Close()
		w.ln = nil
	}
	if w.dir != "" {
		_ = os.RemoveAll(w.dir)
		w.dir = ""
	}
}

// errPeerUnknowable means the platform cannot say which process connected.
var errPeerUnknowable = errors.New("who connected cannot be established here")
