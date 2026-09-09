package server

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"path/filepath"
	"strings"
	"time"

	"github.com/elythi0n/esper/internal/devices"
	"github.com/elythi0n/esper/internal/home"
	"github.com/elythi0n/esper/internal/proto"
	"github.com/elythi0n/esper/internal/settings"
)

// DefaultAddr is where the API listens unless somebody says otherwise.
//
// Loopback, and a port high enough not to collide with anything a person is
// likely to be running. It is the address the desktop app and the MCP adapter
// both assume.
const DefaultAddr = "127.0.0.1:8687"

// Engine is what the API asks about this machine.
//
// An interface rather than the supervisor itself, so the tests can answer
// these questions without starting a process — and so this package cannot
// quietly grow a dependency on how the engine is spawned.
type Engine interface {
	Status(ctx context.Context) (*proto.Status, error)
	Sessions(ctx context.Context, most uint32) ([]proto.Happening, error)
	Running() bool
}

// Explainer is an engine that can say *why* it is not running.
//
// Optional, and asked for by assertion rather than added to Engine, because
// not every caller has an answer: a control plane handed an engine it did
// not start knows only that there isn't one. "The engine is not running" on
// a screen tells somebody what they can already see; what they need is the
// thing it said on the way out.
type Explainer interface {
	Why() error
}

// Restarter is an engine this process started and can start again.
//
// Also optional, and for a sharper reason: **only the process holding the
// supervisor can restart it.** A control plane that attached to an engine
// somebody else started must not offer a button that cannot work, so the
// route is present only when the engine can actually do it.
type Restarter interface {
	Restart() error
}

// Server is the local API.
type Server struct {
	engine Engine
	token  Token
	// bound is the address actually listened on, which is what the `Host`
	// check compares against: a caller may only address this server as the
	// thing it bound, never as a name.
	bound string
	// listening is told the address actually bound, once, when it is bound.
	listening func(addr string)
	http      *http.Server
	// events is where session lines are published for the SSE stream.
	events *fanout
	// store is the device store's path, re-read on every request.
	store string
	// writer changes the store by asking the engine to. Nothing here opens it
	// for writing; see `internal/devices`.
	writer devices.Writer
	// settings reads and changes what this machine is configured to do, by
	// asking the engine — which owns the registry and is the only writer.
	settings settings.Writer
	// pairing is the one code this machine may be showing. See pairing.go.
	pairing pairing
	// The one invitation this machine may be offering, held for the same
	// reason a pairing offer is: the engine waits at the broker.
	inviting inviting
}

// Options are what a caller decides.
type Options struct {
	// Addr to listen on. Empty means [DefaultAddr].
	Addr string
	// Token every request must present.
	Token Token
	// Engine to ask.
	Engine Engine
	// EngineBinary is the `esper-engine` to ask when the store must change.
	//
	// A path rather than a running engine, because changing the store is a
	// command run to completion — the same arrangement `esper doctor` uses.
	// Empty means changes are refused, which is what a control plane with no
	// engine beside it should say rather than writing the file itself.
	EngineBinary string
	// Store is the device store to read, or empty for this machine's own.
	//
	// A path rather than a loaded list, because it is **re-read on every
	// request**. The engine re-reads the same file while it runs, which is
	// what makes a revocation reach a session already in progress; an API
	// holding a copy from start-up would be the one place in the product that
	// still believed a device was trusted.
	Store string
	// Exposed must be set to bind anywhere but loopback.
	//
	// **A separate switch from the address**, deliberately: somebody who
	// mistypes an address gets a refusal rather than a listener on their
	// office network, and somebody who means it says so twice.
	Exposed bool
	// Listening is called once, with the address actually bound, the moment
	// the listener is up and before anything is served.
	//
	// **The address asked for is not the address bound.** `--listen
	// 127.0.0.1:0` means "pick a port", and a report printed before the bind
	// says `:0` — which is the one thing the person who asked for a port of
	// nought cannot work out for themselves. Nil discards it.
	Listening func(addr string)
}

