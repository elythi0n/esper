package engine

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"syscall"
	"time"

	"github.com/elythi0n/esper/internal/proto"
)

// Watch is how a long-running engine is kept alive, and how closely.
//
// Everything here is a field rather than a constant so the behavior can be
// **tested without waiting for it**: a test that proved a thirty-second
// backoff by sleeping for thirty seconds is a test nobody runs, and one that
// proved a five-crash limit by crashing five times over two minutes is worse.
type Watch struct {
	// Delay is how long to wait before the nth consecutive restart. Nil means
	// Backoff.
	Delay func(attempt int) time.Duration

	// MostFailures is how many consecutive failures to tolerate before giving
	// up. A program that cannot start will not start on the sixth attempt
	// either, and a supervisor that keeps trying turns one broken install into
	// a log file full of the same line.
	MostFailures int

	// Settled is how long a run must last to count as having worked. Without
	// it, a program that starts, serves for an hour and then crashes would
	// count as the sixth failure of a sequence that began yesterday.
	Settled time.Duration

	// Heartbeat is how often to ask the engine whether it is working. Zero
	// means DefaultHeartbeat.
	//
	// Asked rather than assumed, because a process that exists and a process
	// that works are different things and only one of them is what somebody
	// wanted. The question is cheap by construction — see proto.CommandAlive.
	Heartbeat time.Duration

	// Answer is how long the engine gets to answer. Zero means DefaultAnswer.
	Answer time.Duration

	// Misses is how many unanswered questions in a row mean the engine is
	// wedged rather than busy. Zero means DefaultMisses.
	//
	// More than one, because a machine under load can miss a deadline without
	// anything being wrong, and restarting a working host would be a worse
	// outcome than a late answer.
	Misses int

	// Grace is how long a stopping engine gets to end on its own. Zero means
	// DefaultGrace.
	//
	// Bounded, because the teardown is the whole reason to ask rather than
	// kill — sessions closed with a reason, the screen put back, mappings
	// withdrawn — and an unbounded wait would hand a wedged engine the power
	// to stop a machine from shutting down.
	Grace time.Duration

	// Unwatched runs the engine with no channel to it, for a machine where
	// one cannot be opened. Supervision still restarts a crash; nothing
	// notices a wedge.
	Unwatched bool
}

// Defaults for Watch, named so a caller can reason about them.
//
// Ten seconds between questions: often enough that a wedged host is noticed
// inside half a minute, rare enough to be free. Two seconds to answer, which
// is a hundred times what the exchange costs on a loaded machine. Three misses
// before acting, so a burst of load is not a restart.
const (
	DefaultHeartbeat = 10 * time.Second
	DefaultAnswer    = 2 * time.Second
	DefaultMisses    = 3
	DefaultGrace     = 15 * time.Second
)

// ErrGaveUp means the supervised program failed too many times in a row.
var ErrGaveUp = errors.New("the engine keeps failing to run")

// Supervise runs the engine until it is asked to stop, restarting it when it
// dies unexpectedly or stops answering.
//
// **The engine's own stdout is inherited, not relayed.** This is the path where
// somebody is watching a console: `esper host` prints a live report of what it
// is serving, and putting every line of that through a logger with a prefix
// would turn a layout into a transcript. Its stderr passes through this process
// so the last thing it said can be quoted when it fails — see lastWords.
//
// Returns when the engine exits cleanly, when a signal arrives, when the two
// binaries disagree about the protocol, or when it has failed too many times.
func (s *Supervisor) Supervise(ctx context.Context, args []string, w Watch) error {
	w.fill()

	// **A signal we were sent is not a crash.** A terminal sends SIGINT to the
	// whole foreground group, so the engine gets it too and exits cleanly; a
	// supervisor that did not know the signal had happened would call that an
	// unexpected exit and start a new one over the top of the shutdown.
	stopping := make(chan os.Signal, 1)
	signal.Notify(stopping, os.Interrupt, syscall.SIGTERM)
	defer signal.Stop(stopping)

	failures := 0
	for {
		run := s.runOnce(ctx, args, w, stopping)
		if run.stop {
			return run.err
		}
		if run.err == nil {
			// It stopped of its own accord with nothing wrong —
			// `host --once` does exactly that.
			return nil
		}
		if run.lasted >= w.Settled {
			// It ran for long enough to count as having worked, so this is a
			// fresh problem rather than the same one repeating.
			failures = 0
		}
		failures++
		if failures >= w.MostFailures {
			return fmt.Errorf("%w: %d times in a row, last: %v%s",
				ErrGaveUp, failures, run.err, quoted(run.said))
		}
		delay := w.Delay(failures)
		s.log("engine %v — starting it again in %s%s", run.err, delay, quoted(run.said))
		select {
		case <-time.After(delay):
		case <-stopping:
			return nil
		case <-ctx.Done():
			return nil
		}
	}
}

