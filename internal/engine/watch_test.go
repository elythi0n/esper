package engine

import (
	"context"
	"errors"
	"fmt"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/elythi0n/esper/internal/proto"
)

// TestFakeEngineHelper is not a test. It is the program the supervisor tests
// supervise.
//
// A shell script cannot speak length-prefixed CBOR over a socket, and shipping
// a second binary to build would put the protocol in three places. `go test`
// has already compiled this package into a program; running it again with one
// environment variable set is how that program becomes an engine.
func TestFakeEngineHelper(t *testing.T) {
	how := os.Getenv("ESPER_FAKE_ENGINE")
	if how == "" {
		t.Skip("not the helper process")
	}
	if runs := os.Getenv("ESPER_FAKE_RUNS"); runs != "" {
		f, err := os.OpenFile(runs, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
		if err == nil {
			_, _ = f.WriteString("x")
			_ = f.Close()
		}
	}
	// os.Exit rather than a return: the testing framework would otherwise
	// print PASS into the terminal a supervised host is reporting on.
	os.Exit(behaveLikeAnEngine(how, os.Getenv("ESPER_FAKE_SOCKET")))
}

// behaveLikeAnEngine is every way an engine can behave that a supervisor has
// to have an answer for.
func behaveLikeAnEngine(how, socket string) int {
	if how == "noisy-crash" {
		fmt.Fprintln(os.Stderr, "host: this display cannot show a session indicator")
		return 1
	}
	if how == "seam-deaf" {
		return beDeafOnTheSeam()
	}
	if socket == "" {
		fmt.Fprintln(os.Stderr, "the supervisor did not say where to connect")
		return 2
	}
	conn, err := net.Dial("unix", socket)
	if err != nil {
		fmt.Fprintf(os.Stderr, "dialing %s: %v\n", socket, err)
		return 2
	}
	defer func() { _ = conn.Close() }()

	hello := proto.NewHello(proto.RoleHost, nil, "fake-engine", "0.0.0")
	if how == "mismatch" {
		hello.Proto++
	}
	if err := proto.WriteFrame(conn, hello); err != nil {
		return 2
	}
	var theirs proto.Hello
	if err := proto.ReadFrame(conn, &theirs); err != nil {
		// A supervisor that hung up on our greeting has already decided.
		return 3
	}

	answer := 0
	switch how {
	case "answers-then-stops":
		answer = 2
	case "answers":
		answer = -1
	case "ignores-term":
		answer = -1
		// Ignored rather than handled: what is being tested is a supervisor
		// that has to kill something, and a process that exits on the signal
		// would never make it do so.
		signal.Ignore(syscall.SIGTERM)
	}
	for answer != 0 {
		var command proto.Command
		if err := proto.ReadFrame(conn, &command); err != nil {
			return 0
		}
		report := proto.Report{Alive: &proto.Alive{
			Since:     1_756_700_000,
			Listening: "127.0.0.1:7645",
			Served:    1,
		}}
		if err := proto.WriteFrame(conn, report); err != nil {
			return 0
		}
		if answer > 0 {
			answer--
		}
	}
	// Answered what it was going to answer. Now it stops reading, which from
	// outside is indistinguishable from a host wedged holding a session — and
	// that is the whole point of the exercise.
	select {}
}

// beDeafOnTheSeam greets a control plane over stdio and then ignores the
// channel closing, which is the one clean exit the seam's loop has.
func beDeafOnTheSeam() int {
	var theirs proto.Hello
	if err := proto.ReadFrame(os.Stdin, &theirs); err != nil {
		return 2
	}
	if err := proto.WriteFrame(os.Stdout, proto.NewHello(proto.RoleHost, nil, "fake-engine", "0.0.0")); err != nil {
		return 2
	}
	select {}
}

// fakeEngine returns the path to a program that behaves like an engine in the
// way `how` names, and a file whose length is how many times it has run.
//
// The wrapper is a shell script because the supervisor passes `--watched-by`
// on the command line and Go's testing package refuses to start with a flag it
// does not recognize. Turning the flag into an environment variable in the
// wrapper keeps the fake honest about being told where to connect: a
// supervisor that stopped passing the flag would fail these tests.
func fakeEngine(t *testing.T, how string) (string, string) {
	t.Helper()
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	runs := filepath.Join(t.TempDir(), "runs")
	t.Setenv("ESPER_FAKE_ENGINE", how)
	t.Setenv("ESPER_FAKE_RUNS", runs)
	body := `while [ $# -gt 0 ]; do
  case "$1" in
    --watched-by) ESPER_FAKE_SOCKET="$2"; export ESPER_FAKE_SOCKET; shift 2 ;;
    *) shift ;;
  esac
done
exec ` + strconv.Quote(self) + ` -test.run='^TestFakeEngineHelper$'`
	return script(t, body), runs
}

// ranTimes is how many times the fake engine started.
func ranTimes(t *testing.T, runs string) int {
	t.Helper()
	body, err := os.ReadFile(runs)
	if errors.Is(err, os.ErrNotExist) {
		return 0
	}
	if err != nil {
		t.Fatalf("reading the run counter: %v", err)
	}
	return strings.Count(string(body), "x")
}

// closely is a Watch that does everything at test speed.
func closely() Watch {
	return Watch{
		Delay:        func(int) time.Duration { return time.Millisecond },
		Settled:      time.Hour,
		Heartbeat:    20 * time.Millisecond,
		Answer:       500 * time.Millisecond,
		Misses:       2,
		Grace:        300 * time.Millisecond,
		MostFailures: 20,
	}
}

// An engine that answers is left alone.
//
// The first thing to establish, because every other test here is about a
// supervisor deciding something is wrong: one that decided that about a
// working engine would restart a host in the middle of somebody's session.
func TestAnEngineThatAnswersIsLeftAlone(t *testing.T) {
	path, runs := fakeEngine(t, "answers")
	s, _ := supervisorFor(t, path)

	ctx, cancel := context.WithTimeout(context.Background(), 600*time.Millisecond)
	defer cancel()
	if err := s.Supervise(ctx, nil, closely()); err != nil {
		t.Fatalf("Supervise = %v, want nil", err)
	}
	// 600ms at a 20ms heartbeat is around thirty questions, all answered.
	if got := ranTimes(t, runs); got != 1 {
		t.Errorf("the engine ran %d times, want 1 — a supervisor restarted one that was answering", got)
	}
}

// The first question is asked at once rather than a heartbeat later.
//
// A supervisor that reported a host as watched and then waited ten seconds to
// find out whether it answers has spent the most interesting moment of a
// process's life not looking.
func TestTheFirstQuestionIsAskedStraightAway(t *testing.T) {
	path, _ := fakeEngine(t, "answers")
	s, lines := supervisorFor(t, path)

	w := closely()
	// A heartbeat far longer than the test runs: any answer at all must
	// therefore be the one asked before the first tick.
	w.Heartbeat = time.Hour
	ctx, cancel := context.WithTimeout(context.Background(), 400*time.Millisecond)
	defer cancel()
	if err := s.Supervise(ctx, nil, w); err != nil {
		t.Fatalf("Supervise = %v, want nil", err)
	}
	said := lines.joined()
	if !strings.Contains(said, "the engine answered") {
		t.Errorf("nothing was asked before the first heartbeat:\n%s", said)
	}
	if !strings.Contains(said, "127.0.0.1:7645") || !strings.Contains(said, "serving nobody") {
		t.Errorf("the answer was not reported:\n%s", said)
	}
}

// An engine that stops answering is restarted, and the reason says what it was
// doing when it last did.
//
// This is the failure supervision existed and could not see: a crash comes
// back on its own, and a wedge looks exactly like a working host from outside.
func TestAnEngineThatStopsAnsweringIsRestarted(t *testing.T) {
	path, runs := fakeEngine(t, "answers-then-stops")
	s, lines := supervisorFor(t, path)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_ = s.Supervise(ctx, nil, closely())

	if got := ranTimes(t, runs); got < 2 {
		t.Fatalf("the engine ran %d times, want it restarted after it went quiet", got)
	}
	said := lines.joined()
	if !strings.Contains(said, "stopped answering") {
		t.Errorf("the log does not say the engine stopped answering:\n%s", said)
	}
	// And it says what the last answer was, because "it stopped answering" on
	// its own is not something anybody can act on.
	if !strings.Contains(said, "127.0.0.1:7645") || !strings.Contains(said, "serving nobody") {
		t.Errorf("the log does not say what the engine last reported:\n%s", said)
	}
}

// Two binaries that disagree about the protocol are a mixed install, and a
// sixth attempt would disagree the same way.
func TestAnEngineFromADifferentInstallIsNotRetried(t *testing.T) {
	path, runs := fakeEngine(t, "mismatch")
	s, _ := supervisorFor(t, path)

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	err := s.Supervise(ctx, nil, closely())
	if !errors.Is(err, ErrVersionMismatch) {
		t.Fatalf("Supervise = %v, want ErrVersionMismatch", err)
	}
	if !strings.Contains(err.Error(), "different install") {
		t.Errorf("the error does not say what is wrong: %v", err)
	}
	if got := ranTimes(t, runs); got != 1 {
		t.Errorf("the engine ran %d times, want 1 — a mismatch will not fix itself", got)
	}
}

// An engine is asked to stop, and killed only if it will not.
//
// The ask is the whole reason a supervisor signals rather than kills: the
// teardown closes sessions with a reason and puts the screen back, and only
// the engine knows how to do that. The bound is the other half — an unbounded
// wait would let a wedged engine stop the machine from shutting down.
func TestAnEngineThatWillNotStopIsKilledAndItIsSaid(t *testing.T) {
	path, _ := fakeEngine(t, "ignores-term")
	s, lines := supervisorFor(t, path)

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- s.Supervise(ctx, nil, closely()) }()

	time.Sleep(200 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Supervise after cancel = %v, want nil", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("an engine that ignores SIGTERM was never killed")
	}
	said := lines.joined()
	if !strings.Contains(said, "being killed") {
		t.Errorf("the log does not say the engine had to be killed:\n%s", said)
	}
}

