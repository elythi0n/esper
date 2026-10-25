// Package engine spawns and supervises esper-engine, the Rust process that
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
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/elythi0n/esper/internal/proto"
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

	// ErrStopped means an engine that was running ended without being asked.
	ErrStopped = errors.New("the engine stopped on its own")
)

// binaryName is the engine executable, without a platform extension.
const binaryName = "esper-engine"

// Supervisor keeps one engine process alive and owns the control channel to it.
//
// The zero value is not usable; construct with New.
type Supervisor struct {
	path      string
	reachedAs string
	log       func(string, ...any)
	grace     time.Duration

	mu      sync.Mutex
	cmd     *exec.Cmd
	stdin   io.WriteCloser
	stdout  io.ReadCloser
	hello   *proto.Hello
	running bool
	// asked means this side ended it, so the reaper below knows an exit is
	// the shutdown finishing rather than a crash.
	asked bool
	// saidMu guards `said` alone. See `keep` for why it is not `mu`.
	saidMu sync.Mutex
	// relayed closes when the engine's stderr has been read to the end, which
	// is when the process has exited and everything it said has been kept.
	// Waited on where the engine's own words are the answer.
	relayed chan struct{}
	// reaped carries the one Wait for the running process. One waiter,
	// because calling Wait twice on an exec.Cmd is an error, and both the
	// reaper and a caller stopping the engine need the result.
	reaped chan error
	// said is the last few lines the engine wrote to stderr, kept so a crash
	// can be quoted rather than merely counted.
	said []string
	// trouble is why there is no engine, when it stopped on its own.
	trouble error
	// greet is what was said at the handshake, kept so a restart can say it
	// again without the caller having to hold on to it.
	greet proto.Hello
	// life is the context the engine was first started with, which is the
	// one that bounds the *process*. See Restart.
	life context.Context
}

// Options configures a Supervisor.
type Options struct {
	// Path to the engine binary. Empty means look beside the current
	// executable, then fall back to PATH — an installed layout puts them in the
	// same directory, and a development build puts them both in ./build.
	Path string

	// Log receives supervision events. Nil discards them.
	Log func(string, ...any)

	// Grace is how long a stopping engine gets to end on its own before it is
	// killed. Zero means DefaultGrace.
	//
	// A field rather than a constant because a test that had to wait the real
	// answer out is a test nobody runs.
	Grace time.Duration

	// ReachedAs is the name the engine should call itself in a message.
	//
	// The engine reads its zeroth argument and answers as `esper` or as
	// `esper-engine` accordingly, so that a usage line names the command
	// somebody typed rather than a binary they may not know exists. Empty
	// means the binary's own name, which is what a caller that is not a front
	// door wants.
	ReachedAs string
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
	grace := opts.Grace
	if grace <= 0 {
		grace = DefaultGrace
	}
	reachedAs := opts.ReachedAs
	if reachedAs == "" {
		reachedAs = filepath.Base(path)
	}
	return &Supervisor{path: path, reachedAs: reachedAs, log: log, grace: grace}, nil
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
		"usually a partial install", ErrNotFound, tried, binaryName, "esper")
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
	// **The engine's stderr is relayed, not inherited.** Inherited, it
	// interleaves with whatever the control plane is printing — a C library's
	// warning lands in the middle of a report, attributed to nobody. Relayed,
	// every line says where it came from, and a command that writes a report
	// on stdout keeps that report clean.
	//
	// **A pipe of our own, not `StderrPipe`.** `Wait` closes a `StderrPipe`
	// the moment the process exits, and the reaper calls `Wait` while the
	// relay is still reading: an engine that printed its reason and exited
	// straight away could have the pipe closed under the relay before a line
	// of it was read, and the reason was lost. Here the relay owns the read
	// end and closes it when it has read everything.
	stderr, stderrW, err := os.Pipe()
	if err != nil {
		return fmt.Errorf("engine stderr: %w", err)
	}
	cmd.Stderr = stderrW

	err = cmd.Start()
	// The child has its own copy now; ours would keep the pipe from ever
	// reaching its end.
	_ = stderrW.Close()
	if err != nil {
		_ = stderr.Close()
		return fmt.Errorf("starting %s: %w", s.path, err)
	}
	s.cmd, s.stdin, s.stdout, s.running = cmd, stdin, stdout, true
	s.asked, s.trouble, s.greet, s.life = false, nil, local, ctx
	s.forgetWhatItSaid()
	s.reaped = make(chan error, 1)
	s.relayed = make(chan struct{})
	s.log("engine started: %s (pid %d)", s.path, cmd.Process.Pid)
	relayed := s.relayed
	go func() {
		s.relay(stderr)
		_ = stderr.Close()
		close(relayed)
	}()
	go s.reap(cmd, s.reaped)

	remote, err := s.handshake(local)
	if err != nil {
		/*
			**What the engine said, not what the pipe did.**

			An engine that refuses to start says why and exits — a
			configuration file with a typo in it, a home directory it cannot
			read — and what reaches here is the *stream* ending: "reading
			engine hello: stream ended mid-frame, wanted 4 bytes, got 0".
			Somebody with a mistyped setting was told about a protocol frame,
			and the sentence naming their file and its line number went into a
			log they were not reading.

			The relay has it. Waiting for stderr to reach its end is waiting
			for the process to have exited and everything it said to have been
			kept — bounded, because a *hung* engine also fails this handshake
			and must not add a wait of any consequence to the failure.
		*/
		if said := s.whatItSaid(relayed); said != "" {
			// The engine's words first. A person reading this has a typo in a
			// file; the framing detail is for whoever is reading the code, and
			// it stays wrapped so `errors.Is` still finds what it always did.
			err = fmt.Errorf("the engine stopped before it could answer: %s (%w)", said, err)
		}
		_ = s.stopLocked()
		return err
	}
	s.hello = &remote
	s.log("engine handshake ok: %s protocol %d", remote.Engine, remote.Proto)
	return nil
}

