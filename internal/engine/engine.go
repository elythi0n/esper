// Package engine spawns and supervises ownview-engine, the Rust process that
// owns the realtime path, and speaks the control protocol to it.
//
// The seam is a process boundary rather than cgo. That is deliberate: cgo would
// put a call trampoline and a scheduler hazard in the per-frame path, force a
// C toolchain per cross-compilation target, and let a panic in an unsafe codec
// binding take down the CLI along with it.
//
// The property that makes the boundary affordable is that **media never crosses
// it**. This channel carries commands and state — connect, grant, stop, and
// status coming back — at roughly ten messages a second. If a per-frame message
// ever appears here, the reason for the split is gone and only its cost remains.
package engine

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"time"

	"github.com/elythi0n/ownview/internal/proto"
)

// Errors callers are expected to branch on.
var (
	// ErrNotFound means no engine binary could be located. Almost always a
	// broken install rather than a bug, so the message says where we looked.
	ErrNotFound = errors.New("engine binary not found")

	// ErrVersionMismatch means the engine speaks a different protocol version.
	// The two binaries ship together, so this is a mixed install.
	ErrVersionMismatch = errors.New("engine protocol version mismatch")

	// ErrNotRunning means an operation needed a live engine and there was none.
	ErrNotRunning = errors.New("engine is not running")
)

// binaryName is the engine executable, without a platform extension.
const binaryName = "ownview-engine"

// Supervisor keeps one engine process alive and owns the control channel to it.
//
// The zero value is not usable; construct with New.
type Supervisor struct {
	path string
	log  func(string, ...any)

	mu      sync.Mutex
	cmd     *exec.Cmd
	stdin   io.WriteCloser
	stdout  io.ReadCloser
	hello   *proto.Hello
	running bool
}

// Options configures a Supervisor.
type Options struct {
	// Path to the engine binary. Empty means look beside the current
	// executable, then fall back to PATH — an installed layout puts them in the
	// same directory, and a development build puts them both in ./build.
	Path string

	// Log receives supervision events. Nil discards them.
	Log func(string, ...any)
}

// New locates the engine binary and returns a Supervisor for it. It does not
// start the process; call Start.
func New(opts Options) (*Supervisor, error) {
	path, err := resolve(opts.Path)
	if err != nil {
		return nil, err
	}
	log := opts.Log
	if log == nil {
		log = func(string, ...any) {}
	}
	return &Supervisor{path: path, log: log}, nil
}

// Path returns the resolved engine binary path.
func (s *Supervisor) Path() string { return s.path }

// resolve finds the engine binary, preferring the directory holding the current
// executable so a development build never accidentally runs an installed engine
// of a different version.
func resolve(explicit string) (string, error) {
	if explicit != "" {
		if isExecutable(explicit) {
			return explicit, nil
		}
		return "", fmt.Errorf("%w: %s (from configuration)", ErrNotFound, explicit)
	}

	tried := make([]string, 0, 3)

	if self, err := os.Executable(); err == nil {
		candidate := filepath.Join(filepath.Dir(self), binaryName+exeSuffix())
		tried = append(tried, candidate)
		if isExecutable(candidate) {
			return candidate, nil
		}
	}

	if found, err := exec.LookPath(binaryName); err == nil {
		return found, nil
	}
	tried = append(tried, "$PATH")

	return "", fmt.Errorf("%w: looked in %v — %s and %s ship together, so this is "+
		"usually a partial install", ErrNotFound, tried, binaryName, "ownview")
}

func isExecutable(p string) bool {
	fi, err := os.Stat(p)
	return err == nil && !fi.IsDir() && fi.Mode()&0o111 != 0
}

// Start launches the engine and completes the Hello exchange.
//
// A version mismatch is reported as ErrVersionMismatch and the process is torn
// down: two halves that disagree about the protocol must not proceed, because
// every later failure would be a confusing symptom of this one.
func (s *Supervisor) Start(ctx context.Context, local proto.Hello) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.running {
		return nil
	}

	cmd := exec.CommandContext(ctx, s.path, "serve")
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return fmt.Errorf("engine stdin: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("engine stdout: %w", err)
	}
	cmd.Stderr = os.Stderr

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("starting %s: %w", s.path, err)
	}
	s.cmd, s.stdin, s.stdout, s.running = cmd, stdin, stdout, true
	s.log("engine started: %s (pid %d)", s.path, cmd.Process.Pid)

	remote, err := s.handshake(local)
	if err != nil {
		_ = s.stopLocked()
		return err
	}
	s.hello = &remote
	s.log("engine handshake ok: %s protocol %d", remote.Engine, remote.Proto)
	return nil
}

// handshake sends our Hello and validates the engine's reply.
func (s *Supervisor) handshake(local proto.Hello) (proto.Hello, error) {
	var remote proto.Hello

	if err := proto.WriteFrame(s.stdin, local); err != nil {
		return remote, fmt.Errorf("sending hello: %w", err)
	}
	if err := proto.ReadFrame(s.stdout, &remote); err != nil {
		return remote, fmt.Errorf("reading engine hello: %w", err)
	}

	if !remote.VersionMatches() {
		return remote, fmt.Errorf(
			"%w: control plane speaks %d, engine %s speaks %d — the two ship "+
				"together, so one of them is from a different install",
			ErrVersionMismatch, proto.ProtocolVersion, remote.Engine, remote.Proto)
	}
	if !remote.NameIsAcceptable() {
		return remote, fmt.Errorf("engine sent an unacceptable name")
	}
	return remote, nil
}

// Hello returns the engine's Hello, or nil if the handshake has not completed.
func (s *Supervisor) Hello() *proto.Hello {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.hello == nil {
		return nil
	}
	h := *s.hello
	return &h
}

// Running reports whether an engine process is live.
func (s *Supervisor) Running() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.running
}

// Stop terminates the engine and waits for it to exit.
func (s *Supervisor) Stop() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.stopLocked()
}

func (s *Supervisor) stopLocked() error {
	if !s.running {
		return nil
	}
	s.running = false
	if s.stdin != nil {
		_ = s.stdin.Close()
	}
	if s.cmd != nil && s.cmd.Process != nil {
		_ = s.cmd.Process.Kill()
		_ = s.cmd.Wait()
	}
	s.cmd, s.stdin, s.stdout, s.hello = nil, nil, nil, nil
	s.log("engine stopped")
	return nil
}

// Backoff produces the restart delay for the nth consecutive failure, doubling
// from 1s and capped at 30s.
//
// Capped rather than unbounded because an engine that cannot start is usually a
// broken install, and the UI needs to keep retrying often enough to notice when
// the user fixes it.
func Backoff(attempt int) time.Duration {
	const (
		base = time.Second
		max  = 30 * time.Second
	)
	if attempt < 1 {
		return base
	}
	d := base
	for range attempt - 1 {
		d *= 2
		if d >= max {
			return max
		}
	}
	return d
}
