package server

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"regexp"
	"strings"
	"sync"
	"time"
)

/*
Pairing, over the API, so a window can do it.

# Why this is a process and not a function

Pairing is the engine's, in both directions, and the engine speaks it as a
command: `pair --listen <addr>` offers a code and waits, `pair <addr> --code
<code>` uses one. The control plane owns neither the protocol nor the trust
store, so it does what it does for grant and revoke — it asks the engine —
and the only difference is that an offer *waits*, so the process is held
rather than run to completion.

# One offer at a time

A second offer replaces the first. Two live codes for one machine is a
question nobody can answer at the other end ("which one did you mean?"), and
holding several listeners open is several ports open for a minute each.

# What is deliberately not here

No code is generated, parsed for validity, or stored. The engine's offer is
read back from its own output and passed on. A control plane that minted
pairing codes would be a second implementation of the one thing this product
must not get wrong twice.
*/

// howLongAnOfferRuns bounds the listener even if nothing reads it.
//
// The engine expires its own code in about a minute; this is the outer edge,
// so a caller that walks away does not leave a process and a port behind.
const howLongAnOfferRuns = 3 * time.Minute

// howLongJoiningMayTake bounds the other direction.
const howLongJoiningMayTake = 60 * time.Second

// theOffer reads the engine's offer out of what it prints.
//
// The engine prints the command to run on the other machine, which carries
// the port and the code; the fingerprint words follow, indented, for reading
// aloud. Parsed rather than reinvented — the engine decides what a code is.
var (
	offerLine   = regexp.MustCompile(`pair \S*:(\d+) --code ([A-Z0-9-]+)`)
	wordsLine   = regexp.MustCompile(`^\s{6,}([a-z]+(?: [a-z]+)+)\s*$`)
	expiresLine = regexp.MustCompile(`expires\s+in (\d+) seconds`)
)

// Offer is a pairing code this machine is currently showing.
type Offer struct {
	Code string `json:"code"`
	Port int    `json:"port"`
	// Words is this device's fingerprint, for reading aloud to the person at
	// the other machine. The whole point of a code is that both ends check.
	Words []string `json:"words"`
	// ExpiresAt is when the engine says the code lapses, in Unix seconds.
	ExpiresAt int64 `json:"expires_at"`
	// State is "offering", "paired", "expired" or "failed".
	State string `json:"state"`
	// Detail is why, when the state is "failed".
	Detail string `json:"detail,omitempty"`
}

// pairing holds the one offer this machine may have running.
type pairing struct {
	mu     sync.Mutex
	cancel context.CancelFunc
	offer  *Offer
}

func (p *pairing) snapshot() *Offer {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.offer == nil {
		return nil
	}
	seen := *p.offer
	return &seen
}

func (p *pairing) set(o *Offer) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.offer = o
}

func (p *pairing) finish(state, detail string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.offer != nil {
		p.offer.State = state
		p.offer.Detail = detail
	}
}

// stop ends any running offer. Safe with none.
func (p *pairing) stop() {
	p.mu.Lock()
	cancel := p.cancel
	p.cancel = nil
	p.mu.Unlock()
	if cancel != nil {
		cancel()
	}
}