// When the supervisor gives up, it quotes what the engine said.
//
// Without this the message is "the engine keeps failing to run: 5 times in a
// row, last: exit status 1" — a sentence containing everything except the
// reason, which the engine had already printed.
func TestWhatTheEngineLastSaidIsQuotedWhenItIsGivenUpOn(t *testing.T) {
	path, _ := fakeEngine(t, "noisy-crash")
	s, _ := supervisorFor(t, path)

	w := closely()
	w.MostFailures = 2
	err := s.Supervise(context.Background(), nil, w)
	if !errors.Is(err, ErrGaveUp) {
		t.Fatalf("Supervise = %v, want ErrGaveUp", err)
	}
	if !strings.Contains(err.Error(), "session indicator") {
		t.Errorf("giving up did not quote what the engine said: %v", err)
	}
}

// Only the process the supervisor started may answer for it.
//
// The socket is in a directory only its owner can enter, which keeps other
// people out. This is the other half: another program belonging to the same
// person must not be able to answer a health check and hand back an extract of
// the audit trail on the engine's behalf.
func TestSomethingOtherThanTheEngineIsRefused(t *testing.T) {
	ch, err := openWatchChannel()
	if err != nil {
		t.Fatalf("openWatchChannel: %v", err)
	}
	defer ch.close()

	said := &logged{}

	// The impostor does everything right except being the right process: it
	// dials and completes the handshake. Without that it would fail the
	// deadline instead, and a test that passes because the impostor was slow
	// proves nothing about the check.
	conn, err := net.Dial("unix", ch.path)
	if err != nil {
		t.Fatalf("dialing the channel: %v", err)
	}
	defer func() { _ = conn.Close() }()
	go func() {
		_ = proto.WriteFrame(conn, proto.NewHello(proto.RoleHost, nil, "impostor", "0.0.0"))
		var theirs proto.Hello
		_ = proto.ReadFrame(conn, &theirs)
	}()

	// This process is not the pid the supervisor is waiting for, so the
	// connection must be refused and the wait must go on.
	err = ch.accept(os.Getpid()+1, controlPlaneHello(), 300*time.Millisecond, said.collect())
	if err == nil {
		t.Fatal("a connection from another process was accepted")
	}
	if !strings.Contains(said.joined(), "was refused") {
		t.Errorf("the refusal was not reported: %v", said.all())
	}
}

