package server

import (
	"net/http"
	"os"
	"strings"
	"testing"
)

/*
Asking to turn the host on must never turn it off.

`POST /host` read `{"on": bool}` into a plain `bool`, so a body that did not
set it left the value `false` and the route ran `systemctl --user disable`. An
empty object did it. So did `{"start": true}` from somebody who guessed the
name, and so would a field a later window sends. The answer was 200, with a
report of what had been done.

**Every case below is refused before anything reaches the service manager**,
which is the only reason this file can exist: `internal/service`'s own tests
say plainly that a suite which enables or disables the host on a developer's
machine is worse than no suite, and they are right. Nothing here sends a body
that would be acted on.
*/
func TestABodyThatDoesNotSayIsRefusedRatherThanActedOn(t *testing.T) {
	engine, _ := anEngineThat(t, 0, "")
	s, token, _ := serverThatCanWrite(t, engine)

	for _, c := range []struct {
		name string
		body string
	}{
		{"nothing at all", `{}`},
		{"a field this does not know", `{"start":true}`},
		{"the field, misspelled", `{"onn":true}`},
		{"the right field and an unknown one", `{"on":true,"force":true}`},
		{"a value that is not a yes or a no", `{"on":"yes"}`},
		{"not an object", `[]`},
		{"nothing at all, literally", ``},
	} {
		t.Run(c.name, func(t *testing.T) {
			w := askWith(t, s, http.MethodPost, "/host", token, c.body)
			if w.Code != http.StatusBadRequest {
				t.Fatalf(
					"answered %d for %s — and anything but a refusal here means the service "+
						"manager was reached with a request nobody made: %s",
					w.Code, c.body, w.Body.String(),
				)
			}
			// The refusal says what to send, because a 400 with no shape in it
			// sends somebody to the source.
			if !strings.Contains(w.Body.String(), `"on"`) {
				t.Errorf("the refusal does not say what the body should be: %s", w.Body.String())
			}
		})
	}
}

/*
And a permissions change with a field the server does not know.

`{"permissons": [...]}` decoded to an empty list, the engine refused with
"needs at least one permission", and the caller was told the *engine* had a
problem — with a body the server had not read properly. It is a 400 now, which
is where the fault was.
*/
func TestAMisspeltFieldIsTheRequestsFaultNotTheEngines(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/grant", token, `{"permissons":["control"]}`)
	if w.Code != http.StatusBadRequest {
		t.Errorf("a body with a misspelt field answered %d: %s", w.Code, w.Body.String())
	}
	// And it never reached the engine, which is the other half: a request the
	// server could not read is not one to pass on.
	if _, err := os.Stat(log); err == nil {
		t.Error("a body the server could not read was handed to the engine anyway")
	}
}