// offerCode starts a listener and reads back what it is offering.
//
// Returns once the code is known — which is what the caller needs to put on
// screen — and leaves the listener running until somebody pairs, the code
// lapses, or [pairing.stop] is called.
func (s *Server) offerCode(ctx context.Context) (*Offer, error) {
	if s.writer.Engine == "" {
		return nil, errors.New("no engine to ask: pairing is the engine's, and there is none beside this control plane")
	}
	s.pairing.stop()

	// Detached from the request: the offer outlives the call that made it.
	runCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), howLongAnOfferRuns)

	// **Every interface, because the other machine has to reach this one.**
	// Port zero so nothing on this machine is displaced, and the engine
	// prints what it took.
	cmd := exec.CommandContext(runCtx, s.writer.Engine, "pair", "--listen", "0.0.0.0:0") // #nosec G204 -- the path is the operator's own engine
	cmd.Env = append(os.Environ(), "ESPER_HOME="+s.writer.Home)
	said, err := cmd.StdoutPipe()
	if err != nil {
		cancel()
		return nil, fmt.Errorf("reading the pairing offer: %w", err)
	}
	cmd.Stderr = cmd.Stdout
	if err := cmd.Start(); err != nil {
		cancel()
		return nil, fmt.Errorf("starting the pairing listener: %w", err)
	}

	offer, readErr := readOffer(said)
	if readErr != nil {
		cancel()
		_ = cmd.Wait()
		return nil, readErr
	}

	s.mu()
	s.pairing.cancel = cancel
	s.unmu()
	s.pairing.set(offer)

	// Watch it to the end, so the state a caller polls is the truth rather
	// than the last thing that was printed.
	go func() {
		defer cancel()
		// Drain, or the engine blocks on a full pipe and never finishes.
		rest, _ := io.ReadAll(said)
		waitErr := cmd.Wait()
		switch {
		case runCtx.Err() != nil && waitErr != nil:
			s.pairing.finish("expired", "")
		case waitErr != nil:
			s.pairing.finish("failed", lastSentence(string(rest)))
		default:
			s.pairing.finish("paired", "")
		}
	}()

	return offer, nil
}

// mu/unmu guard only the cancel handle; the offer has its own lock.
func (s *Server) mu()   { s.pairing.mu.Lock() }
func (s *Server) unmu() { s.pairing.mu.Unlock() }

// readOffer reads until the engine has said what it is offering.
func readOffer(from io.Reader) (*Offer, error) {
	offer := &Offer{State: "offering"}
	scanner := bufio.NewScanner(from)
	seen := make([]string, 0, 16)
	for scanner.Scan() {
		line := scanner.Text()
		seen = append(seen, line)
		if m := expiresLine.FindStringSubmatch(line); m != nil {
			var secs int
			if _, err := fmt.Sscanf(m[1], "%d", &secs); err == nil {
				offer.ExpiresAt = time.Now().Add(time.Duration(secs) * time.Second).Unix()
			}
		}
		if m := offerLine.FindStringSubmatch(line); m != nil {
			_, _ = fmt.Sscanf(m[1], "%d", &offer.Port)
			offer.Code = m[2]
			continue
		}
		// The words follow the offer, two indented lines of them.
		if offer.Code != "" {
			if m := wordsLine.FindStringSubmatch(line); m != nil {
				offer.Words = append(offer.Words, strings.Fields(m[1])...)
				if len(offer.Words) >= 8 {
					return offer, nil
				}
			}
		}
	}
	if offer.Code != "" {
		return offer, nil
	}
	return nil, fmt.Errorf("the engine offered no code: %s", lastSentence(strings.Join(seen, "; ")))
}

// accept takes somebody up on an invitation.
//
// A different exchange from `join`, not a convenience over it: a code is
// something read aloud between two machines that can already reach each
// other, and an invitation is a token two strangers present to a broker so
// that they can. The engine has both, and this control plane has no business
// turning one into the other.
func (s *Server) accept(ctx context.Context, link string) error {
	if s.writer.Engine == "" {
		return errors.New("no engine to ask: pairing is the engine's, and there is none beside this control plane")
	}
	runCtx, cancel := context.WithTimeout(ctx, howLongJoiningMayTake)
	defer cancel()

	cmd := exec.CommandContext(runCtx, s.writer.Engine, "join", link) // #nosec G204 -- operator's own engine; the link is shape-checked by the caller
	cmd.Env = append(os.Environ(), "ESPER_HOME="+s.writer.Home)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return errors.New(lastSentence(string(out)))
	}
	return nil
}

