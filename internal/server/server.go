package server

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"strings"
	"time"

	"github.com/elythi0n/esper/internal/proto"
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

// Server is the local API.
type Server struct {
	engine Engine
	token  Token
	// bound is the address actually listened on, which is what the `Host`
	// check compares against: a caller may only address this server as the
	// thing it bound, never as a name.
	bound string
	http  *http.Server
	// events is where session lines are published for the SSE stream.
	events *fanout
}

// Options are what a caller decides.
type Options struct {
	// Addr to listen on. Empty means [DefaultAddr].
	Addr string
	// Token every request must present.
	Token Token
	// Engine to ask.
	Engine Engine
	// Exposed must be set to bind anywhere but loopback.
	//
	// **A separate switch from the address**, deliberately: somebody who
	// mistypes an address gets a refusal rather than a listener on their
	// office network, and somebody who means it says so twice.
	Exposed bool
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
	s := &Server{engine: o.Engine, token: o.Token, bound: addr, events: newFanout()}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /health", s.guard(s.health))
	mux.HandleFunc("GET /status", s.guard(s.status))
	mux.HandleFunc("GET /sessions", s.guard(s.sessions))
	mux.HandleFunc("GET /events", s.guard(s.stream))
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

func (s *Server) health(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, map[string]any{"running": s.engine.Running()})
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
