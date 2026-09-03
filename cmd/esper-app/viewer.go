package main

import (
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"sync"
	"time"
)

// say prints where everything else in this window prints: stderr, prefixed,
// so a line is attributable when it lands among the engine's own.
func say(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "esper-app: "+format+"\n", args...)
}

// viewerSocket is where a session's pictures arrive.
const viewerSocketName = "video.sock"

// howLongToWaitForASession is how long a request for the picture waits for a
// session to start sending one.
//
// Long enough to cover somebody typing the command the Session screen shows
// them, short enough that a page which asked for a picture nobody is sending
// gets an answer rather than a spinner. It is a *first* frame budget, not a
// session's: once the stream is flowing this is not consulted again.
const howLongToWaitForASession = 90 * time.Second

// mostInputBytes is the largest batch of input a window may send at once.
//
// A frame of input is a few events of a few bytes each; a kilobyte is already
// a person typing impossibly fast. The bound is here because this path ends
// at somebody else's keyboard, and the first rule of that path is that it
// carries what a person did and nothing else.
const mostInputBytes = 64 * 1024

// viewer accepts the engine's connection and hands the bytes to the page.
//
// # Why the window listens and the engine dials
//
// The socket is the window's: it knows its own home, creates the file with
// the permissions everything else there has, and removes it when it closes.
// An engine that outlives a window cannot do any of that for it. The window
// also has to be able to say *where* the socket is, because the way a session
// is started today is a person running `esper connect --viewer <path>`.
//
// # Why the page gets a chunked body and not a WebSocket
//
// Because a body is enough. The page reads it with `fetch` and a stream
// reader, which is the same code in both webviews and in a browser tab —
// where a WebSocket would be a second protocol, a second failure mode, and a
// dependency in the engine. Nothing is sent *up* this path: input will have
// its own, and a channel that only ever flows one way should look like one.
type viewer struct {
	listener net.Listener
	path     string
	// The session this window started, if it started one. See session.go —
	// the window owns the socket, so the window is what can name it on a
	// command line.
	sessions running
	// What has been dropped on this window and sent to the far machine.
	drops transfers
	// The session's connection, kept so what the page does can go back down
	// it. Guarded because the reader and the writer are different requests.
	mu      sync.Mutex
	session net.Conn
	// Held across a write, so two batches of input cannot be cut into each
	// other. Separate from mu, which is only ever held for a moment.
	writing sync.Mutex
	// How long a request for the picture waits for a session. A field so a
	// test does not have to wait ninety seconds to prove the timeout says
	// something useful.
	patience time.Duration
}

// viewerName is what a window's video socket is called.
//
// A popped-out window gets its own, because it shows its own session: one
// socket shared between two windows would be one of them taking the other's
// pictures, which is the sort of thing that looks like a bug in the engine
// for a week. The pid is in the name because a popped-out window is exactly
// as long-lived as its process.
func viewerName(secondary bool) string {
	if secondary {
		return fmt.Sprintf("video-%d.sock", os.Getpid())
	}
	return viewerSocketName
}

// openViewer creates the socket and starts listening on it.
func openViewer(home string, secondary bool) (*viewer, error) {
	path := socketPath(home, viewerName(secondary))
	// A socket left by a window that died is a file nothing is listening on,
	// and binding over it is the only way to take the name back. Removing it
	// is safe because a *live* window holds the single-instance lock, so
	// there cannot be a second one using this.
	_ = os.Remove(path)
	listener, err := net.Listen("unix", path)
	if err != nil {
		return nil, err
	}
	if err := os.Chmod(path, 0o600); err != nil {
		// Not fatal: the socket works, and its directory is already the
		// user's own. Worth saying, because a socket anybody can dial is a
		// socket anybody can feed pictures into.
		say("could not restrict %s: %v", path, err)
	}
	return &viewer{listener: listener, path: path, patience: howLongToWaitForASession}, nil
}

// close stops listening and removes the socket file.
func (v *viewer) close() {
	if v == nil {
		return
	}
	_ = v.listener.Close()
	_ = os.Remove(v.path)
}