// relay copies the engine's stderr into the log, a line at a time, until the
// pipe closes — which happens when the process ends.
//
// Bounded: a line longer than the scanner's buffer is truncated rather than
// grown into, because a process printing a megabyte without a newline is
// misbehaving and must not be able to make the supervisor allocate for it.
func (s *Supervisor) relay(stderr io.Reader) {
	scanner := bufio.NewScanner(stderr)
	scanner.Buffer(make([]byte, 0, 4096), maxLogLine)
	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			continue
		}
		s.keep(line)
		s.log("engine: %s", line)
	}
}

// howLongToWaitForLastWords bounds the wait for the engine's own explanation.
//
// Short, because this is on the path where the engine has almost certainly
// already exited: its stderr closes when it does. A hung engine fails the
// handshake too and has said nothing, and waiting on it would turn a fast
// failure into a slow one.
const howLongToWaitForLastWords = 500 * time.Millisecond

// whatItSaid is the engine's last non-empty line, once it has finished saying
// things — or nothing, if it has not finished in the time allowed.
//
// Safe to call with `mu` held: the relay takes `saidMu` and never `mu`.
func (s *Supervisor) whatItSaid(relayed <-chan struct{}) string {
	select {
	case <-relayed:
	case <-time.After(howLongToWaitForLastWords):
		// Still talking, or not talking at all. Either way there is nothing
		// settled to report.
		return ""
	}
	s.saidMu.Lock()
	defer s.saidMu.Unlock()
	for i := len(s.said) - 1; i >= 0; i-- {
		if line := strings.TrimSpace(s.said[i]); line != "" {
			return line
		}
	}
	return ""
}

// forgetWhatItSaid clears the kept lines, for an engine that is starting again.
func (s *Supervisor) forgetWhatItSaid() {
	s.saidMu.Lock()
	defer s.saidMu.Unlock()
	s.said = nil
}

// maxLogLine bounds one relayed line.
const maxLogLine = 16 * 1024

// keepLast is how many of the engine's own words to remember.
//
// The same count the supervising loop keeps, and for the same reason: a Rust
// panic is a message, a location and sometimes a note about a backtrace, and
// the interesting part of any crash is at the end.
const keepLast = 10

// keep remembers one line of the engine's stderr.
//
// **Its own lock, not the supervisor's.** `Start` holds `mu` across the
// handshake, and the handshake's failure path wants to know what the engine
// said on the way out — so a relay that took `mu` to record a line would be
// blocked by the very code waiting for it, and the wait would always time
// out. Nothing here ever takes `mu`, so there is no order to get wrong.
func (s *Supervisor) keep(line string) {
	s.saidMu.Lock()
	defer s.saidMu.Unlock()
	s.said = append(s.said, line)
	if len(s.said) > keepLast {
		s.said = s.said[len(s.said)-keepLast:]
	}
}

