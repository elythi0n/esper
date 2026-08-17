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
Inviting somebody, over the API, so a window can do it.

# Why this is held, like a pairing offer

`invite` is not a command that returns a link and finishes — it prints the
link and then *waits at the broker* for whoever it was sent to. So this is
the same shape as `offerCode`: start the process, read back the one thing the
screen needs, and leave it running until somebody arrives, it lapses, or it
is cancelled.

# One invitation at a time

The same reason as a pairing code: two live invitations for one machine is a
question nobody can answer, and each is a process holding a rendezvous open.

# What is deliberately not here

The link is not built, parsed for meaning, or stored anywhere. It carries a
broker's pinned key, a random token and a pairing code, and every one of
those is the engine's arithmetic. A control plane that assembled one would be
a second implementation of the thing that decides who may pair with this
machine.
*/

// howLongAnInvitationRuns bounds the process even if nobody arrives.
//
// The engine expires the invitation itself — half an hour by default — and
// this is the outer edge, so an abandoned screen does not leave a process
// waiting at a broker for ever.
const howLongAnInvitationRuns = 35 * time.Minute

// The engine's own printing of an invitation.
var (
	linkPrinted    = regexp.MustCompile(`(esper://join/[A-Za-z0-9_-]{16,512})`)
	expiresMinutes = regexp.MustCompile(`expires\s+in (\d+) minute`)
	brokerPrinted  = regexp.MustCompile(`(?m)^\s*broker\s+(\S+)\s*$`)
)

// Invitation is a way in that this machine is currently offering.
type Invitation struct {
	// Link is the whole of what somebody is sent. It is the secret: anybody
	// holding it can pair with this machine until it expires, which is what
	// it is for and why it is short-lived.
	Link string `json:"link"`
	// Who it was made for, as a label. Nothing checks it; it is so the person
	// offering can tell two invitations apart.
	For string `json:"for,omitempty"`
	// Broker is the rendezvous it meets at, so a screen can say where.
	Broker string `json:"broker,omitempty"`
	// ExpiresAt is when the engine says it lapses, in Unix seconds.
	ExpiresAt int64 `json:"expires_at"`
	// State is "offering", "paired", "expired" or "failed".
	State string `json:"state"`
	// Detail is why, when the state is "failed" — which is where "there is no
	// broker configured" arrives, and it is the useful answer rather than a
	// generic refusal.
	Detail string `json:"detail,omitempty"`
}

// inviting holds the one invitation this machine may have running.
type inviting struct {
	mu     sync.Mutex
	cancel context.CancelFunc
	live   *Invitation
}

func (i *inviting) snapshot() *Invitation {
	i.mu.Lock()
	defer i.mu.Unlock()
	if i.live == nil {
		return nil
	}
	seen := *i.live
	return &seen
}

func (i *inviting) set(v *Invitation) {
	i.mu.Lock()
	defer i.mu.Unlock()
	i.live = v
}

func (i *inviting) finish(state, detail string) {
	i.mu.Lock()
	defer i.mu.Unlock()
	if i.live != nil {
		i.live.State = state
		i.live.Detail = detail
	}
}

func (i *inviting) stop() {
	i.mu.Lock()
	cancel := i.cancel
	i.cancel = nil
	i.mu.Unlock()
	if cancel != nil {
		cancel()
	}
}

// whoShape is what a label may look like.
//
// It goes on a command line, so it is held to a shape rather than escaped:
// letters, digits, spaces and a few marks a name really has. Anything else
// is refused, because a sanitiser is a thing that gets one case wrong.
var whoShape = regexp.MustCompile(`^[\p{L}\p{N} ._'-]{1,64}$`)

