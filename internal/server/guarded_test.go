package server

import (
	"net/http"
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
