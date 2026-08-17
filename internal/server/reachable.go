package server

import (
	"context"
	"errors"
	"net/http"
	"os"
	"os/exec"
	"strings"
	"time"
)

// howLongLookingTakes bounds a look at the network.
//
// Two seconds of listening plus room for the process: discovery asks twice
// inside its window, so a shorter look loses to a busy segment, and a longer
// one is a screen somebody is waiting on.
const howLongLookingTakes = 6 * time.Second

// lookFor is how long the engine listens.
const lookFor = "2"

// Seen is a paired device found on this network, right now.
type Seen struct {
	// Where it is, as the advertisement gave it.
	Address string `json:"address"`
}

// reachable answers which paired devices are on this network.
//
// # What this does and does not know
//
// **One rung of the ladder, and it says so.** Esper reaches a machine over
// the LAN, over an overlay network, at an address it was given, or through a
// broker somebody runs. Of those, only the first announces itself: mDNS is a
// device saying "I am here" and this is that announcement, matched against
// the store by fingerprint.
//
// So a device that appears here is genuinely reachable, and a device that
// does not may be perfectly reachable over an overlay or by address. That
// asymmetry is why the screen says **"on this network"** rather than
// "online": a lamp meaning reachable would be read as a promise, and would
// break the first time somebody clicked a green device that was only ever
// reachable through a broker.
//
// The remaining rungs need a probe the engine does not have — an overlay
// interface enumerated and dialled, an address tried, a broker asked — and
// inventing one here would be the control plane growing its own idea of
// connectivity beside the engine's.
func (s *Server) reachable(ctx context.Context) (map[string]Seen, error) {
	if s.writer.Engine == "" {
		return nil, errors.New("no engine to ask: discovery is the engine's, and there is none beside this control plane")
	}
	runCtx, cancel := context.WithTimeout(ctx, howLongLookingTakes)
	defer cancel()

	cmd := exec.CommandContext(runCtx, s.writer.Engine, "discover", "--porcelain", "--seconds", lookFor) // #nosec G204 -- operator's own engine, fixed arguments
	cmd.Env = append(os.Environ(), "ESPER_HOME="+s.writer.Home)
	out, err := cmd.Output()
	if err != nil {
		return nil, errors.New(lastSentence(string(out)))
	}

	seen := map[string]Seen{}
	for _, line := range strings.Split(string(out), "\n") {
		// state, fingerprint, address, name — and the name may contain
		// anything, so only the first three separators are separators.
		parts := strings.SplitN(strings.TrimRight(line, "\r"), "\t", 4)
		if len(parts) < 3 || parts[0] != "paired" || parts[1] == "" {
			continue
		}
		seen[parts[1]] = Seen{Address: parts[2]}
	}
	return seen, nil
}

func (s *Server) devicesReachable(w http.ResponseWriter, r *http.Request) {
	seen, err := s.reachable(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, map[string]any{
		// Keyed by fingerprint, because that is what a device *is*. A name is
		// what somebody called it and two machines may share one.
		"seen": seen,
		// What was actually looked at, so a screen can say so rather than
		// implying it looked everywhere.
		"looked": "this network",
	})
}
