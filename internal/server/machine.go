package server

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"os/exec"
	"regexp"
	"strings"
	"time"

	"github.com/elythi0n/esper/internal/service"
)

// howLongIdentityTakes bounds reading a local file through a process.
const howLongIdentityTakes = 5 * time.Second

// Identity is what this machine is, to anybody who has paired with it.
type Identity struct {
	// Key is the short fingerprint, the same form the device store uses.
	Key string `json:"key"`
	// Words are what somebody reads aloud to check it. The half of pairing a
	// code cannot do: a code proves the same code was typed, and these prove
	// the machine at the other end is the one that was meant.
	Words []string `json:"words"`
	// File is where the key lives, so a person can look themselves.
	File string `json:"file,omitempty"`
}

// keyLine and wordLine read the engine's own printing of the identity.
//
// # Why this parses rather than reading the file
//
// The identity is a key file, and turning a key into a fingerprint is the
// engine's arithmetic — one hash, one alphabet, one word list. A control
// plane doing it independently would be a second implementation of the thing
// two people read aloud to each other, and the day the two disagreed the
// disagreement would look like an attack.
var (
	keyLine  = regexp.MustCompile(`(?m)^\s*key\s+([0-9a-f]{8,64})\s*$`)
	fileLine = regexp.MustCompile(`(?m)^\s*file\s+(\S.*?)\s*$`)
	wordLine = regexp.MustCompile(`(?m)^\s{4,}([a-z]+(?: [a-z]+){1,7})\s*$`)
)

func (s *Server) identity(ctx context.Context) (*Identity, error) {
	if s.writer.Engine == "" {
		return nil, errors.New("no engine to ask: the identity is the engine's, and there is none beside this control plane")
	}
	runCtx, cancel := context.WithTimeout(ctx, howLongIdentityTakes)
	defer cancel()

	cmd := exec.CommandContext(runCtx, s.writer.Engine, "identity") // #nosec G204 -- operator's own engine, no arguments
	cmd.Env = append(os.Environ(), "ESPER_HOME="+s.writer.Home)
	out, err := cmd.Output()
	if err != nil {
		return nil, errors.New(lastSentence(string(out)))
	}

	said := string(bytes.TrimSpace(out))
	key := keyLine.FindStringSubmatch(said)
	if key == nil {
		return nil, errors.New("the engine printed no fingerprint")
	}
	answer := &Identity{Key: key[1]}
	if file := fileLine.FindStringSubmatch(said); file != nil {
		answer.File = file[1]
	}
	for _, line := range wordLine.FindAllStringSubmatch(said, -1) {
		answer.Words = append(answer.Words, strings.Fields(line[1])...)
	}
	return answer, nil
}

func (s *Server) machineIdentity(w http.ResponseWriter, r *http.Request) {
	who, err := s.identity(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, who)
}

// hostState is whether this machine serves its screen.
//
// Two flags rather than one, because they answer different questions and can
// disagree: a host that is running but not enabled will be gone after a
// reboot, and a screen showing one switch would be telling somebody something
// they find out the hard way.
func (s *Server) hostState(w http.ResponseWriter, _ *http.Request) {
	now := service.Now()
	writeJSON(w, map[string]any{
		"managed": now.Managed,
		"enabled": now.Enabled,
		"active":  now.Active,
		"why":     now.Why,
		"manager": service.Manager,
	})
}

// setHost turns the host on or off through the machine's own service manager.
//
// # Why the control plane does this at all
//
// Turning the host on is most of what the main screen is, and it has to work
// by being pressed — a screen cannot ask somebody to copy four commands out
// of a comment, one of which (`enable-linger`) is the one everybody misses.
// The wrapper reports every command it runs, and that report is handed back
// here rather than swallowed: a machine where this half-worked is still a
// machine somebody can finish by hand.
func (s *Server) setHost(w http.ResponseWriter, r *http.Request) {
	var asked struct {
		On bool `json:"on"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1024)).Decode(&asked); err != nil {
		http.Error(w, `the body should be {"on": true}`, http.StatusBadRequest)
		return
	}

	var report bytes.Buffer
	change := service.Disable
	if asked.On {
		change = service.Enable
	}
	err := change(service.Report{To: &report})

	now := service.Now()
	answer := map[string]any{
		"managed": now.Managed,
		"enabled": now.Enabled,
		"active":  now.Active,
		"manager": service.Manager,
		// Every command it ran, in order, so somebody can see what was done
		// to their machine and repeat it without this.
		"said": strings.TrimRight(report.String(), "\n"),
	}
	if err != nil {
		answer["why"] = err.Error()
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusBadGateway)
		_ = json.NewEncoder(w).Encode(answer)
		return
	}
	writeJSON(w, answer)
}
