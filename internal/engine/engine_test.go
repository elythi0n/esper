package engine

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"github.com/elythi0n/ownview/internal/proto"
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
	p := filepath.Join(root, "build", "ownview-engine")
	if _, err := os.Stat(p); err != nil {
		t.Skip("no built engine at build/ownview-engine — run 'make build'")
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
	if h.Name != "ownview-engine" {
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
	p := filepath.Join(t.TempDir(), "ownview-engine")
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
