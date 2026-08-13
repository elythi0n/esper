package server

import (
	"bufio"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/elythi0n/esper/internal/proto"
)

// A stand-in engine: the API's job is to answer, not to spawn anything.
type answers struct {
	status   *proto.Status
	sessions []proto.Happening
	running  bool
	asked    int
}

func (a *answers) Status(context.Context) (*proto.Status, error) {
	a.asked++
	return a.status, nil
}

func (a *answers) Sessions(_ context.Context, most uint32) ([]proto.Happening, error) {
	a.asked++
	if int(most) < len(a.sessions) {
		return a.sessions[:most], nil
	}
	return a.sessions, nil
}

func (a *answers) Running() bool { return a.running }

func serverForTest(t *testing.T) (*Server, Token) {
	t.Helper()
	token, err := LoadOrCreateToken(t.TempDir())
	if err != nil {
		t.Fatalf("a token: %v", err)
	}
	s, err := New(Options{
		Addr:  "127.0.0.1:8687",
		Token: token,
		// A store of this test's own. Left empty it would resolve to the
		// machine's real one, and a test suite that reads the operator's trust
		// store is a test suite nobody should run.
		Store:  filepath.Join(t.TempDir(), "devices.json"),
		Engine: &answers{running: true, status: &proto.Status{Engine: "test"}},
	})
	if err != nil {
		t.Fatalf("a server: %v", err)
	}
	return s, token
}

func ask(t *testing.T, s *Server, path string, prepare func(*http.Request)) *httptest.ResponseRecorder {
	t.Helper()
	r := httptest.NewRequest(http.MethodGet, "http://127.0.0.1:8687"+path, nil)
	if prepare != nil {
		prepare(r)
	}
	w := httptest.NewRecorder()
	s.http.Handler.ServeHTTP(w, r)
	return w
}

// **A request without the token is refused**, which is the whole of why the
// token exists: every process and every user on this machine can reach
// 127.0.0.1, so "it only listens on loopback" is not access control.
func TestATokenIsRequired(t *testing.T) {
	s, token := serverForTest(t)

	for _, route := range []string{"/health", "/status", "/sessions", "/events"} {
		if got := ask(t, s, route, nil).Code; got != http.StatusUnauthorized {
			t.Errorf("%s answered %d without a token, wanted 401", route, got)
		}
		wrong := ask(t, s, route, func(r *http.Request) {
			r.Header.Set("Authorization", "Bearer "+token.String()+"x")
		})
		if wrong.Code != http.StatusUnauthorized {
			t.Errorf("%s answered %d to a wrong token, wanted 401", route, wrong.Code)
		}
	}

	ok := ask(t, s, "/health", func(r *http.Request) {
		r.Header.Set("Authorization", "Bearer "+token.String())
	})
	if ok.Code != http.StatusOK {
		t.Fatalf("the right token was refused: %d %s", ok.Code, ok.Body.String())
	}
}

// **A request that arrived under a *name* is refused**, even with the token.
//
// A page in a browser can send requests to 127.0.0.1, and DNS rebinding lets
// it do so under a hostname it controls — the name resolves to loopback, the
// browser treats the origin as its own, and the page reads the reply. Only the
// literal address this server bound is accepted, because a name cannot be
// rebound to something that is not a name.
func TestAHostnameIsRefusedEvenWithTheToken(t *testing.T) {
	s, token := serverForTest(t)
	withToken := func(r *http.Request) {
		r.Header.Set("Authorization", "Bearer "+token.String())
	}

	for _, host := range []string{
		"localhost:8687",         // a name, and the obvious one
		"esper.example.com:8687", // a name an attacker controls
		"my-router.local:8687",
		"",
	} {
		w := httptest.NewRecorder()
		r := httptest.NewRequest(http.MethodGet, "http://127.0.0.1:8687/status", nil)
		withToken(r)
		r.Host = host
		s.http.Handler.ServeHTTP(w, r)
		if w.Code != http.StatusNotFound {
			t.Errorf("Host %q answered %d, wanted 404", host, w.Code)
		}
	}

	// And the literal address is accepted.
	w := httptest.NewRecorder()
	r := httptest.NewRequest(http.MethodGet, "http://127.0.0.1:8687/status", nil)
	withToken(r)
	r.Host = "127.0.0.1:8687"
	s.http.Handler.ServeHTTP(w, r)
	if w.Code != http.StatusOK {
		t.Fatalf("the address this server bound was refused: %d", w.Code)
	}
}

// **Binding off loopback takes saying so twice.** Somebody who mistypes an
// address gets a refusal rather than a listener on their office network.
func TestBindingOffLoopbackIsRefusedUnlessMeant(t *testing.T) {
	token, err := LoadOrCreateToken(t.TempDir())
	if err != nil {
		t.Fatalf("a token: %v", err)
	}
	engine := &answers{}

	for _, addr := range []string{"0.0.0.0:8687", "192.168.1.10:8687", ":8687", "[::]:8687"} {
		_, err := New(Options{Addr: addr, Token: token, Engine: engine})
		if err == nil {
			t.Errorf("%s was accepted without --exposed", addr)
			continue
		}
		if !strings.Contains(err.Error(), "--exposed") {
			t.Errorf("the refusal for %s does not say what to do: %v", addr, err)
		}
		// And it says what would have been exposed, because a person who meant
		// it needs to know what they are agreeing to.
		if !strings.Contains(err.Error(), "paired") {
			t.Errorf("the refusal for %s does not name what it would expose: %v", addr, err)
		}
	}

	if _, err := New(Options{Addr: "0.0.0.0:8687", Token: token, Engine: engine, Exposed: true}); err != nil {
		t.Fatalf("somebody who said --exposed was still refused: %v", err)
	}

	// A *name* is refused whether or not it was meant: what it resolves to can
	// change under a running server.
	if _, err := New(Options{Addr: "localhost:8687", Token: token, Engine: engine, Exposed: true}); err == nil {
		t.Error("a hostname was accepted as a bind address")
	}
}

