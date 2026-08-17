package engine

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/elythi0n/esper/internal/proto"
)

// engineBinary returns the built engine, skipping the test when it is absent.
// These tests exercise a real process across a real pipe — the seam is the
// thing being tested, so a mock would test nothing.
func engineBinary(t *testing.T) string {
	t.Helper()
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	p := filepath.Join(root, "build", "esper-engine")
	if _, err := os.Stat(p); err != nil {
		t.Skip("no built engine at build/esper-engine — run 'make build'")
	}
	return p
}

func controlPlaneHello() proto.Hello {
	return proto.NewHello(proto.RoleController, nil, "test-control-plane", "0.0.0")
}

// The headline test: two binaries, two languages, one protocol, over a real pipe.
func TestHandshakeAcrossTheProcessSeam(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	t.Cleanup(func() { _ = s.Stop() })

	if !s.Running() {
		t.Fatal("engine should be running after a successful Start")
	}

	h := s.Hello()
	if h == nil {
		t.Fatal("no Hello recorded after handshake")
	}
	if h.Proto != proto.ProtocolVersion {
		t.Errorf("engine protocol = %d, want %d", h.Proto, proto.ProtocolVersion)
	}
	if h.Role != proto.RoleHost {
		t.Errorf("engine role = %q, want %q", h.Role, proto.RoleHost)
	}
	if h.Name != "esper-engine" {
		t.Errorf("engine name = %q", h.Name)
	}
	if !h.DeviceKeyIsWellFormed() {
		t.Errorf("engine device key is %d bytes, want %d", len(h.Device), proto.DeviceKeyLen)
	}
}

// A mismatched protocol version must fail loudly and tear the process down,
// rather than proceeding into confusing downstream symptoms.
func TestVersionMismatchIsRejectedAndTearsDown(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	bad := controlPlaneHello()
	bad.Proto = proto.ProtocolVersion + 99

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	err = s.Start(ctx, bad)
	if err == nil {
		t.Fatal("a mismatched protocol version must not be accepted")
	}
	if s.Running() {
		t.Error("the engine must be torn down after a failed handshake")
	}
}

func TestStopIsIdempotent(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	for i := range 3 {
		if err := s.Stop(); err != nil {
			t.Fatalf("Stop #%d: %v", i+1, err)
		}
	}
	if s.Running() {
		t.Error("still running after Stop")
	}
}

func TestStartIsIdempotent(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("first Start: %v", err)
	}
	t.Cleanup(func() { _ = s.Stop() })
	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("second Start should be a no-op, got: %v", err)
	}
}

// A missing engine is almost always a partial install, so the error has to say
// where we looked rather than reporting a bare failure.
func TestMissingBinaryIsExplained(t *testing.T) {
	_, err := New(Options{Path: filepath.Join(t.TempDir(), "definitely-not-here")})
	if !errors.Is(err, ErrNotFound) {
		t.Fatalf("want ErrNotFound, got %v", err)
	}
	if !contains(err.Error(), "definitely-not-here") {
		t.Errorf("error should name the path it tried, got %q", err)
	}
}

func TestDirectoryIsNotMistakenForABinary(t *testing.T) {
	if _, err := New(Options{Path: t.TempDir()}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("a directory must not resolve as the engine, got %v", err)
	}
}