// And the process that was started is accepted, on the same path.
//
// Its own test, because a check that refuses everything passes the one above
// while making supervision impossible.
func TestTheProcessTheSupervisorStartedIsAccepted(t *testing.T) {
	ch, err := openWatchChannel()
	if err != nil {
		t.Fatalf("openWatchChannel: %v", err)
	}
	defer ch.close()

	greeted := make(chan error, 1)
	go func() {
		conn, err := net.Dial("unix", ch.path)
		if err != nil {
			greeted <- err
			return
		}
		defer func() { _ = conn.Close() }()
		if err := proto.WriteFrame(conn, proto.NewHello(proto.RoleHost, nil, "fake-engine", "0.0.0")); err != nil {
			greeted <- err
			return
		}
		var theirs proto.Hello
		greeted <- proto.ReadFrame(conn, &theirs)
	}()

	if err := ch.accept(os.Getpid(), controlPlaneHello(), 3*time.Second, func(string, ...any) {}); err != nil {
		t.Fatalf("accept from our own pid: %v", err)
	}
	if err := <-greeted; err != nil {
		t.Fatalf("the engine side of the handshake: %v", err)
	}
	if ch.hello.Engine != "0.0.0" {
		t.Errorf("the engine's greeting was not kept: %+v", ch.hello)
	}
}

