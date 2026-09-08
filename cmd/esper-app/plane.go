package main

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"time"

	"github.com/elythi0n/esper/internal/devices"
	"github.com/elythi0n/esper/internal/engine"
	"github.com/elythi0n/esper/internal/home"
	"github.com/elythi0n/esper/internal/proto"
	"github.com/elythi0n/esper/internal/server"
)

// version is stamped by the linker, the same way the front door's is.
var version = "dev"

// occupant is what, if anything, holds the API address.
//
// A dial says the port is taken; it does not say by *what*, and the
// difference decides everything that happens next. It also decides what the
// window can honestly put on screen: a control plane that refuses this
// process's token is a different fault from no control plane at all, and
// telling somebody to run `esper api` when one is already running is advice
// that cannot work.
type occupant int

const (
	// Nothing is listening. This process starts the control plane.
	nobody occupant = iota
	// An Esper that takes our token — one somebody started themselves, from
	// the same home. It is used as it is.
	ours
	// Something is listening and it is not ours: an Esper holding a
	// different home's token, or another program on the port entirely.
	// Neither can be used, and neither can be displaced.
	stranger
)

// listening says only whether the port is taken. Enough to wait on.
func listening(addr string) bool {
	conn, err := net.DialTimeout("tcp", addr, 400*time.Millisecond)
	if err != nil {
		return false
	}
	_ = conn.Close()
	return true
}

// whoIsThere asks the address `/health` with our own token and reads the answer.
func whoIsThere(addr string, token server.Token) occupant {
	if !listening(addr) {
		return nobody
	}

	request, err := http.NewRequest(http.MethodGet, "http://"+addr+"/health", nil)
	if err != nil {
		return stranger
	}
	// Addressed as the address, exactly as the proxy does: the API refuses a
	// name, and a refusal for that reason would read as a stranger.
	request.Host = addr
	request.Header.Set("Authorization", "Bearer "+token.String())

	answer, err := (&http.Client{Timeout: 2 * time.Second}).Do(request)
	if err != nil {
		return stranger
	}
	// Closing a body this function has finished reading fails only if the
	// connection was already gone, which is what it is about to report.
	defer func() { _ = answer.Body.Close() }()
	if answer.StatusCode == http.StatusOK {
		return ours
	}
	return stranger
}

// runControlPlane starts the local API in this process, and the engine under it.
//
// # Why the window runs it
//
// "Open the app and it works" is the whole requirement, and a window that
// told somebody to go and run `esper api` in a terminal has failed it. The
// control plane is a library in this repository, so the window imports it
// rather than shelling out to a second binary it would then have to find,
// supervise and report on.
//
// **It does not fight for the port.** If an Esper this window can talk to is
// already listening — somebody running `esper api` with their own flags —
// that one is used and this starts nothing. What runs is one control plane,
// whoever started it.
//
// If something is listening that refuses this window's token, nothing here
// can fix it: the port cannot be bound and the API cannot be asked. That is
// reported as what it is, so the screen can say so rather than blaming an
// absence.
//
// The engine is a separate binary and stays one: it is Rust, and the same
// rules the front door follows apply here. Its absence is not fatal, exactly
// as in `esper api` — which devices are paired is read from the store, and
// that is the answer somebody needs when nothing else works.
func runControlPlane(ctx context.Context, esperHome string, token server.Token, addr string) (started bool, err error) {
	switch whoIsThere(addr, token) {
	case ours:
		return false, nil
	case stranger:
		return false, fmt.Errorf(
			"something is already answering on %s and it refuses this machine's token — "+
				"another Esper started from a different %s, most likely. "+
				"Stop it, or start this window with the same %s",
			addr, home.Variable, home.Variable)
	}

	var asking server.Engine
	enginePath := ""
	sup, supErr := engine.New(engine.Options{Log: func(format string, args ...any) {
		fmt.Fprintf(os.Stderr, format+"\n", args...)
	}})
	switch {
	case supErr != nil:
		asking = engine.Missing(supErr)
	default:
		enginePath = sup.Path()
		asking = sup
		if trouble := sup.Start(ctx, proto.NewHello(proto.RoleController, nil, hostname(), version)); trouble != nil {
			asking = engine.Missing(trouble)
		}
	}

	api, err := server.New(server.Options{
		Addr:         addr,
		Token:        token,
		Store:        devices.PathIn(esperHome),
		EngineBinary: enginePath,
		Engine:       asking,
	})
	if err != nil {
		return false, fmt.Errorf("starting the control plane: %w", err)
	}

	go func() {
		if serveErr := api.Serve(ctx); serveErr != nil && ctx.Err() == nil {
			fmt.Fprintf(os.Stderr, "esper-app: the control plane stopped: %v\n", serveErr)
		}
		if sup != nil {
			_ = sup.Stop()
		}
	}()

	// Wait for it to be listening before the window asks it anything, so the
	// first paint is the product rather than the first-run screen.
	waitFor(ctx, addr, 3*time.Second)
	return true, nil
}

func waitFor(ctx context.Context, addr string, patience time.Duration) {
	deadline := time.Now().Add(patience)
	for time.Now().Before(deadline) {
		if ctx.Err() != nil || listening(addr) {
			return
		}
		time.Sleep(40 * time.Millisecond)
	}
}

func hostname() string {
	name, err := os.Hostname()
	if err != nil || name == "" {
		return "esper"
	}
	return name
}