// fill applies the defaults, so every field can be read without asking whether
// the caller set it.
func (w *Watch) fill() {
	if w.Delay == nil {
		w.Delay = Backoff
	}
	if w.MostFailures <= 0 {
		w.MostFailures = 5
	}
	if w.Settled <= 0 {
		w.Settled = 30 * time.Second
	}
	if w.Heartbeat <= 0 {
		w.Heartbeat = DefaultHeartbeat
	}
	if w.Answer <= 0 {
		w.Answer = DefaultAnswer
	}
	if w.Misses <= 0 {
		w.Misses = DefaultMisses
	}
	if w.Grace <= 0 {
		w.Grace = DefaultGrace
	}
}

// ended is how one run of the engine finished.
type ended struct {
	// err is what went wrong, or nil when the engine finished on its own.
	err error
	// stop means do not start another one: we were asked to stop, or the two
	// halves disagree about the protocol and a sixth attempt would disagree
	// the same way.
	stop bool
	// lasted is how long the engine ran, which decides whether a failure is a
	// fresh problem or the same one repeating.
	lasted time.Duration
	// said is the last thing the engine wrote to stderr, or "".
	said string
}

// runOnce starts the engine, watches it, and returns how it finished.
func (s *Supervisor) runOnce(ctx context.Context, args []string, w Watch, stopping <-chan os.Signal) ended {
	// The channel first: the engine is told where to connect on its command
	// line, so it has to exist before the process does.
	var ch *watchChannel
	if !w.Unwatched {
		opened, err := openWatchChannel()
		if err != nil {
			// **Not fatal, and not silent.** A host that would not run
			// because its supervisor could not make a socket would be
			// refusing to do the thing somebody asked for over a way of
			// watching it.
			s.log("supervising without a channel to the engine (%v), so a wedged "+
				"host will not be noticed", err)
		} else {
			ch = opened
			defer ch.close()
			args = append(append([]string{}, args...), "--watched-by", ch.path)
		}
	}

	words := newLastWords(os.Stderr)
	cmd := exec.Command(s.path, args...)
	// **The name it was reached under, not the name of the binary.** The engine
	// reads its zeroth argument to decide what to call itself in a message, and
	// the hand-off path has always set it — so `esper discover --help` says
	// `esper`. This path did not, so `esper host --help` answered with a usage
	// line naming `esper-engine`: a binary the person did not type and may not
	// know exists. `Path` still points at the real file; only the name changes.
	cmd.Args[0] = s.reachedAs
	cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, words

	began := time.Now()
	if err := cmd.Start(); err != nil {
		return ended{err: fmt.Errorf("starting %s: %w", s.path, err), stop: true}
	}

	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()

	// **The engine connecting is a race with the engine dying.** A host that
	// refuses to serve — no session indicator on a machine with a screen —
	// exits during startup, and a supervisor waiting only for the connection
	// would wait out its whole deadline before noticing.
	if ch != nil {
		accepted := make(chan error, 1)
		go func() {
			accepted <- ch.accept(cmd.Process.Pid, s.greeting(), w.Grace, s.log)
		}()
		select {
		case err := <-accepted:
			switch {
			case err == nil:
				s.log("watching engine %s (pid %d) over %s",
					ch.hello.Engine, cmd.Process.Pid, ch.path)
			case errors.Is(err, ErrVersionMismatch):
				// A sixth attempt would disagree the same way.
				s.stopEngine(cmd, done, w.Grace)
				return ended{err: err, stop: true, lasted: time.Since(began)}
			default:
				s.log("no channel to the engine (%v), so a wedged host will not "+
					"be noticed", err)
				ch = nil
			}
		case err := <-done:
			return s.exited(ctx, err, stopping, began, words)
		case <-stopping:
			s.stopEngine(cmd, done, w.Grace)
			return ended{stop: true, lasted: time.Since(began)}
		case <-ctx.Done():
			s.stopEngine(cmd, done, w.Grace)
			return ended{stop: true, lasted: time.Since(began)}
		}
	}

	// The heartbeat runs on its own goroutine so that a question waiting for
	// its answer cannot delay this process noticing a signal.
	wedged := make(chan error, 1)
	until := make(chan struct{})
	defer close(until)
	if ch != nil {
		go keepAsking(ch, w, s.log, wedged, until)
	}

	select {
	case <-stopping:
		// The engine has had the same signal from the terminal, but not from a
		// service manager that signaled only this process — so it is asked,
		// and then waited for. Killing it outright would skip the teardown
		// that closes sessions with a reason and puts the screen back.
		s.stopEngine(cmd, done, w.Grace)
		return ended{stop: true, lasted: time.Since(began)}
	case <-ctx.Done():
		s.stopEngine(cmd, done, w.Grace)
		return ended{stop: true, lasted: time.Since(began)}
	case err := <-wedged:
		s.stopEngine(cmd, done, w.Grace)
		return ended{err: err, lasted: time.Since(began), said: words.last()}
	case err := <-done:
		return s.exited(ctx, err, stopping, began, words)
	}
}