// New builds the API without listening.
//
// # Errors
//
// If the address is not loopback and [Options.Exposed] was not set. The
// refusal names the address and what it would have exposed, because a person
// who meant it needs to know what they are agreeing to and a person who did
// not needs to know what they nearly did.
func New(o Options) (*Server, error) {
	addr := o.Addr
	if addr == "" {
		addr = DefaultAddr
	}
	if o.Engine == nil {
		return nil, errors.New("the local API needs an engine to ask")
	}
	if o.Token.String() == "" {
		return nil, errors.New("the local API needs a token; refusing to serve without one")
	}
	loopback, err := isLoopback(addr)
	if err != nil {
		return nil, err
	}
	if !loopback && !o.Exposed {
		return nil, fmt.Errorf(
			"refusing to listen on %s: that is not loopback, and this API answers "+
				"what has happened on this machine, which devices are paired, and what "+
				"this machine can do. Pass --exposed as well if that is what you mean",
			addr,
		)
	}
	store := o.Store
	if store == "" {
		dir, err := home.Dir()
		if err != nil {
			return nil, fmt.Errorf("finding the device store: %w", err)
		}
		store = devices.PathIn(dir)
	}
	s := &Server{
		engine:    o.Engine,
		token:     o.Token,
		bound:     addr,
		listening: o.Listening,
		events:    newFanout(),
		store:     store,
		writer: devices.Writer{
			Engine: o.EngineBinary,
			Home:   filepath.Dir(store),
		},
		settings: settings.Writer{
			Engine: o.EngineBinary,
			Home:   filepath.Dir(store),
		},
	}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /health", s.guard(s.health))
	mux.HandleFunc("GET /status", s.guard(s.status))
	mux.HandleFunc("POST /engine/restart", s.guard(s.restartEngine))
	mux.HandleFunc("GET /identity", s.guard(s.machineIdentity))
	mux.HandleFunc("GET /host", s.guard(s.hostState))
	mux.HandleFunc("POST /host", s.guard(s.setHost))
	mux.HandleFunc("GET /sessions", s.guard(s.sessions))
	mux.HandleFunc("GET /devices", s.guard(s.devices))
	mux.HandleFunc("GET /devices/reachable", s.guard(s.devicesReachable))
	// **State-changing, and guarded by exactly what the reads are.** The token
	// is a header, which a form cannot set and a page cannot read; the `Host`
	// check refuses a request addressed to a name, which is how a browser
	// reaches a loopback listener. Nothing further is needed and nothing less
	// would do.
	mux.HandleFunc("POST /devices/{fingerprint}/grant", s.guard(s.grant))
	mux.HandleFunc("POST /devices/{fingerprint}/revoke", s.guard(s.revoke))
	mux.HandleFunc("DELETE /devices/{fingerprint}", s.guard(s.forget))
	mux.HandleFunc("POST /devices/{fingerprint}/keep", s.guard(s.keepDevice))
	// When a device's unattended access applies. The engine has had the verb
	// since Phase 14 and nothing reached it, so the hours could be seen in the
	// app and changed only in a terminal.
	mux.HandleFunc("POST /devices/{fingerprint}/schedule", s.guard(s.scheduleDevice))
	// What a session with one device leaves the screen as. Same story: the
	// panel showed the answer and only a terminal could change it.
	mux.HandleFunc("POST /devices/{fingerprint}/lock-after", s.guard(s.lockAfterDevice))
	// A magic packet to a device asleep on this network. The engine has had
	// the verb since Phase 12 and only a terminal could send one.
	mux.HandleFunc("POST /devices/{fingerprint}/wake", s.guard(s.wakeDevice))
	// And the three that act on the far machine, each behind a permission that
	// host granted this one. The window could list a device and not lock it.
	mux.HandleFunc("POST /devices/{fingerprint}/lock", s.guard(s.lockHost))
	mux.HandleFunc("POST /devices/{fingerprint}/logoff", s.guard(s.logOffHost))
	mux.HandleFunc("POST /devices/{fingerprint}/reboot", s.guard(s.restartHost))
	// Which groups a device is in. The panel could show them and change none.
	mux.HandleFunc("POST /devices/{fingerprint}/groups", s.guard(s.groupDevice))
	// What this machine is configured to do. Every setting the product has
	// was a file somebody edited by hand until this: `config` listed
	// thirty-two of them and offered no way to change one.
	mux.HandleFunc("GET /config", s.guard(s.config))
	mux.HandleFunc("POST /config", s.guard(s.setSetting))
	mux.HandleFunc("GET /events", s.guard(s.stream))

	// Pairing, in both directions, so it is something a window can do rather
	// than something a person is sent to a terminal for.
	mux.HandleFunc("POST /pair/offer", s.guard(s.pairOffer))
	mux.HandleFunc("GET /pair/offer", s.guard(s.pairState))
	mux.HandleFunc("DELETE /pair/offer", s.guard(s.pairCancel))
	mux.HandleFunc("POST /pair/join", s.guard(s.pairJoin))
	mux.HandleFunc("POST /pair/accept", s.guard(s.pairAccept))
	mux.HandleFunc("POST /invite", s.guard(s.inviteOffer))
	mux.HandleFunc("GET /invite", s.guard(s.inviteState))
	mux.HandleFunc("DELETE /invite", s.guard(s.inviteCancel))
	s.http = &http.Server{
		Addr:    addr,
		Handler: mux,
		// A local caller that opens a connection and says nothing must not
		// hold a slot for ever.
		ReadHeaderTimeout: 5 * time.Second,
	}
	return s, nil
}