// offerInvitation starts an invitation and reads back the link.
func (s *Server) offerInvitation(ctx context.Context, who string) (*Invitation, error) {
	if s.writer.Engine == "" {
		return nil, errors.New("no engine to ask: an invitation is the engine's, and there is none beside this control plane")
	}
	s.inviting.stop()

	args := []string{"invite"}
	if who != "" {
		args = append(args, "--for", who)
	}

	// Detached from the request: the invitation outlives the call that made it.
	runCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), howLongAnInvitationRuns)

	// **No broker on the command line.** The engine reads `broker.address`
	// and `broker.key` from the operator's own settings, as every other
	// command that needs a rendezvous does — so a pinned key never passes
	// through this process, and there is still no default broker anywhere.
	cmd := exec.CommandContext(runCtx, s.writer.Engine, args...) // #nosec G204 -- operator's own engine; the label is shape-checked by the caller
	cmd.Env = append(os.Environ(), "ESPER_HOME="+s.writer.Home)
	said, err := cmd.StdoutPipe()
	if err != nil {
		cancel()
		return nil, fmt.Errorf("reading the invitation: %w", err)
	}
	cmd.Stderr = cmd.Stdout
	if err := cmd.Start(); err != nil {
		cancel()
		return nil, fmt.Errorf("starting the invitation: %w", err)
	}

	offered, readErr := readInvitation(said, who)
	if readErr != nil {
		cancel()
		_ = cmd.Wait()
		return nil, readErr
	}

	s.inviting.mu.Lock()
	s.inviting.cancel = cancel
	s.inviting.mu.Unlock()
	s.inviting.set(offered)

	go func() {
		defer cancel()
		rest, _ := io.ReadAll(said)
		waitErr := cmd.Wait()
		switch {
		case runCtx.Err() != nil && waitErr != nil:
			s.inviting.finish("expired", "")
		case waitErr != nil:
			s.inviting.finish("failed", lastSentence(string(rest)))
		default:
			s.inviting.finish("paired", "")
		}
	}()

	return offered, nil
}

// readInvitation reads until the engine has printed the link.
func readInvitation(from io.Reader, who string) (*Invitation, error) {
	offered := &Invitation{State: "offering", For: who}
	scanner := bufio.NewScanner(from)
	seen := make([]string, 0, 16)
	for scanner.Scan() {
		line := scanner.Text()
		seen = append(seen, line)
		if m := brokerPrinted.FindStringSubmatch(line); m != nil {
			offered.Broker = m[1]
		}
		if m := expiresMinutes.FindStringSubmatch(line); m != nil {
			var minutes int
			if _, err := fmt.Sscanf(m[1], "%d", &minutes); err == nil {
				offered.ExpiresAt = time.Now().Add(time.Duration(minutes) * time.Minute).Unix()
			}
		}
		if m := linkPrinted.FindStringSubmatch(line); m != nil {
			offered.Link = m[1]
			return offered, nil
		}
	}
	// **The failure is the useful part here.** With no broker configured the
	// engine explains exactly that, and passing its sentence on is what makes
	// the screen able to say why rather than merely that.
	return nil, errors.New(lastSentence(strings.Join(seen, "\n")))
}

// ── Routes ────────────────────────────────────────────────────────────────

func (s *Server) inviteOffer(w http.ResponseWriter, r *http.Request) {
	var asked struct {
		For string `json:"for"`
	}
	// A body is optional: an invitation with no label is a valid invitation.
	_ = json.NewDecoder(http.MaxBytesReader(w, r.Body, 1024)).Decode(&asked)
	who := strings.TrimSpace(asked.For)
	if who != "" && !whoShape.MatchString(who) {
		http.Error(w, "that is not a name this will put on a command line", http.StatusBadRequest)
		return
	}

	offered, err := s.offerInvitation(r.Context(), who)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, offered)
}

func (s *Server) inviteState(w http.ResponseWriter, _ *http.Request) {
	offered := s.inviting.snapshot()
	if offered == nil {
		writeJSON(w, map[string]any{"state": "none"})
		return
	}
	writeJSON(w, offered)
}

func (s *Server) inviteCancel(w http.ResponseWriter, _ *http.Request) {
	s.inviting.stop()
	s.inviting.set(nil)
	w.WriteHeader(http.StatusNoContent)
}
