package main

import (
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/elythi0n/esper/internal/server"
)

// aToken makes a real token the way the window does.
func aToken(t *testing.T) server.Token {
	t.Helper()
	token, err := server.LoadOrCreateToken(t.TempDir())
	if err != nil {
		t.Fatalf("making a token: %v", err)
	}
	return token
}

// anAPI stands up something that behaves like the control plane's `/health`
// for one token, on a real loopback address.
func anAPI(t *testing.T, accepts server.Token) string {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		presented := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
		if !accepts.Matches(presented) {
			w.Header().Set("WWW-Authenticate", `Bearer realm="esper"`)
			http.Error(w, "a token is required", http.StatusUnauthorized)
			return
		}
		_, _ = w.Write([]byte(`{"running":true}`))
	})}
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(func() { _ = server.Close() })
	return listener.Addr().String()
}

// TestARefusalIsNotAnAbsence is the bug this seam existed to have.
//
// The window attaches the token itself, so a 401 from upstream can only mean
// the thing on the address is not the API that token belongs to. Passed
// through untouched it reached the page as one more failed request, the
// first-run screen said "nothing is answering", and it said so for ever
// while another Esper answered every ping — with a remedy (`esper api`) that
// could not work, because one was already running.
func TestARefusalIsNotAnAbsence(t *testing.T) {
	t.Parallel()
	stranger := anAPI(t, aToken(t)) // accepts a token that is not ours
	proxy, err := proxyToAPI(stranger, aToken(t))
	if err != nil {
		t.Fatalf("building the proxy: %v", err)
	}

	recorded := httptest.NewRecorder()
	proxy.ServeHTTP(recorded, httptest.NewRequest(http.MethodGet, "/api/health", nil))

	if recorded.Code != http.StatusUnauthorized {
		t.Fatalf("a refusal should stay a refusal, got %d", recorded.Code)
	}
	said := recorded.Body.String()
	if !strings.Contains(said, stranger) {
		t.Errorf("the sentence should name the address it happened at, got %q", said)
	}
	if strings.Contains(said, "a token is required") {
		t.Errorf("the upstream's wording reached the page unchanged: %q", said)
	}
	// The header would make a browser prompt for a password nobody has.
	if recorded.Header().Get("Www-Authenticate") != "" {
		t.Error("the challenge should not reach the page")
	}
}

// TestTheProxyPassesOurTokenAndTheAnswer holds the other half: with the
// right token, a 401 is not manufactured and the body is untouched.
func TestTheProxyPassesOurTokenAndTheAnswer(t *testing.T) {
	t.Parallel()
	token := aToken(t)
	proxy, err := proxyToAPI(anAPI(t, token), token)
	if err != nil {
		t.Fatalf("building the proxy: %v", err)
	}

	recorded := httptest.NewRecorder()
	proxy.ServeHTTP(recorded, httptest.NewRequest(http.MethodGet, "/api/health", nil))

	if recorded.Code != http.StatusOK {
		t.Fatalf("our own API answered %d: %s", recorded.Code, recorded.Body.String())
	}
	if got := recorded.Body.String(); got != `{"running":true}` {
		t.Errorf("the answer was rewritten: %q", got)
	}
}

// TestWhatIsOnThePortIsThreeAnswers is why the window can say which fault it
// is. A dial says the port is taken; it does not say by what, and telling
// somebody to start a control plane when one is already running is advice
// that cannot work.
func TestWhatIsOnThePortIsThreeAnswers(t *testing.T) {
	t.Parallel()
	token := aToken(t)

	if got := whoIsThere(anAPI(t, token), token); got != ours {
		t.Errorf("an API that takes our token should be ours, got %v", got)
	}
	if got := whoIsThere(anAPI(t, aToken(t)), token); got != stranger {
		t.Errorf("an API that refuses our token should be a stranger, got %v", got)
	}

	// A port nothing holds: bound, read, and released.
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	free := listener.Addr().String()
	_ = listener.Close()
	if got := whoIsThere(free, token); got != nobody {
		t.Errorf("an address nothing holds should be nobody, got %v", got)
	}
}

// documentServed runs one document request through the rewriter.
func documentServed(t *testing.T, route, addr string) string {
	t.Helper()
	page := `<html><head><title>Esper</title></head><body></body></html>`
	next := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html")
		_, _ = w.Write([]byte(page))
	})
	recorded := httptest.NewRecorder()
	serveDocument(recorded, httptest.NewRequest(http.MethodGet, "/", nil), next, route, addr)
	body, err := io.ReadAll(recorded.Body)
	if err != nil {
		t.Fatalf("reading the document: %v", err)
	}
	return string(body)
}

// TestTheDocumentKnowsItIsAWindow is the reason this is not asked for over
// HTTP. The bar carries the only button that closes a frameless window, so a
// page that learns about its own chrome a frame late can also fail to learn
// it at all — and then the window cannot be shut.
func TestTheDocumentKnowsItIsAWindow(t *testing.T) {
	t.Parallel()
	got := documentServed(t, "", "127.0.0.1:8687")

	if !strings.Contains(got, `d.desktop="true"`) {
		t.Errorf("the document does not say it is a window: %q", got)
	}
	if !strings.Contains(got, `d.api="127.0.0.1:8687"`) {
		t.Errorf("the document does not say where its requests go: %q", got)
	}
	// Before the bundle, or the bundle has already read an empty dataset.
	if strings.Index(got, "d.desktop") > strings.Index(got, "</head>") {
		t.Error("the script has to run before the bundle loads")
	}
	if strings.Contains(got, "location.hash") {
		t.Error("the main window is not sent anywhere")
	}
}

// TestAPoppedOutWindowIsToldWhereItIs keeps the other half: a webview is
// handed a URL at start-up with no way to add a fragment afterwards.
func TestAPoppedOutWindowIsToldWhereItIs(t *testing.T) {
	t.Parallel()
	got := documentServed(t, "/sessions/abc", "127.0.0.1:8687")

	if !strings.Contains(got, `location.hash="#/sessions/abc"`) {
		t.Errorf("the window was not sent to its session: %q", got)
	}
	if !strings.Contains(got, `d.desktop="true"`) {
		t.Error("a popped-out window is frameless too")
	}
}
