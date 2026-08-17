package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"sync"
	"time"
)

// began is when this process started, for the one measurement that matters.
var began = time.Now()

// firstPaint reports how long it took to become usable.
//
// # Why the page is the one that says
//
// "Cold start to a visible Devices screen" is the number somebody feels, and
// nothing on this side of the webview knows when that happened: the process
// is up long before the window is mapped, and the window is mapped before
// anything is on it. The page knows — it has just painted — so the page says
// so, once, and the shell turns it into a duration from `exec`.
//
// One line on stderr, and nowhere else. It never leaves the machine, which is
// the same rule the rest of the product follows about facts concerning it.
type firstPaint struct {
	once sync.Once
}

func (f *firstPaint) route(mux *http.ServeMux) {
	mux.HandleFunc("POST /shell/painted", func(w http.ResponseWriter, r *http.Request) {
		var said struct {
			Screen string `json:"screen"`
			// What the *desktop* asked for, not what the app's own toggle
			// says. It is reported because "reduced motion is honoured in
			// the shipped window with the OS setting on" is otherwise a
			// claim that needs a person watching a screen — and this is the
			// only place the built window says anything about itself.
			Reduced bool `json:"reduced"`
			// The base duration as the document resolves it, which is the
			// value that proves the override reached the CSS rather than
			// only the media query.
			Base string `json:"base"`
			// Which theme the *first frame* is in. Reported rather than
			// inspected afterwards, because a window that mounts dark and
			// corrects itself to light is a flash somebody sees every
			// launch — and a screenshot taken a second later shows the
			// corrected one.
			Theme string `json:"theme"`
		}
		// A body is a courtesy, not a requirement: the timing is the point.
		_ = json.NewDecoder(http.MaxBytesReader(w, r.Body, 256)).Decode(&said)
		f.once.Do(func() {
			screen := said.Screen
			if screen == "" {
				screen = "a screen"
			}
			motion := "motion full"
			if said.Reduced {
				motion = "motion reduced"
			}
			if said.Base != "" {
				motion += ", --dur-base " + said.Base
			}
			if said.Theme != "" {
				motion += ", theme " + said.Theme
			}
			fmt.Fprintf(os.Stderr, "window ready in %dms (%s, %s)\n",
				time.Since(began).Milliseconds(), screen, motion)
		})
		w.WriteHeader(http.StatusNoContent)
	})
}
