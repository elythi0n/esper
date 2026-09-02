package main

import (
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sync"
)

/*
Starting a session from the window, which is the product's whole promise.

# Why this is here and not in the control plane

The window owns the socket the pictures arrive on. A session is started by
running `esper connect <device> --viewer <socket>`, and only this process knows
the path to pass — so the thing that starts it is the thing that would
otherwise have to tell somebody else where to write.

Until now the Session screen showed that command and asked a person to type it.
That is a fine thing for a screen to do when there is nothing better, and it is
not a product: "one click from Devices to a live session" is the sentence this
exists to make true.

# Why running a command is safe here, and where the care went

This route is served to the webview in-process — it is not a network listener,
so nothing off this machine can reach it. What it does do is put a string on a
command line, and that is the part worth being careful about:

  - **The device is matched against an allowlist**, not escaped. Sixteen
    lowercase hexadecimal characters and nothing else, which is what a
    fingerprint is. A sanitiser is a thing that gets one case wrong; a
    fingerprint that does not look like a fingerprint is refused.
  - **`esper` is found beside this binary**, not on `$PATH`. A `PATH` lookup
    would let whatever is earliest on it decide what "connect" means.
  - **One session at a time.** Without that, a page that retried would leave a
    pile of engines behind, each holding a connection to somebody's machine.
*/

// aFingerprint is what a device may look like on a command line.
//
// The same discipline as safeRoute above it: an allowlist of the shape the
// thing actually has, rather than an attempt to remove what is dangerous.
var aFingerprint = regexp.MustCompile(`^[0-9a-f]{16}$`)

// running is the session this window started, if it started one.
type running struct {
	mu     sync.Mutex
	cmd    *exec.Cmd
	device string
}

// start runs `esper connect` against a device, writing pictures to socket.
func (r *running) start(device, socket string) error {
	if !aFingerprint.MatchString(device) {
		return errors.New("that is not a fingerprint, and only a fingerprint is put on a command line")
	}

	r.mu.Lock()
	defer r.mu.Unlock()
	if r.cmd != nil && r.cmd.ProcessState == nil {
		if r.device == device {
			// Already showing this machine. Saying so is better than starting
			// a second engine that competes for the same socket.
			return nil
		}
		return errors.New("this window is already showing a session; end it before starting another")
	}

	front, err := frontDoor()
	if err != nil {
		return err
	}
	// `--control` because a window is a thing you drive a machine *with*. It
	// asks; the host decides, and a host that says no leaves a session that
	// watches — which is what every session here was before, since nothing
	// asked at all.
	cmd := exec.Command(front, "connect", device, "--viewer", socket, "--control") // #nosec G204 -- device is matched against aFingerprint above, and front is resolved beside this binary
	cmd.Env = os.Environ()
	if err := cmd.Start(); err != nil {
		return err
	}
	r.cmd = cmd
	r.device = device

	// Reaped in the background so a finished session does not become a zombie,
	// and so `ProcessState` is set for the check above.
	go func() { _ = cmd.Wait() }()
	return nil
}

// stop ends the session this window started, if there is one.
func (r *running) stop() {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.cmd == nil || r.cmd.Process == nil {
		return
	}
	// Interrupt rather than kill: the engine ends the session cleanly, which
	// is what tells the *other* machine that the session is over and takes its
	// indicator down. Killing it would leave somebody's screen saying it was
	// being watched by nobody.
	_ = r.cmd.Process.Signal(os.Interrupt)
	r.cmd = nil
	r.device = ""
}

// frontDoor is the `esper` binary beside this one.
//
// Beside, rather than on `$PATH`: a lookup would let whatever is earliest on
// the path decide what this runs, and this runs something that connects to
// another machine.
func frontDoor() (string, error) {
	self, err := os.Executable()
	if err != nil {
		return "", err
	}
	front := filepath.Join(filepath.Dir(self), "esper")
	if _, err := os.Stat(front); err != nil {
		return "", errors.New("no `esper` next to this window, so there is nothing to start a session with")
	}
	return front, nil
}

// routeSession adds the start and end of a session to the window's own API.
func (v *viewer) routeSession(mux *http.ServeMux) {
	mux.HandleFunc("POST /shell/session", func(w http.ResponseWriter, req *http.Request) {
		var body struct {
			Device string `json:"device"`
		}
		if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 1<<12)).Decode(&body); err != nil {
			http.Error(w, "a device to connect to is the whole of the request", http.StatusBadRequest)
			return
		}
		if err := v.sessions.start(body.Device, v.path); err != nil {
			// 409 rather than 500: every one of these is something about the
			// request or the window's state, not a fault.
			http.Error(w, err.Error(), http.StatusConflict)
			return
		}
		writeJSON(w, map[string]any{"showing": body.Device})
	})

	mux.HandleFunc("DELETE /shell/session", func(w http.ResponseWriter, _ *http.Request) {
		v.sessions.stop()
		w.WriteHeader(http.StatusNoContent)
	})
}
