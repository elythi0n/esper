package main

import (
	"context"
	"net/http"
	"sync"

	wr "github.com/wailsapp/wails/v2/pkg/runtime"
)

// theWindow holds the context Wails hands over at start-up.
//
// The runtime's window calls all take it, and there is exactly one window per
// process, so one package-level handle is the whole of the state. Guarded
// because start-up and the first HTTP request are different goroutines.
type theWindow struct {
	mu  sync.RWMutex
	ctx context.Context
}

func (w *theWindow) set(ctx context.Context) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.ctx = ctx
}

func (w *theWindow) get() context.Context {
	w.mu.RLock()
	defer w.mu.RUnlock()
	return w.ctx
}

// show raises the window and puts it on a route.
//
// Called when a second launch hands this one a link. Raising is the point:
// somebody who clicked an invitation is looking for a window, and one that
// changed screens behind a minimised title bar would look like nothing
// happened.
func (w *theWindow) show(route string) {
	ctx := w.get()
	if ctx == nil {
		return
	}
	if wr.WindowIsMinimised(ctx) {
		wr.WindowUnminimise(ctx)
	}
	wr.WindowShow(ctx)
	// The router reads `location.hash`, so this is the same seam a document
	// rewrite uses for a window opened on a route — one way in, not two.
	wr.WindowExecJS(ctx, "location.hash = "+quote(route, "#")+";")
}

// windowRoutes are the controls a frameless window has to provide itself.
//
// # Why the window has no system chrome
//
// The title bar is ours, so the buttons are ours. That is the deal a
// frameless window makes: it buys a bar that matches the product, and it
// owes minimise, maximise and close — which the OS was drawing for free.
//
// Over HTTP like everything else the shell answers, so the bundle carries no
// Wails bindings and a browser tab is told there is no window rather than
// finding a missing function.
func windowRoutes(mux *http.ServeMux, win *theWindow) {
	act := func(do func(context.Context)) http.HandlerFunc {
		return func(w http.ResponseWriter, _ *http.Request) {
			ctx := win.get()
			if ctx == nil {
				http.Error(w, "the window is not ready", http.StatusServiceUnavailable)
				return
			}
			do(ctx)
			w.WriteHeader(http.StatusNoContent)
		}
	}

	mux.HandleFunc("POST /shell/window/minimise", act(wr.WindowMinimise))
	mux.HandleFunc("POST /shell/window/maximise", act(wr.WindowToggleMaximise))
	mux.HandleFunc("POST /shell/window/fullscreen", act(func(ctx context.Context) {
		if wr.WindowIsFullscreen(ctx) {
			wr.WindowUnfullscreen(ctx)
			return
		}
		wr.WindowFullscreen(ctx)
	}))
	// **Close, not quit.** A popped-out session closing should not take the
	// main window with it, and Wails ends the process when its one window
	// goes — so per-process, closing the window *is* quitting this one.
	mux.HandleFunc("POST /shell/window/close", act(wr.Quit))
}