// route registers the page's two questions about the picture.
func (v *viewer) route(mux *http.ServeMux) {
	if v == nil {
		// No socket, so the page is told plainly rather than left waiting on
		// a route that does not answer.
		mux.HandleFunc("GET /shell/video", func(w http.ResponseWriter, _ *http.Request) {
			http.Error(w, "this window has no socket for a session's pictures", http.StatusNotImplemented)
		})
		return
	}

	// Where to point a session. Still answered, because a person may want to
	// run the command themselves — but no longer the only way, now that the
	// window can start one: see routeSession below.
	mux.HandleFunc("GET /shell/video/socket", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, map[string]any{"path": v.path})
	})

	v.routeSession(mux)
	v.routeTransfers(mux)

	/*
		What the person did, on its way back down the same socket.

		A POST per batch rather than a stream, because a request body that
		stays open is a duplex fetch — which needs HTTP/2 in Chromium and is
		not there at all in the webview this product ships. A batch per
		animation frame costs one request every sixteen milliseconds and adds
		at most that much to the hop, which is a price worth paying for a path
		that works in both engines and in a browser tab.

		Bounded, because it reaches somebody else's keyboard: a body larger
		than any plausible frame of input is refused rather than forwarded.
	*/
	mux.HandleFunc("POST /shell/video/input", func(w http.ResponseWriter, r *http.Request) {
		v.mu.Lock()
		session := v.session
		v.mu.Unlock()
		if session == nil {
			http.Error(w, "no session is listening for input", http.StatusConflict)
			return
		}
		body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, mostInputBytes))
		if err != nil {
			http.Error(w, "that is more input than a window sends in one frame", http.StatusRequestEntityTooLarge)
			return
		}
		// Written under the lock, so two batches cannot interleave on the
		// socket. A stream socket may split a write, and half of one batch
		// followed by half of another is a length field reading somebody
		// else's bytes.
		v.writing.Lock()
		_, err = session.Write(body)
		v.writing.Unlock()
		if err != nil {
			http.Error(w, "the session stopped taking input", http.StatusGone)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	})

	mux.HandleFunc("GET /shell/video", func(w http.ResponseWriter, r *http.Request) {
		conn, err := v.accept(r)
		if err == nil {
			v.mu.Lock()
			v.session = conn
			v.mu.Unlock()
			defer func() {
				v.mu.Lock()
				if v.session == conn {
					v.session = nil
				}
				v.mu.Unlock()
			}()
		}
		if err != nil {
			// A page that asked for a picture and waited is told why rather
			// than being left with a stream that never starts.
			http.Error(w, err.Error(), http.StatusGatewayTimeout)
			return
		}
		defer func() { _ = conn.Close() }()

		w.Header().Set("Content-Type", "application/octet-stream")
		w.Header().Set("Cache-Control", "no-store")
		// The body is framed by the stream's own records; a sniffing
		// middlebox deciding it is something else would be a mess nobody
		// would look for here.
		w.Header().Set("X-Content-Type-Options", "nosniff")
		w.WriteHeader(http.StatusOK)

		flusher, _ := w.(http.Flusher)
		// **The headers go out before the first picture.** Without this they
		// sit in the server's buffer until something is written, so a page
		// that has connected to a session with a still screen cannot tell the
		// difference between "connected, nothing has changed" and "still
		// waiting" — and neither could the test that found it.
		if flusher != nil {
			flusher.Flush()
		}
		// **Flushed per read, not per buffer.** A picture that waits in a
		// buffer for the next one is a frame of latency added by tidiness,
		// which is exactly what the loopback hop exists to avoid.
		buf := make([]byte, 64*1024)
		for {
			select {
			case <-r.Context().Done():
				return
			default:
			}
			_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
			n, err := conn.Read(buf)
			if n > 0 {
				if _, werr := w.Write(buf[:n]); werr != nil {
					return
				}
				if flusher != nil {
					flusher.Flush()
				}
			}
			if err != nil {
				var timeout net.Error
				if errors.As(err, &timeout) && timeout.Timeout() {
					// A session with nothing to send is a still screen, not a
					// session that ended. Keep waiting; the page's own
					// request context ends this when the page goes away.
					continue
				}
				if !errors.Is(err, io.EOF) {
					say("the session's picture stream ended: %v", err)
				}
				return
			}
		}
	})
}

// accept waits for a session to dial, or for the page to give up.
func (v *viewer) accept(r *http.Request) (net.Conn, error) {
	type arrival struct {
		conn net.Conn
		err  error
	}
	got := make(chan arrival, 1)
	go func() {
		conn, err := v.listener.Accept()
		got <- arrival{conn, err}
	}()

	select {
	case a := <-got:
		return a.conn, a.err
	case <-r.Context().Done():
		return nil, errors.New("the page stopped waiting for a session")
	case <-time.After(v.patience):
		return nil, errors.New("no session is sending pictures to this window")
	}
}