func TestNonExecutableFileIsRejected(t *testing.T) {
	p := filepath.Join(t.TempDir(), "esper-engine")
	if err := os.WriteFile(p, []byte("not a binary"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := New(Options{Path: p}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("a non-executable file must be rejected, got %v", err)
	}
}

func TestBackoffDoublesAndCaps(t *testing.T) {
	for _, tc := range []struct {
		attempt int
		want    time.Duration
	}{
		{0, time.Second},
		{1, time.Second},
		{2, 2 * time.Second},
		{3, 4 * time.Second},
		{4, 8 * time.Second},
		{5, 16 * time.Second},
		{6, 30 * time.Second},
		{100, 30 * time.Second},
	} {
		if got := Backoff(tc.attempt); got != tc.want {
			t.Errorf("Backoff(%d) = %v, want %v", tc.attempt, got, tc.want)
		}
	}
}

// An engine that dies mid-session must leave the supervisor in a state that
// reports honestly rather than hanging.
func TestSupervisorSurvivesTheEngineBeingKilled(t *testing.T) {
	bin := engineBinary(t)
	s, err := New(Options{Path: bin})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	t.Cleanup(func() { _ = s.Stop() })

	s.mu.Lock()
	pid := s.cmd.Process.Pid
	s.mu.Unlock()

	if err := exec.Command("kill", "-9", itoa(pid)).Run(); err != nil {
		t.Skipf("could not kill the process on this platform: %v", err)
	}
	// Stop must not hang or panic on an already-dead process.
	done := make(chan error, 1)
	go func() { done <- s.Stop() }()
	select {
	case err := <-done:
		if err != nil {
			t.Errorf("Stop after a kill: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Stop hung after the engine was killed")
	}
}

func contains(s, sub string) bool {
	return len(s) >= len(sub) && (len(sub) == 0 || indexOf(s, sub) >= 0)
}

func indexOf(s, sub string) int {
	for i := 0; i+len(sub) <= len(s); i++ {
		if s[i:i+len(sub)] == sub {
			return i
		}
	}
	return -1
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	var b []byte
	for n > 0 {
		b = append([]byte{byte('0' + n%10)}, b...)
		n /= 10
	}
	return string(b)
}

// Status is the first command the seam carries, and the engine answers it by
// probing rather than by reporting what it was built with.
func TestStatusReportsWhatThisMachineCanDo(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	t.Cleanup(func() { _ = s.Stop() })

	status, err := s.Status(ctx)
	if err != nil {
		t.Fatalf("Status: %v", err)
	}

	// Asserted on shape, not on outcome: whether this machine has a screen
	// depends on whether it has a screen, and a test that demanded one would
	// fail on every server this product exists for.
	if status.Engine == "" {
		t.Error("the engine did not say what version it is")
	}
	for name, c := range map[string]proto.Capability{
		"capture": status.Capture,
		"encoder": status.Encoder,
		"input":   status.Input,
	} {
		if c.Detail == "" {
			t.Errorf("%s answered with no detail — the useful half of a no is why", name)
		}
	}
	// The encoder is the one capability that needs nothing from the machine
	// but a CPU, so it is the one that can be asserted — and what is asserted
	// is the evidence rather than the verdict. A report that declared the
	// encoder working without running it would set the same boolean.
	if !status.Encoder.Available {
		t.Errorf("the encoder could not encode a frame: %s", status.Encoder.Detail)
	}
	if !strings.Contains(status.Encoder.Detail, "bytes for one keyframe") {
		t.Errorf("the encoder answered %q, which is not the size of a frame it encoded",
			status.Encoder.Detail)
	}
}

// The channel has no message identifiers, so the second exchange is the one
// that proves the first left the reader in the right place.
func TestStatusCanBeAskedTwice(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	t.Cleanup(func() { _ = s.Stop() })

	first, err := s.Status(ctx)
	if err != nil {
		t.Fatalf("first Status: %v", err)
	}
	second, err := s.Status(ctx)
	if err != nil {
		t.Fatalf("second Status: %v", err)
	}
	if first.Engine != second.Engine {
		t.Errorf("two answers disagreed about the engine: %q then %q", first.Engine, second.Engine)
	}
}

func TestStatusNeedsARunningEngine(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	if _, err := s.Status(context.Background()); !errors.Is(err, ErrNotRunning) {
		t.Fatalf("Status without a started engine = %v, want ErrNotRunning", err)
	}
}

// Everything the engine writes to stderr is attributed and bounded.
//
// Attributed, because a C library's warning that lands in the middle of a
// report with nothing to say where it came from is worse than no warning.
// Bounded, because a process printing a megabyte without a newline must not be
// able to make the supervisor allocate for it.
func TestEngineOutputIsRelayedWithItsOrigin(t *testing.T) {
	var lines []string
	s, err := New(Options{Path: engineBinary(t), Log: func(format string, args ...any) {
		lines = append(lines, fmt.Sprintf(format, args...))
	}})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	s.relay(strings.NewReader("first\n\nsecond\n" + strings.Repeat("x", maxLogLine*2) + "\n"))

	if len(lines) < 2 {
		t.Fatalf("relayed %d lines, want at least the two non-empty ones: %v", len(lines), lines)
	}
	if lines[0] != "engine: first" || lines[1] != "engine: second" {
		t.Errorf("relayed %v, want each line attributed to the engine", lines[:2])
	}
	for _, l := range lines {
		if len(l) > maxLogLine+len("engine: ") {
			t.Errorf("a relayed line was %d bytes, which is unbounded", len(l))
		}
	}
}

// TestACrashIsNoticed is the bug this reaper exists for.
//
// `running` used to be set at Start and cleared only at Stop, so an engine
// that died went on reporting itself as running for as long as the process
// that started it stayed up. Every question then failed on a broken pipe
// with a different sentence, and nothing could say the one true thing.
func TestACrashIsNoticed(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	t.Cleanup(func() { _ = s.Stop() })

	if !s.Running() {
		t.Fatal("a started engine should be running")
	}
	if s.Why() != nil {
		t.Errorf("a running engine has no trouble, got %v", s.Why())
	}

	s.mu.Lock()
	pid := s.cmd.Process.Pid
	s.mu.Unlock()
	if err := exec.Command("kill", "-9", itoa(pid)).Run(); err != nil {
		t.Skipf("could not kill the process on this platform: %v", err)
	}

	if !eventually(2*time.Second, func() bool { return !s.Running() }) {
		t.Fatal("a killed engine still reports itself as running")
	}
	trouble := s.Why()
	if trouble == nil {
		t.Fatal("an engine that died should say so")
	}
	if !errors.Is(trouble, ErrStopped) {
		t.Errorf("the reason should be that it stopped on its own, got %v", trouble)
	}
	if !contains(trouble.Error(), "killed") && !contains(trouble.Error(), "signal") {
		t.Errorf("the reason should name how it ended, got %q", trouble)
	}
	// **And it should not blame the engine's last words.** A SIGKILL is
	// something that happened to the engine; whatever it said most recently
	// is unrelated by construction, and the first time this printed in anger
	// it offered a harmless codec warning as the reason.
	if contains(trouble.Error(), "last:") {
		t.Errorf("a killed engine's last words are not the reason: %q", trouble)
	}
}

// TestWhoEndedItDecidesWhetherItsWordsAreQuoted is that cut, stated on its
// own. A process that broke has a reason in its last line; a process
// somebody stopped does not.
func TestWhoEndedItDecidesWhetherItsWordsAreQuoted(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		why    string
		script string
		quoted bool
	}{
		{"a non-zero exit of its own choosing", "echo 'the config is nonsense' >&2; exit 3", true},
		{"a fault it raised by breaking", "echo 'thread panicked at src/main.rs:12' >&2; kill -ABRT $$", true},
		{"stopped from outside", "echo 'a routine warning' >&2; sleep 0.2; kill -KILL $$", false},
		{"asked to stop from outside", "echo 'a routine warning' >&2; sleep 0.2; kill -TERM $$", false},
	} {
		s := &Supervisor{log: func(string, ...any) {}}
		cmd := exec.Command(script(t, c.script))
		var out strings.Builder
		cmd.Stderr = &out
		if err := cmd.Start(); err != nil {
			t.Fatalf("%s: start: %v", c.why, err)
		}
		err := cmd.Wait()
		s.said = splitLines(out.String())
		if got := s.lastWordsFor(err) != ""; got != c.quoted {
			t.Errorf("%s: quoted = %v, wanted %v (said %q, err %v)", c.why, got, c.quoted, s.said, err)
		}
	}
}

func splitLines(s string) []string {
	var kept []string
	for _, line := range strings.Split(s, "\n") {
		if strings.TrimSpace(line) != "" {
			kept = append(kept, line)
		}
	}
	return kept
}

// TestRestartBringsItBack — the whole point of noticing is being able to
// offer the one thing that fixes it.
func TestRestartBringsItBack(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if err := s.Start(ctx, controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	t.Cleanup(func() { _ = s.Stop() })

	s.mu.Lock()
	first := s.cmd.Process.Pid
	s.mu.Unlock()
	if err := exec.Command("kill", "-9", itoa(first)).Run(); err != nil {
		t.Skipf("could not kill the process on this platform: %v", err)
	}
	if !eventually(2*time.Second, func() bool { return !s.Running() }) {
		t.Fatal("the crash was not noticed")
	}

	// No Hello is passed: a caller recovering from a crash should not have to
	// have kept one.
	if err := s.Restart(); err != nil {
		t.Fatalf("Restart: %v", err)
	}
	if !s.Running() {
		t.Fatal("the engine should be running again")
	}
	if s.Why() != nil {
		t.Errorf("and it should have nothing to complain about, got %v", s.Why())
	}
	s.mu.Lock()
	second := s.cmd.Process.Pid
	s.mu.Unlock()
	if second == first {
		t.Error("Restart should have started a new process")
	}
	if s.Hello() == nil {
		t.Error("the new engine should have been greeted")
	}

	// **And it must still be there a moment later.** Restart took the
	// caller's context once, and the caller with a reason to restart an
	// engine is an HTTP handler whose context ends when it answers: the new
	// engine was killed a millisecond after it handshook, and the control
	// plane reported a crash it had caused itself.
	time.Sleep(300 * time.Millisecond)
	if !s.Running() {
		t.Fatalf("the engine did not survive its own restart: %v", s.Why())
	}
}

// TestARestartOutlivesWhoAskedForIt states that trap on its own, in the
// shape the API actually uses: a context that is cancelled immediately.
func TestARestartOutlivesWhoAskedForIt(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t)})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	life, endLife := context.WithCancel(context.Background())
	defer endLife()
	if err := s.Start(life, controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	t.Cleanup(func() { _ = s.Stop() })

	if err := s.Restart(); err != nil {
		t.Fatalf("Restart: %v", err)
	}
	// Long enough that a process bound to something already finished would
	// be gone, and short enough that nobody minds.
	time.Sleep(300 * time.Millisecond)
	if !s.Running() {
		t.Errorf("the engine died with the request that restarted it: %v", s.Why())
	}
}

// eventually polls until the condition holds or the patience runs out.
//
// Polled rather than slept: the reaper runs on a goroutine of its own, and a
// fixed sleep would either be flaky or slow, depending which way it was wrong.
func eventually(patience time.Duration, holds func() bool) bool {
	deadline := time.Now().Add(patience)
	for time.Now().Before(deadline) {
		if holds() {
			return true
		}
		time.Sleep(10 * time.Millisecond)
	}
	return holds()
}
