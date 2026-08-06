package engine

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"
)

// script writes an executable that behaves however the test needs.
//
// A script rather than the real engine: what is being tested here is the
// supervisor's reaction to a program that dies, and the real engine is
// inconveniently good at not dying.
func script(t *testing.T, body string) string {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("this test writes a shell script")
	}
	path := filepath.Join(t.TempDir(), "fake-engine")
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"+body+"\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	return path
}

// logged is what a supervisor said.
//
// **Behind a mutex both ways.** The heartbeat logs from a goroutine of its
// own, so a test that read the slice directly would be racing it — and the
// race detector is right about that even when the writer has already
// finished, because nothing orders the two.
type logged struct {
	mu    sync.Mutex
	lines []string
}

func (l *logged) add(line string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.lines = append(l.lines, line)
}

func (l *logged) all() []string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]string(nil), l.lines...)
}

// joined is everything said, for an assertion about one line among many.
func (l *logged) joined() string { return strings.Join(l.all(), "\n") }

func (l *logged) count() int { return len(l.all()) }

// collect returns a log function writing into l.
func (l *logged) collect() func(string, ...any) {
	return func(format string, args ...any) { l.add(fmt.Sprintf(format, args...)) }
}

func supervisorFor(t *testing.T, path string) (*Supervisor, *logged) {
	t.Helper()
	said := &logged{}
	s, err := New(Options{Path: path, Log: said.collect()})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	return s, said
}

// A program that exits cleanly has finished, not failed.
//
// `esper host --once` serves one session and stops, and a supervisor that
// started it again would turn a deliberate ending into a loop.
func TestACleanExitIsNotRestarted(t *testing.T) {
	s, _ := supervisorFor(t, script(t, "exit 0"))
	started := time.Now()
	if err := s.Supervise(context.Background(), nil, Watch{
		Delay:        func(int) time.Duration { return time.Millisecond },
		MostFailures: 3,
	}); err != nil {
		t.Fatalf("Supervise: %v", err)
	}
	if time.Since(started) > 5*time.Second {
		t.Error("a clean exit took long enough to have been restarted")
	}
}

// A program that keeps failing is given up on, rather than retried for ever.
//
// An engine that cannot start will not start on the sixth attempt either, and
// a supervisor that keeps trying turns one broken install into a log file with
// the same line in it ten thousand times.
func TestAProgramThatKeepsFailingIsGivenUpOn(t *testing.T) {
	counter := filepath.Join(t.TempDir(), "runs")
	s, lines := supervisorFor(t, script(t, "echo x >> "+counter+"; exit 3"))

	err := s.Supervise(context.Background(), nil, Watch{
		Delay:        func(int) time.Duration { return time.Millisecond },
		MostFailures: 4,
	})
	if !errors.Is(err, ErrGaveUp) {
		t.Fatalf("Supervise = %v, want ErrGaveUp", err)
	}

	body, readErr := os.ReadFile(counter)
	if readErr != nil {
		t.Fatalf("reading the run counter: %v", readErr)
	}
	if runs := strings.Count(string(body), "x"); runs != 4 {
		t.Errorf("the program ran %d times, want 4", runs)
	}
	// And it said so each time rather than failing silently.
	if lines.count() < 3 {
		t.Errorf("logged %d lines about the failures: %v", lines.count(), lines.all())
	}
}

// A run that lasted counts as having worked, so the next failure starts a new
// sequence rather than continuing yesterday's.
func TestALongRunResetsTheFailureCount(t *testing.T) {
	counter := filepath.Join(t.TempDir(), "runs")
	// Every run lasts longer than `Settled`, so however many times it fails it
	// is never the fifth failure of a sequence.
	s, _ := supervisorFor(t, script(t, "echo x >> "+counter+"; sleep 0.05; exit 3"))

	ctx, cancel := context.WithTimeout(context.Background(), 900*time.Millisecond)
	defer cancel()
	err := s.Supervise(ctx, nil, Watch{
		Delay:        func(int) time.Duration { return time.Millisecond },
		MostFailures: 3,
		Settled:      10 * time.Millisecond,
	})
	if errors.Is(err, ErrGaveUp) {
		t.Fatal("gave up on a program that was running for long enough each time")
	}

	body, readErr := os.ReadFile(counter)
	if readErr != nil {
		t.Fatalf("reading the run counter: %v", readErr)
	}
	if runs := strings.Count(string(body), "x"); runs < 4 {
		t.Errorf("the program ran %d times, want more than the failure limit", runs)
	}
}

// Being asked to stop ends the supervision rather than starting another one.
func TestCancellingStopsSupervision(t *testing.T) {
	// `exec`, so the script *becomes* the sleep rather than fathering one:
	// a shell that forks leaves the sleep behind when the shell is signaled,
	// and an orphan holding this test binary's stdout makes `go test` wait
	// thirty seconds after the test has passed.
	s, _ := supervisorFor(t, script(t, "exec sleep 30"))
	ctx, cancel := context.WithCancel(context.Background())

	done := make(chan error, 1)
	go func() {
		done <- s.Supervise(ctx, nil, Watch{Delay: func(int) time.Duration { return time.Millisecond }})
	}()

	time.Sleep(50 * time.Millisecond)
	cancel()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Supervise after cancel = %v, want nil", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Supervise did not return after its context was canceled")
	}
}