// Serve listens until the context is done.
//
// # Errors
//
// If the address cannot be bound, or the server fails while running. A clean
// shutdown is not an error.
func (s *Server) Serve(ctx context.Context) error {
	listener, err := net.Listen("tcp", s.bound)
	if err != nil {
		return fmt.Errorf("listening on %s: %w", s.bound, err)
	}
	// The address actually bound, which differs from what was asked for when
	// the port was zero — and the `Host` check has to compare against what a
	// caller can actually reach.
	s.bound = listener.Addr().String()
	// Said here rather than by the caller before this: here is the first
	// moment the answer exists. Called on this goroutine, so nothing reads
	// `bound` while it is being written.
	if s.listening != nil {
		s.listening(s.bound)
	}
	go func() {
		<-ctx.Done()
		shutdown, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = s.http.Shutdown(shutdown)
		s.events.close()
	}()
	if err := s.http.Serve(listener); err != nil && !errors.Is(err, http.ErrServerClosed) {
		return fmt.Errorf("serving the local API: %w", err)
	}
	return nil
}

// Bound is the address being listened on.
func (s *Server) Bound() string { return s.bound }

// Publish sends one session line to every open event stream.
//
// Called by whatever is watching sessions happen. Nothing is stored: a stream
// that was not open when something happened learns about it from `/sessions`,
// which is the record, and this is only the notification.
func (s *Server) Publish(h proto.Happening) { s.events.publish(h) }

// guard is what every route goes through.
//
// Three refusals, in the order that leaks least: the `Host` first, because it
// is the only one a browser cannot control; then the token, in constant time;
// and only then does a handler see the request.
func (s *Server) guard(next func(http.ResponseWriter, *http.Request)) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !s.addressedCorrectly(r) {
			// **Deliberately not a helpful message.** A page probing this from
			// a browser learns nothing about what is here.
			http.Error(w, "not found", http.StatusNotFound)
			return
		}
		presented := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
		if !s.token.Matches(strings.TrimSpace(presented)) {
			w.Header().Set("WWW-Authenticate", `Bearer realm="esper"`)
			http.Error(w, "a token is required", http.StatusUnauthorized)
			return
		}
		next(w, r)
	}
}

// addressedCorrectly refuses a request that reached here under a name.
//
// # Why a name is refused
//
// A page in a browser can send requests to 127.0.0.1, and **DNS rebinding**
// lets it do so under a hostname it controls: the name resolves to loopback,
// the browser considers the origin its own, and the page reads the reply. The
// defense is to accept only the literal address this server bound — a name
// cannot be rebound to something the check will accept, because the check does
// not accept names at all.
func (s *Server) addressedCorrectly(r *http.Request) bool {
	if r.Host == "" {
		return false
	}
	host := r.Host
	if h, _, err := net.SplitHostPort(host); err == nil {
		host = h
	}
	// An IP literal, and loopback. `localhost` is a *name*, and a name is
	// exactly what rebinding controls.
	ip := net.ParseIP(strings.Trim(host, "[]"))
	if ip == nil {
		return false
	}
	if ip.IsLoopback() {
		return true
	}
	// A server somebody deliberately exposed is addressed by the address they
	// exposed it on, and nothing else.
	bound, _, err := net.SplitHostPort(s.bound)
	return err == nil && bound == ip.String()
}

// health says whether there is an engine, and when there is not, why.
//
// The reason travels with the fact because every screen that shows one wants
// the other, and because the alternative — a second request to find out — is
// a second thing to get wrong at exactly the moment something already has.
func (s *Server) health(w http.ResponseWriter, _ *http.Request) {
	running := s.engine.Running()
	answer := map[string]any{"running": running}

	if !running {
		if explains, ok := s.engine.(Explainer); ok {
			if why := explains.Why(); why != nil {
				answer["why"] = why.Error()
			}
		}
	}
	// Advertised whether or not it is needed, so a screen can decide what to
	// offer before anything has gone wrong.
	_, restartable := s.engine.(Restarter)
	answer["restartable"] = restartable

	writeJSON(w, answer)
}