// The socket nobody else can reach is checked rather than assumed.
func TestTheWatchSocketIsPrivateToItsOwner(t *testing.T) {
	ch, err := openWatchChannel()
	if err != nil {
		t.Fatalf("openWatchChannel: %v", err)
	}
	defer ch.close()

	dir, err := os.Stat(ch.dir)
	if err != nil {
		t.Fatal(err)
	}
	if dir.Mode().Perm()&0o077 != 0 {
		t.Errorf("the socket's directory is %v, which others can enter", dir.Mode().Perm())
	}
	sock, err := os.Stat(ch.path)
	if err != nil {
		t.Fatal(err)
	}
	if sock.Mode().Perm()&0o077 != 0 {
		t.Errorf("the socket is %v, which others can open", sock.Mode().Perm())
	}
}

// And everything it made is gone afterwards.
func TestClosingTheChannelLeavesNothingBehind(t *testing.T) {
	ch, err := openWatchChannel()
	if err != nil {
		t.Fatalf("openWatchChannel: %v", err)
	}
	dir := ch.dir
	ch.close()
	if _, err := os.Stat(dir); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("%s is still there after close: %v", dir, err)
	}
}

// The seam asks the engine to end before killing it.
//
// Closing the channel is the ask, and the engine's command loop treats it as
// the ordinary way to end. Until Stop waited for that, the line after the
// close killed the process before it could take it.
func TestTheSeamAsksTheEngineToEndBeforeKillingIt(t *testing.T) {
	s, err := New(Options{Path: engineBinary(t), Grace: 3 * time.Second})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	said := &logged{}
	s.log = said.collect()

	if err := s.Start(context.Background(), controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	began := time.Now()
	if err := s.Stop(); err != nil {
		t.Fatalf("Stop: %v", err)
	}
	if took := time.Since(began); took > 2*time.Second {
		t.Errorf("stopping took %v, so the engine was not taking the hint", took)
	}
	if strings.Contains(said.joined(), "being killed") {
		t.Errorf("the real engine was killed rather than asked:\n%s", said.joined())
	}
}

// And kills one that will not take it, rather than waiting for ever.
func TestTheSeamKillsAnEngineThatIgnoresTheClosedChannel(t *testing.T) {
	path, _ := fakeEngine(t, "seam-deaf")
	s, err := New(Options{Path: path, Grace: 200 * time.Millisecond})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	said := &logged{}
	s.log = said.collect()

	if err := s.Start(context.Background(), controlPlaneHello()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	done := make(chan error, 1)
	go func() { done <- s.Stop() }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Stop: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Stop never returned for an engine that ignored the closed channel")
	}
	if !strings.Contains(said.joined(), "being killed") {
		t.Errorf("an engine that would not end was not reported as killed:\n%s",
			said.joined())
	}
}
