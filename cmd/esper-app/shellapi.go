package main

import (
	"encoding/json"
	"net/http"
	"os"
	"os/exec"
	"regexp"
	"strings"
)

// shellPrefix is what the page asks for when it wants the *window*, not the
// control plane.
const shellPrefix = "/shell/"

// safeRoute is what a route may look like.
//
// **An allowlist, not an escape.** Whatever this accepts is put on a command
// line and becomes a URL fragment in a new window, so it is held to the shape
// the app's own routes have — segments of letters, digits and hyphens — and
// anything else is refused rather than sanitised. A sanitiser is a thing that
// gets one case wrong.
var safeRoute = regexp.MustCompile(`^/[A-Za-z0-9/-]{0,128}$`)

// shellAPI answers the handful of questions the page has about its window.
//
// Over HTTP rather than through Wails' generated bindings, for the same
// reason the control plane is proxied: one seam, no vendor globals in the
// bundle, and a browser tab gets a coherent answer ("no window to open")
// instead of a missing function.
func shellAPI(popoutAllowed bool, win *theWindow, pictures *viewer) http.Handler {
	mux := http.NewServeMux()
	windowRoutes(mux, win)
	// Where a session's pictures arrive, and where to point one.
	pictures.route(mux)
	// Every window, including a popped-out one. There is no state to keep in
	// step — the switch reads the file and writes the file — so two windows
	// showing it cannot disagree, and restricting it would only mean a
	// screen that works in one window and not in another.
	autostartRoutes(mux)

	// How long it took to be usable, said by the only thing that knows.
	painted := &firstPaint{}
	painted.route(mux)

	mux.HandleFunc("GET /shell/capabilities", func(w http.ResponseWriter, _ *http.Request) {
		// Only what no first paint depends on. That the window is frameless
		// and where its requests go are written onto the document instead
		// (document.go), because a bar that arrives a frame late is a bar
		// that might not arrive at all.
		writeJSON(w, map[string]any{
			"popout": popoutAllowed,
			"host":   "desktop",
		})
	})

	mux.HandleFunc("POST /shell/popout", func(w http.ResponseWriter, r *http.Request) {
		if !popoutAllowed {
			http.Error(w, "this window cannot open another", http.StatusNotImplemented)
			return
		}
		var asked struct {
			Route string `json:"route"`
		}
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096)).Decode(&asked); err != nil {
			http.Error(w, "the body should be {\"route\": \"/…\"}", http.StatusBadRequest)
			return
		}
		route := strings.TrimSpace(asked.Route)
		if !safeRoute.MatchString(route) {
			http.Error(w, "that is not a route this window will open", http.StatusBadRequest)
			return
		}
		if err := openWindow(route); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	})

	return mux
}

// openWindow starts a second window on a route.
//
// # Why a second process rather than a second webview
//
// Wails 2 has one window per application, and the webview it uses on Linux
// answers `window.open` with `null` — measured in this window, not assumed —
// so neither the library's pop-out nor anything else built on it can work.
// A second process with its own window does, on every platform Wails
// supports, and for this product it is the better shape anyway: a
// popped-out session gets its own webview and its own media pipeline instead
// of a second video competing inside one.
//
// The child takes no single-instance lock (see main), so it does not have to
// be told about the parent, and closing either leaves the other alone.
func openWindow(route string) error {
	self, err := os.Executable()
	if err != nil {
		return err
	}
	cmd := exec.Command(self, "--window", route)
	cmd.Env = os.Environ()
	// Detached: the child outliving a parent that quits is the behaviour
	// somebody who popped a session out expects.
	cmd.Stdout = nil
	cmd.Stderr = nil
	return cmd.Start()
}

func writeJSON(w http.ResponseWriter, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	_ = json.NewEncoder(w).Encode(body)
}
