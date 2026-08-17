package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"time"

	wr "github.com/wailsapp/wails/v2/pkg/runtime"
)

// stateFile is where the window remembers its own shape, under the Esper home.
const stateFile = "window.json"

// windowState is what a window is worth remembering.
//
// Not the route. Where you were last time is a guess about what you want
// next time, and a remote desktop opening on a session that ended yesterday
// is a worse guess than opening on the device list.
type windowState struct {
	Width     int  `json:"width"`
	Height    int  `json:"height"`
	X         int  `json:"x"`
	Y         int  `json:"y"`
	Maximised bool `json:"maximised"`
}

// sane says whether a remembered size is one this window can honour.
//
// A zero is a file that was never written; anything below the minimums is a
// file that has been edited or has survived a change this build does not
// know about. Either way the defaults are better than obeying it.
func (s windowState) sane() bool {
	return s.Width >= 480 && s.Height >= 400 && s.Width <= 30000 && s.Height <= 30000
}

// loadWindowState reads the remembered shape. A missing or unreadable file is
// not an error: it is a window that has not been moved yet.
func loadWindowState(home string) windowState {
	raw, err := os.ReadFile(filepath.Join(home, stateFile))
	if err != nil {
		return windowState{}
	}
	var state windowState
	if json.Unmarshal(raw, &state) != nil {
		return windowState{}
	}
	return state
}

// saveWindowState writes the shape, and says nothing if it cannot.
//
// 0600 like everything else under the home. It holds no secret, but a home
// where one file is world-readable and the rest are not is a home somebody
// has to think about.
func saveWindowState(home string, state windowState) {
	raw, err := json.Marshal(state)
	if err != nil {
		return
	}
	_ = os.WriteFile(filepath.Join(home, stateFile), raw, 0o600)
}

// currentWindowState reads the shape back out of the live window.
func currentWindowState(ctx context.Context) windowState {
	w, h := wr.WindowGetSize(ctx)
	x, y := wr.WindowGetPosition(ctx)
	return windowState{Width: w, Height: h, X: x, Y: y, Maximised: wr.WindowIsMaximised(ctx)}
}

// restoreWindow puts the window back where it was.
//
// # Why the position is checked against the screens and the size is not
//
// A size that no longer fits is clamped by the window manager and the window
// is still on screen. A **position** that no longer fits is not: unplug the
// monitor a window was on and the remembered coordinates put it somewhere
// nothing can be seen, which looks exactly like an application that failed
// to start. So the position is honoured only if it lands on a screen that
// exists now, and otherwise the window opens where the manager puts it.
func restoreWindow(ctx context.Context, state windowState) {
	if !state.sane() {
		return
	}
	if onAScreen(ctx, state) {
		wr.WindowSetPosition(ctx, state.X, state.Y)
	}
	if state.Maximised {
		wr.WindowMaximise(ctx)
	}
}

// onAScreen says whether a remembered position is still a possible one.
//
// **A bound, not a hit test, and deliberately so.** Wails 2's `Screen`
// carries sizes and no origin, so there is no way from here to know that the
// second monitor starts at x=1920 — which means a real hit test is not
// available and pretending otherwise would throw away the position of every
// window on a second display. What can be bounded is the whole desktop: no
// arrangement of these screens extends further than their widths laid end to
// end, or lower than the tallest stacked as many times as there are screens.
// Inside that, the file is trusted; outside it, the monitor it named is gone.
//
// It errs towards trusting the file, because the failure it exists to
// prevent is a window opening where nothing can be seen, and that only
// happens for positions well outside any arrangement.
func onAScreen(ctx context.Context, state windowState) bool {
	screens, err := wr.ScreenGetAll(ctx)
	if err != nil || len(screens) == 0 {
		// Nothing to check against. Trusting the file is the better guess:
		// the common case is the same machine with the same monitors.
		return true
	}

	widest, tallest := 0, 0
	for _, s := range screens {
		w, h := s.Size.Width, s.Size.Height
		if w == 0 || h == 0 {
			w, h = s.Width, s.Height
		}
		widest += w
		if h > tallest {
			tallest = h
		}
	}
	// Enough of the top edge has to be reachable to drag the window back,
	// since the title bar is the only way to move it.
	const grabbable = 64
	return state.X+grabbable > 0 &&
		state.X+grabbable < widest &&
		state.Y >= 0 &&
		state.Y < tallest*len(screens)
}

// watchWindowState writes the shape whenever it changes.
//
// # Why this polls rather than waiting for the close
//
// Saving at shutdown is the obvious place and it is not enough: a window
// that is killed, or that goes down with a session that crashed, is exactly
// the window somebody reopens straight away — and it would come back the
// wrong size every time. Wails 2 raises no resize or move event to hook, so
// the shape is read on a slow tick and written only when it differs. Three
// seconds is far below how often anybody drags a window and far above the
// cost of two runtime calls.
func watchWindowState(ctx context.Context, home string, first windowState) {
	last := first
	tick := time.NewTicker(3 * time.Second)
	defer tick.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-tick.C:
			// A minimised window reports a size and a position nobody chose.
			// Remembering either would reopen the window somewhere it was
			// never put.
			if wr.WindowIsMinimised(ctx) {
				continue
			}
			now := currentWindowState(ctx)
			if !now.sane() || now == last {
				continue
			}
			last = now
			saveWindowState(home, now)
		}
	}
}