// exited works out what an exit means.
func (s *Supervisor) exited(
	ctx context.Context,
	err error,
	stopping <-chan os.Signal,
	began time.Time,
	words *lastWords,
) ended {
	// **Did it die, or were we all asked to stop?** A terminal sends SIGINT to
	// the whole foreground group, so the engine gets it at the same moment
	// this process does — and if its exit is noticed first, a supervisor that
	// did not look would call a shutdown a crash and start a new engine over
	// the top of it.
	select {
	case <-stopping:
		return ended{stop: true, lasted: time.Since(began)}
	default:
	}
	if ctx.Err() != nil {
		return ended{stop: true, lasted: time.Since(began)}
	}
	if err == nil {
		return ended{lasted: time.Since(began)}
	}
	return ended{
		err:    fmt.Errorf("exited: %w", err),
		lasted: time.Since(began),
		said:   words.last(),
	}
}

// stopEngine asks the engine to end, waits Grace for it, and kills it if it
// will not.
//
// The signal goes to the engine and not to a process group, deliberately: the
// engine owns whatever it started — it hangs up the shells a terminal session
// left running — and a supervisor that killed the group would be taking that
// teardown away from the one process that knows how to do it.
func (s *Supervisor) stopEngine(cmd *exec.Cmd, done <-chan error, grace time.Duration) {
	if cmd.Process == nil {
		return
	}
	_ = cmd.Process.Signal(syscall.SIGTERM)
	select {
	case <-done:
		return
	case <-time.After(grace):
	}
	s.log("the engine did not stop within %s, so it is being killed — a session "+
		"may not have been told why it ended", grace)
	_ = cmd.Process.Kill()
	<-done
}

// keepAsking asks the engine whether it is working until it stops answering or
// the run ends.
//
// What it reports is the *last answer that arrived*, not a bare failure: a
// supervisor saying "it stopped answering" tells somebody nothing they can act
// on, and "it stopped answering — when it last did, it had served 3 sessions
// and was serving nobody" tells them whether the wedge came with a session.
func keepAsking(ch *watchChannel, w Watch, log func(string, ...any), wedged chan<- error, until <-chan struct{}) {
	ticker := time.NewTicker(w.Heartbeat)
	defer ticker.Stop()

	misses := 0
	var last *proto.Alive
	// **Asked at once, and then on the tick.** A supervisor that waited a whole
	// heartbeat for its first answer would have reported a host as watched
	// before knowing whether it answers at all — and the first question is the
	// one most likely to find something wrong.
	for tick := 0; ; tick++ {
		if tick > 0 {
			select {
			case <-until:
				return
			case <-ticker.C:
			}
		}
		alive, err := ch.alive(w.Answer)
		if err == nil {
			switch {
			case last == nil:
				log("the engine answered: listening on %s, serving %s",
					alive.Listening, servingWhom(alive))
			case misses > 0:
				log("the engine is answering again")
			}
			misses, last = 0, alive
			continue
		}
		misses++
		if misses < w.Misses {
			log("the engine did not answer (%d of %d): %v", misses, w.Misses, err)
			continue
		}
		select {
		case wedged <- fmt.Errorf("%w: %d questions in a row went unanswered%s",
			ErrUnresponsive, misses, whatItWasDoing(last)):
		case <-until:
		}
		return
	}
}

// whatItWasDoing renders the last answer that arrived, for the message about
// the ones that did not.
func whatItWasDoing(last *proto.Alive) string {
	if last == nil {
		return " (it never answered one)"
	}
	return fmt.Sprintf(" (it last answered listening on %s, serving %s, %d session(s) served)",
		last.Listening, servingWhom(last), last.Served)
}

// servingWhom names who is connected, or says that nobody is.
//
// An empty string in a sentence reads as a missing value rather than as an
// idle host, and those are different things to somebody reading a log.
func servingWhom(alive *proto.Alive) string {
	if alive.Serving == "" {
		return "nobody"
	}
	return alive.Serving
}

// quoted renders the engine's last words for a message, or nothing when it
// said none.
func quoted(said string) string {
	if said == "" {
		return ""
	}
	return fmt.Sprintf(" — it last said: %s", said)
}

// greeting is what this supervisor says about itself on the watch channel.
func (s *Supervisor) greeting() proto.Hello {
	name, err := os.Hostname()
	if err != nil {
		name = "esper"
	}
	return proto.NewHello(proto.RoleController, nil, name, buildVersion)
}

// buildVersion is what the supervisor reports as its own version.
//
// The engine's is read from its Hello and compared; this side's is only ever
// shown to a person, because the check that matters is the protocol number.
const buildVersion = "0.0.0"