// reap waits for the process and records that there is no longer an engine.
//
// # Why this exists
//
// Without it `running` was set true at Start and false only at Stop, so a
// **crashed engine still reported itself as running** — for as long as the
// process that started it stayed up. Every question then failed on a broken
// pipe with a different sentence, and nothing anywhere could say the one
// true thing: the engine is gone, and here is what it said on the way out.
//
// One waiter, because `Wait` may only be called once; whoever stops the
// engine reads the same result from the channel.
func (s *Supervisor) reap(cmd *exec.Cmd, out chan<- error) {
	err := cmd.Wait()
	out <- err

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.cmd != cmd {
		// A later Start has already replaced it; this is an old process
		// finishing and says nothing about the engine that is running now.
		return
	}
	if s.asked {
		// The shutdown finishing, which stopLocked is already handling.
		return
	}
	s.running = false
	s.cmd, s.stdin, s.stdout, s.hello = nil, nil, nil, nil
	s.trouble = fmt.Errorf("%w: %s%s", ErrStopped, exitReason(err), s.lastWordsFor(err))
	s.log("the engine ended on its own: %v", s.trouble)
}

// exitReason turns a Wait error into a sentence.
func exitReason(err error) string {
	if err == nil {
		return "it exited"
	}
	return err.Error()
}

// lastWordsFor appends the engine's last words when they might be the reason.
//
// # Why they are sometimes withheld
//
// The engine says plenty on its way through an ordinary life — a codec
// warning, a device note — and the last of it is whatever happened to be
// most recent, not necessarily anything to do with the end. Quoted next to
// "signal: killed" it reads as a cause, and the first thing this printed in
// anger was a harmless OpenH264 warning presented as the reason a process
// somebody else had killed had died.
//
// So the cut is **who ended it**. A signal from outside — killed, terminated,
// interrupted, hung up — is something that happened *to* the engine, and its
// last words are unrelated by construction. Anything else is the engine
// ending itself: a non-zero exit, or a fault signal it raised by breaking.
// Those are exactly the cases where the last line is the panic.
func (s *Supervisor) lastWordsFor(err error) string {
	if !itsOwnDoing(err) {
		return ""
	}
	s.saidMu.Lock()
	defer s.saidMu.Unlock()
	for i := len(s.said) - 1; i >= 0; i-- {
		if strings.TrimSpace(s.said[i]) != "" {
			return ", last: " + s.said[i]
		}
	}
	return ""
}

// itsOwnDoing says whether the engine ended itself.
func itsOwnDoing(err error) bool {
	if err == nil {
		// It exited cleanly, which is its own doing and says nothing useful.
		return false
	}
	var exit *exec.ExitError
	if !errors.As(err, &exit) {
		return true
	}
	status, ok := exit.Sys().(syscall.WaitStatus)
	if !ok || !status.Signaled() {
		// An exit status of its own choosing.
		return true
	}
	switch status.Signal() {
	case syscall.SIGKILL, syscall.SIGTERM, syscall.SIGINT, syscall.SIGHUP, syscall.SIGQUIT:
		return false
	default:
		// SIGSEGV, SIGABRT and their relatives: the engine broke, and the
		// line before it is the one somebody needs.
		return true
	}
}

// Why says why there is no engine, or nil when one is running.
//
// The reason a caller wants this rather than `Running`: a screen that says
// "the engine is not running" and stops there has told somebody the thing
// they can already see. What they need is what it said as it went.
func (s *Supervisor) Why() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.running {
		return nil
	}
	if s.trouble != nil {
		return s.trouble
	}
	return ErrNotRunning
}