// A server with no token refuses to exist, rather than serving without one.
func TestAServerWithoutATokenRefusesToStart(t *testing.T) {
	if _, err := New(Options{Engine: &answers{}}); err == nil {
		t.Fatal("a server with no token was built")
	}
}

// **The token file is not readable by others**, and one that is stops the
// server rather than being fixed quietly: a token anybody can read is not one,
// and carrying on would make this package's promise false while appearing to
// keep it.
func TestATokenOthersCanReadIsRefused(t *testing.T) {
	home := t.TempDir()
	if _, err := LoadOrCreateToken(home); err != nil {
		t.Fatalf("a token: %v", err)
	}
	path := filepath.Join(home, TokenFile)

	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if mode := info.Mode().Perm(); mode != 0o600 {
		t.Fatalf("a fresh token file is mode %04o, wanted 0600", mode)
	}

	if err := os.Chmod(path, 0o644); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	if _, err := LoadOrCreateToken(home); err == nil {
		t.Fatal("a world-readable token was accepted")
	} else if !strings.Contains(err.Error(), "chmod") {
		t.Errorf("the refusal does not say how to fix it: %v", err)
	}
}

// The same token comes back on a second start: a token that changed every time
// would be one no dashboard could keep.
func TestTheTokenSurvivesARestart(t *testing.T) {
	home := t.TempDir()
	first, err := LoadOrCreateToken(home)
	if err != nil {
		t.Fatalf("first: %v", err)
	}
	second, err := LoadOrCreateToken(home)
	if err != nil {
		t.Fatalf("second: %v", err)
	}
	if first.String() != second.String() {
		t.Fatal("the token changed between starts")
	}
	if !first.Matches(second.String()) {
		t.Fatal("a token does not match itself")
	}
}

// `/sessions` answers with the trail, and honors a limit.
func TestSessionsAnswersWithWhatHappened(t *testing.T) {
	token, err := LoadOrCreateToken(t.TempDir())
	if err != nil {
		t.Fatalf("a token: %v", err)
	}
	engine := &answers{
		running: true,
		sessions: []proto.Happening{
			{At: 1, Peer: "one", Event: "session_opened", Direction: "in"},
			{At: 2, Peer: "two", Event: "session_closed", Direction: "out"},
			{At: 3, Peer: "three", Event: "recording"},
		},
	}
	s, err := New(Options{Addr: "127.0.0.1:8687", Token: token, Engine: engine})
	if err != nil {
		t.Fatalf("a server: %v", err)
	}

	w := ask(t, s, "/sessions?most=2", func(r *http.Request) {
		r.Header.Set("Authorization", "Bearer "+token.String())
	})
	if w.Code != http.StatusOK {
		t.Fatalf("answered %d: %s", w.Code, w.Body.String())
	}
	var got []proto.Happening
	if err := json.Unmarshal(w.Body.Bytes(), &got); err != nil {
		t.Fatalf("the body is not JSON: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("asked for 2 and got %d", len(got))
	}
	if got[0].Peer != "one" || got[1].Direction != "out" {
		t.Errorf("the lines came back changed: %+v", got)
	}

	// A limit that is not a number is a refusal rather than a default: a
	// caller that mistyped it should hear about it.
	bad := ask(t, s, "/sessions?most=lots", func(r *http.Request) {
		r.Header.Set("Authorization", "Bearer "+token.String())
	})
	if bad.Code != http.StatusBadRequest {
		t.Errorf("most=lots answered %d, wanted 400", bad.Code)
	}
}

// **The event stream says it is open before anything happens**, so a caller
// knows it is connected rather than waiting to find out from the first
// session — and then carries lines as they arrive.
//
// Over a real loopback server rather than a recorder: a recorder cannot be
// read while a handler is still writing to it, and the first version of this
// test raced against its own subscriber not having joined yet.
func TestTheEventStreamOpensAndCarriesLines(t *testing.T) {
	s, token := serverForTest(t)
	live := httptest.NewServer(s.http.Handler)
	defer live.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	r, err := http.NewRequestWithContext(ctx, http.MethodGet, live.URL+"/events", nil)
	if err != nil {
		t.Fatalf("a request: %v", err)
	}
	r.Header.Set("Authorization", "Bearer "+token.String())
	resp, err := live.Client().Do(r)
	if err != nil {
		t.Fatalf("opening the stream: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("the stream answered %d", resp.StatusCode)
	}
	if got := resp.Header.Get("Content-Type"); got != "text/event-stream" {
		t.Errorf("a stream has to say what it is: %q", got)
	}

	lines := bufio.NewReader(resp.Body)
	first, err := lines.ReadString('\n')
	if err != nil {
		t.Fatalf("reading the first line: %v", err)
	}
	if !strings.HasPrefix(first, ": open") {
		t.Errorf("the stream did not say it was open first: %q", first)
	}

	// Now that the stream is open — and therefore joined — one line.
	s.Publish(proto.Happening{At: 7, Peer: "somebody", Event: "session_opened"})

	var body strings.Builder
	for range 8 {
		line, err := lines.ReadString('\n')
		if err != nil {
			break
		}
		body.WriteString(line)
		if strings.Contains(body.String(), "session_opened") {
			break
		}
	}
	if !strings.Contains(body.String(), "event: session") {
		t.Errorf("no session event in the stream: %q", body.String())
	}
	if !strings.Contains(body.String(), `"peer":"somebody"`) {
		t.Errorf("the line did not survive the stream: %q", body.String())
	}
}
