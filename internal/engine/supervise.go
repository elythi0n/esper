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
)

// Watch is how a long-running engine is kept alive.
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
}

// ErrGaveUp means the supervised program failed too many times in a row.
var ErrGaveUp = errors.New("the engine keeps failing to run")

// Supervise runs the engine until it is asked to stop, restarting it when it
// dies unexpectedly.
//
// **The engine's own stdio is inherited, not relayed.** This is the path where
// somebody is watching a console: `esper host` prints a live report of what it
// is serving, and putting every line of that through a logger with a prefix
// would turn a layout into a transcript. The seam's own channel is where
// relaying belongs, because there the control plane is the one talking.
//
// Returns when the engine exits cleanly, when a signal arrives, or when it has
// failed too many times.
func (s *Supervisor) Supervise(ctx context.Context, args []string, w Watch) error {
	if w.Delay == nil {
		w.Delay = Backoff
	}
	if w.MostFailures <= 0 {
		w.MostFailures = 5
	}
	if w.Settled <= 0 {
		w.Settled = 30 * time.Second
	}

	// **A signal we were sent is not a crash.** A terminal sends SIGINT to the
	// whole foreground group, so the engine gets it too and exits cleanly; a
	// supervisor that did not know the signal had happened would call that an
	// unexpected exit and start a new one over the top of the shutdown.
	stopping := make(chan os.Signal, 1)
	signal.Notify(stopping, os.Interrupt, syscall.SIGTERM)
	defer signal.Stop(stopping)

	failures := 0
	for {
		cmd := exec.Command(s.path, args...)
		cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
		if err := cmd.Start(); err != nil {
			return fmt.Errorf("starting %s: %w", s.path, err)
		}
		began := time.Now()

		done := make(chan error, 1)
		go func() { done <- cmd.Wait() }()

		select {
		case <-stopping:
			// The child has had the same signal from the terminal, but not
			// from a service manager that signaled only this process — so it
			// is sent one, and then waited for. Killing it outright would
			// skip the teardown that closes sessions and releases the screen.
			//
			// The signal goes to the engine and not to a process group,
			// deliberately: the engine owns whatever it started — it hangs up
			// the shells a terminal session left running — and a supervisor
			// that killed the group would be taking that teardown away from
			// the one process that knows how to do it.
			_ = cmd.Process.Signal(syscall.SIGTERM)
			<-done
			return nil
		case <-ctx.Done():
			_ = cmd.Process.Signal(syscall.SIGTERM)
			<-done
			return nil
		case err := <-done:
			// **Did it die, or were we all asked to stop?** A terminal sends
			// SIGINT to the whole foreground group, so the child gets it at
			// the same moment this process does — and if its exit is noticed
			// first, a supervisor that did not look would call a shutdown a
			// crash and start a new engine over the top of it.
			select {
			case <-stopping:
				return nil
			default:
			}
			if ctx.Err() != nil {
				return nil
			}
			if err == nil {
				// It stopped of its own accord with nothing wrong —
				// `host --once` does exactly that.
				return nil
			}
			if time.Since(began) >= w.Settled {
				// It ran for long enough to count as having worked, so this
				// is a fresh problem rather than the same one repeating.
				failures = 0
			}
			failures++
			if failures >= w.MostFailures {
				return fmt.Errorf("%w: %d times in a row, last: %v", ErrGaveUp, failures, err)
			}
			delay := w.Delay(failures)
			s.log("engine exited: %v — starting it again in %s", err, delay)
			select {
			case <-time.After(delay):
			case <-stopping:
				return nil
			case <-ctx.Done():
				return nil
			}
		}
	}
}