// Restart stops whatever is there and starts a new engine.
//
// It greets the new process with what the old one was greeted with, so a
// caller does not have to keep hold of a Hello to be able to recover.
//
// # Why this takes no context
//
// It took the caller's, and that was wrong in the way that only shows up
// when you run it: `Start` binds the process to its context, the caller with
// a reason to restart an engine is an HTTP handler, and a handler's context
// is cancelled the moment it answers. The new engine started, handshook, and
// was killed a millisecond later — leaving a control plane reporting a crash
// it had just caused. **The engine's life belongs to the process that owns
// it**, which is the context the first Start was given, and that is the one
// used here whoever asks.
func (s *Supervisor) Restart() error {
	s.mu.Lock()
	greeting, life := s.greet, s.life
	s.mu.Unlock()
	if life == nil {
		return fmt.Errorf("%w: it was never started from here", ErrNotRunning)
	}
	if greeting.Engine == "" {
		greeting = s.greeting()
	}
	if err := s.Stop(); err != nil {
		return err
	}
	return s.Start(life, greeting)
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

// Status asks the engine what this machine can do.
//
// One command, one report, on the channel the handshake opened. The engine
// **probes** rather than answering from build flags — an encoder that is
// compiled in and cannot start is the failure this exists to find — so this
// takes as long as those probes take, tens of milliseconds rather than the
// microseconds a pipe would suggest.
//
// The lock is held across the exchange on purpose. This is a request/response
// channel with no message identifiers: two callers interleaving would each
// read the other's answer, and adding identifiers to solve a problem that one
// mutex solves is how a local pipe grows a protocol.
func (s *Supervisor) Status(ctx context.Context) (*proto.Status, error) {
	report, err := s.ask(ctx, proto.CommandStatus)
	if err != nil {
		return nil, err
	}
	if report.Status == nil {
		// An empty report is not an answer, and treating it as one would put
		// zeroes on a diagnostic screen as though they were measurements.
		return nil, errors.New("the engine answered something other than a status")
	}
	return report.Status, nil
}

// ask is exchange with the lock taken, for callers that are whole commands.
func (s *Supervisor) ask(ctx context.Context, command any) (*proto.Report, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if !s.running {
		return nil, ErrNotRunning
	}
	return s.exchange(ctx, command)
}

// exchange sends one command and reads one report. The caller holds the lock.
//
// **The lock is held across the whole exchange**, deliberately. This is a
// request/response channel with no message identifiers: two callers
// interleaving would each read the other's answer, and adding identifiers to
// solve what one mutex solves is how a local pipe grows a protocol.
func (s *Supervisor) exchange(ctx context.Context, command any) (*proto.Report, error) {
	// A deadline the caller chose, kept by closing the pipe rather than by
	// leaving a goroutine reading a socket nobody is waiting for. An engine
	// that has stopped answering is not one to keep talking to.
	if deadline, ok := ctx.Deadline(); ok {
		type deadliner interface{ SetReadDeadline(time.Time) error }
		if d, ok := s.stdout.(deadliner); ok {
			_ = d.SetReadDeadline(deadline)
			defer func() { _ = d.SetReadDeadline(time.Time{}) }()
		}
	}

	if err := proto.WriteFrame(s.stdin, command); err != nil {
		return nil, fmt.Errorf("asking the engine: %w", err)
	}
	var report proto.Report
	if err := proto.ReadFrame(s.stdout, &report); err != nil {
		return nil, fmt.Errorf("reading the engine's answer: %w", err)
	}
	if report.Failed != nil {
		return nil, fmt.Errorf("the engine refused: %s", report.Failed.Detail)
	}
	if report.IsEmpty() {
		return nil, errors.New("the engine sent a report with nothing in it")
	}
	return &report, nil
}

// Sessions asks the engine what has happened on this machine lately, most
// recent last.
//
// The same exchange as Status and the same reason for the lock: one channel,
// no message identifiers.
func (s *Supervisor) Sessions(ctx context.Context, most uint32) ([]proto.Happening, error) {
	report, err := s.ask(ctx, proto.AskForSessions(most))
	if err != nil {
		return nil, err
	}
	if report.Sessions == nil {
		return nil, errors.New("the engine answered something other than a list of sessions")
	}
	return *report.Sessions, nil
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

// stopLocked ends the engine, asking before insisting.
//
// **Closing the channel is the ask.** The engine's command loop treats a
// closed pipe as the ordinary way to end — it is the only clean exit that loop
// has — and until this waited for it, the next line killed the process before
// it could take it. That made a documented clean shutdown unreachable, and
// left every command that was in flight abandoned halfway.
func (s *Supervisor) stopLocked() error {
	if !s.running {
		return nil
	}
	s.running, s.asked = false, true
	if s.stdin != nil {
		_ = s.stdin.Close()
	}
	if s.cmd != nil && s.cmd.Process != nil {
		// **The reaper owns the Wait.** Calling it twice on one command is an
		// error, and the reaper has to be there anyway so that a crash is
		// noticed while nobody is stopping anything.
		cmd, done := s.cmd, s.reaped
		select {
		case <-done: //nolint:errcheck // it was asked to stop, so how it exited is not news
		case <-time.After(s.grace):
			s.log("the engine did not end when its channel closed, so it is being killed")
			_ = cmd.Process.Kill()
			<-done //nolint:errcheck // it was asked to stop, so how it exited is not news
		}
	}
	s.cmd, s.stdin, s.stdout, s.hello, s.reaped = nil, nil, nil, nil, nil
	s.trouble = nil
	// `life` is deliberately kept: it is the owning process's context, not
	// this run's, and a restart after a stop needs it.
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