// join uses a code somebody else is showing.
func (s *Server) join(ctx context.Context, address, code string) error {
	if s.writer.Engine == "" {
		return errors.New("no engine to ask: pairing is the engine's, and there is none beside this control plane")
	}
	runCtx, cancel := context.WithTimeout(ctx, howLongJoiningMayTake)
	defer cancel()

	cmd := exec.CommandContext(runCtx, s.writer.Engine, "pair", address, "--code", code) // #nosec G204 -- operator's own engine; both arguments are shape-checked by the caller
	cmd.Env = append(os.Environ(), "ESPER_HOME="+s.writer.Home)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return errors.New(lastSentence(string(out)))
	}
	return nil
}

// lastSentence is the most useful line of a command's output.
//
// The last non-empty one: the engine says what went wrong at the end, and a
// caller shown the whole of a usage message has been shown nothing.
func lastSentence(out string) string {
	lines := strings.Split(strings.TrimSpace(out), "\n")
	for i := len(lines) - 1; i >= 0; i-- {
		if line := strings.TrimSpace(lines[i]); line != "" {
			return line
		}
	}
	return "the engine said nothing"
}

// ── Routes ────────────────────────────────────────────────────────────────

func (s *Server) pairOffer(w http.ResponseWriter, r *http.Request) {
	offer, err := s.offerCode(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, offer)
}

func (s *Server) pairState(w http.ResponseWriter, _ *http.Request) {
	offer := s.pairing.snapshot()
	if offer == nil {
		writeJSON(w, map[string]any{"state": "none"})
		return
	}
	writeJSON(w, offer)
}

func (s *Server) pairCancel(w http.ResponseWriter, _ *http.Request) {
	s.pairing.stop()
	s.pairing.set(nil)
	w.WriteHeader(http.StatusNoContent)
}

// addressShape is what a peer address may look like: a host and a port, and
// nothing that could be another argument.
var addressShape = regexp.MustCompile(`^[A-Za-z0-9._:\[\]-]{1,255}:\d{1,5}$`)

// codeShape is the engine's own alphabet for a code.
var codeShape = regexp.MustCompile(`^[A-Z0-9-]{4,32}$`)

// linkShape is what an invitation looks like.
//
// One scheme, one path, and base64url after it, up to 512 characters (Go's regexp admits no
// repeat count above 1000, and a 32-byte token prints as about 130) — which is what the engine
// prints and the only thing it will take. An allowlist rather than a parse:
// this goes on a command line, and every character it admits is one somebody
// checked, where a URL parser admits whatever the standard does.
var linkShape = regexp.MustCompile(`^esper://join/[A-Za-z0-9_-]{16,512}$`)

func (s *Server) pairJoin(w http.ResponseWriter, r *http.Request) {
	var asked struct {
		Address string `json:"address"`
		Code    string `json:"code"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096)).Decode(&asked); err != nil {
		http.Error(w, `the body should be {"address": "host:port", "code": "…"}`, http.StatusBadRequest)
		return
	}
	address := strings.TrimSpace(asked.Address)
	code := strings.ToUpper(strings.TrimSpace(asked.Code))
	// **Shape-checked before they reach a command line.** Neither is a
	// sanitised version of what arrived: an argument that is not the right
	// shape is refused, because a sanitiser is a thing that gets one case
	// wrong.
	if !addressShape.MatchString(address) {
		http.Error(w, "that is not an address and a port", http.StatusBadRequest)
		return
	}
	if !codeShape.MatchString(code) {
		http.Error(w, "that is not a pairing code", http.StatusBadRequest)
		return
	}
	if err := s.join(r.Context(), address, code); err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// pairAccept takes an invitation link, which is what a deep link carries.
func (s *Server) pairAccept(w http.ResponseWriter, r *http.Request) {
	var asked struct {
		Link string `json:"link"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096)).Decode(&asked); err != nil {
		http.Error(w, `the body should be {"link": "esper://join/…"}`, http.StatusBadRequest)
		return
	}
	link := strings.TrimSpace(asked.Link)
	if !linkShape.MatchString(link) {
		http.Error(w, "that is not an Esper invitation", http.StatusBadRequest)
		return
	}
	if err := s.accept(r.Context(), link); err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}