// restartEngine starts the engine again, for a screen offering the one thing
// that fixes a crash.
//
// A 501 rather than a 404 when this control plane does not own the engine:
// the route exists, and the honest answer is that this process cannot do it.
func (s *Server) restartEngine(w http.ResponseWriter, _ *http.Request) {
	restarts, ok := s.engine.(Restarter)
	if !ok {
		http.Error(w,
			"this control plane did not start the engine, so it cannot start it again",
			http.StatusNotImplemented)
		return
	}
	// No context: the engine's life belongs to whatever owns the supervisor,
	// and a request's context ends when the request does.
	if err := restarts.Restart(); err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) status(w http.ResponseWriter, r *http.Request) {
	status, err := s.engine.Status(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, status)
}

func (s *Server) sessions(w http.ResponseWriter, r *http.Request) {
	most := uint32(20)
	if asked := r.URL.Query().Get("most"); asked != "" {
		var parsed uint32
		if _, err := fmt.Sscanf(asked, "%d", &parsed); err != nil || parsed == 0 {
			http.Error(w, "most has to be a number above zero", http.StatusBadRequest)
			return
		}
		most = min(parsed, 500)
	}
	happenings, err := s.engine.Sessions(r.Context(), most)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, happenings)
}

// listed is a device as this API reports it: the record the store holds, plus
// what a person needs that the file does not spell out.
//
// Derived rather than stored, so nothing here can disagree with the file.
type listed struct {
	devices.Device
	// Fingerprint is what somebody reads out to confirm the device is the one
	// they think it is.
	Fingerprint devices.Fingerprint `json:"fingerprint"`
	// AdmitsASession says whether the pairing would still let this device in.
	// A one-session invitation that has been spent or has lapsed is still in
	// the store and still worth showing — as something to clear out.
	AdmitsASession bool `json:"admits_a_session"`
}

// devices answers which devices this machine trusts.
//
// # Why this one is not asked of the engine
//
// Every other answer here comes from a running engine. This one cannot: a
// machine with nothing connected has no engine, and that is exactly when
// somebody opens the device list — to pick something to connect to. So it is
// read from the store, which is the file the engine reads too.
func (s *Server) devices(w http.ResponseWriter, _ *http.Request) {
	list, err := devices.Load(s.store)
	if err != nil {
		// **Not an empty list.** A device list that looks empty invites
		// somebody to pair everything again rather than to look at the file
		// that is wrong, so the failure says which file and what about it.
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	now := uint64(time.Now().Unix()) //nolint:gosec // Unix seconds, after 1970
	out := make([]listed, 0, len(list))
	for _, device := range list {
		out = append(out, listed{
			Device:         device,
			Fingerprint:    device.Fingerprint(),
			AdmitsASession: device.Pairing.Admits(now),
		})
	}
	writeJSON(w, map[string]any{
		"devices": out,
		// Which file this came from. With `ESPER_HOME` set there is more than
		// one possible answer, and "why is my device not listed" is otherwise
		// unanswerable from the outside.
		"read_from": s.store,
	})
}

func writeJSON(w http.ResponseWriter, body any) {
	w.Header().Set("Content-Type", "application/json")
	// Nothing here is meant for a browser to embed, and saying so costs a
	// header.
	w.Header().Set("X-Content-Type-Options", "nosniff")
	if err := json.NewEncoder(w).Encode(body); err != nil {
		// The status is already written; there is nowhere left to report this.
		return
	}
}

// isLoopback says whether an address is one only this machine can reach.
func isLoopback(addr string) (bool, error) {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return false, fmt.Errorf("%q is not an address and a port: %w", addr, err)
	}
	if host == "" {
		// A bare port means every interface, which is the opposite of
		// loopback however friendly it looks.
		return false, nil
	}
	ip := net.ParseIP(strings.Trim(host, "[]"))
	if ip == nil {
		// A name, which may resolve anywhere and may resolve differently
		// tomorrow. Refused rather than looked up.
		return false, fmt.Errorf(
			"%q is a name rather than an address: the local API binds an address, "+
				"so that what it is reachable from cannot change under it",
			host,
		)
	}
	return ip.IsLoopback(), nil
}
