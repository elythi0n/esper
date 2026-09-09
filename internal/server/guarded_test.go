package server

import (
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"regexp"
	"strings"
	"testing"
)

/*
Every route this server has refuses a request with no token.

The token is the whole of the API's authentication: it is a header, which a
form cannot set and a page on another origin cannot read, and it is the only
thing between a local port and this machine's device list, its identity, its
pairing and whether it serves its screen at all.

Adding a route is one line. Forgetting `s.guard` around the handler is one
word, it looks exactly like the line above it, and nothing else would notice —
the route works, the window can call it, and the tests for what it does all
pass because they send a token.

So this asks the **running mux**, not the source: the patterns come from the
registration so that a new route is picked up automatically, and then every one
of them is called with no token and has to answer 401.
*/
func TestEveryRouteRefusesARequestWithNoToken(t *testing.T) {
	routes := everyRoute(t)

	engine, _ := anEngineThat(t, 0, "")
	s, _, store := serverThatCanWrite(t, engine)
	fingerprint := theFingerprint(t, store)

	for _, route := range routes {
		method, path := route[1], route[2]
		// The one wildcard this server has, filled with a real device so a
		// refusal cannot be mistaken for "no such device".
		path = strings.ReplaceAll(path, "{fingerprint}", fingerprint)

		t.Run(method+" "+path, func(t *testing.T) {
			w := askWith(t, s, method, path, Token{}, `{}`)
			if w.Code != http.StatusUnauthorized {
				t.Errorf(
					"answered %d without a token — this route is reachable by anything "+
						"that can open a socket to this port",
					w.Code,
				)
			}
		})
	}
}

// everyRoute is the patterns this server registers, read from the source.
//
// From the source rather than from a list kept beside it, because a list is a
// thing somebody forgets to add a route to — and a route nobody remembered is
// exactly the one that is not guarded.
func everyRoute(t *testing.T) [][]string {
	t.Helper()
	source, err := os.ReadFile("server.go")
	if err != nil {
		t.Fatalf("reading the routes: %v", err)
	}
	pattern := regexp.MustCompile(`mux\.HandleFunc\("([A-Z]+) ([^"]+)"`)
	routes := pattern.FindAllStringSubmatch(string(source), -1)
	// A floor: a parse that found nothing agrees with everything.
	if len(routes) < 20 {
		t.Fatalf("found %d route(s) to try, which is not this server", len(routes))
	}
	return routes
}

/*
And every route refuses a request that arrived under a *name*, token and all.

A page in a browser can send requests to 127.0.0.1, and DNS rebinding lets it
do so under a hostname it controls — the name resolves to loopback, the
browser treats the origin as its own, and the page reads the reply. Only the
literal address this server bound is accepted.

`server_test.go` checks that on `/status`. This asks the same of every route
the server registers, because the rule is worth nothing if it holds on the one
route somebody thought to test and not on the next one added.
*/
func TestEveryRouteRefusesAHostnameEvenWithTheToken(t *testing.T) {
	routes := everyRoute(t)

	engine, _ := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fingerprint := theFingerprint(t, store)

	for _, route := range routes {
		method, path := route[1], route[2]
		path = strings.ReplaceAll(path, "{fingerprint}", fingerprint)

		t.Run(method+" "+path, func(t *testing.T) {
			w := httptest.NewRecorder()
			r := httptest.NewRequest(method, "http://127.0.0.1:8687"+path, strings.NewReader(`{}`))
			r.Header.Set("Authorization", "Bearer "+token.String())
			r.Header.Set("Content-Type", "application/json")
			// A name an attacker controls, resolving to loopback.
			r.Host = "esper.example.com:8687"
			// **On a context that is already over.** The check being made is
			// about the guard, and the guard answers before any handler runs.
			// Without this, removing the guard does not make this test *fail*
			// — it makes it hang, because the requests then reach handlers
			// that wait on an engine, and a gate that times out instead of
			// failing is a gate nobody can read.
			over, stop := context.WithCancel(context.Background())
			stop()
			s.http.Handler.ServeHTTP(w, r.WithContext(over))
			if w.Code != http.StatusNotFound {
				t.Errorf(
					"answered %d for a request addressed to a name — a page on a site an "+
						"attacker controls could reach this route",
					w.Code,
				)
			}
		})
	}
}
