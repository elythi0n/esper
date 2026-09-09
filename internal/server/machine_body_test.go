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

/*
A schedule arriving over the network, and what may not.

The route exists because the engine has had `schedule` since Phase 14 and
nothing reached it — so a device's hours could be read in the app and changed
only in a terminal. What arrives is the engine's own vocabulary, and what this
checks is that a body which is not that is refused here rather than handed on.
*/
func TestAScheduleIsCheckedBeforeItReachesTheEngine(t *testing.T) {
	engine, _ := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	for _, bad := range []struct {
		what string
		body string
	}{
		{"a field this build does not know", `{"schedule":"always"}`},
		// Beside a good when, so the refusal is about the field and not about
		// a when that decoded to nothing — which a server ignoring unknown
		// fields would also produce.
		{"an unknown field beside a known one", `{"when":"always","schedule":"never"}`},
		{"a when that is a flag", `{"when":"--yes"}`},
		{"a when with a shell in it", `{"when":"always; rm -rf /"}`},
		{"no when at all", `{"when":""}`},
		{"not an object", `"always"`},
	} {
		w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/schedule", token, bad.body)
		if w.Code == http.StatusOK {
			t.Fatalf("%s (%s) was accepted", bad.what, bad.body)
		}
		if w.Code >= 500 && w.Code != http.StatusBadGateway {
			t.Fatalf("%s answered %d, which blames the server for the request", bad.what, w.Code)
		}
	}
}

/*
A lock-after arriving over the network, and what may not.

Same story as the schedule: the engine has had the verb since the setting
existed and nothing reached it, so a device's answer was visible in the panel
and changeable only in a terminal. What is checked here is the half the engine
never sees — a body that is not one of four words is refused before anything
is run.
*/
func TestALockAfterIsCheckedBeforeItReachesTheEngine(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	for _, bad := range []struct {
		what string
		body string
	}{
		{"a field this build does not know", `{"lock_after":"always"}`},
		// Beside a good answer, so the refusal is about the field rather
		// than about an answer that decoded to nothing. Without it a server
		// that ignored unknown fields passes this test.
		{"an unknown field beside a known one", `{"after":"always","lock_after":"never"}`},
		{"an answer that is a flag", `{"after":"--yes"}`},
		{"an answer with a shell in it", `{"after":"always; rm -rf /"}`},
		{"no answer at all", `{"after":""}`},
		{"a word nobody knows", `{"after":"sometimes"}`},
		{"not an object", `"always"`},
	} {
		w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/lock-after", token, bad.body)
		if w.Code == http.StatusOK {
			t.Fatalf("%s (%s) was accepted", bad.what, bad.body)
		}
		if w.Code >= 500 && w.Code != http.StatusBadGateway {
			t.Fatalf("%s answered %d, which blames the server for the request", bad.what, w.Code)
		}
	}
	// And none of them reached the engine: a request the server refuses is not
	// one to pass on and find out about.
	if _, err := os.Stat(log); err == nil {
		t.Error("a body the server refused was handed to the engine anyway")
	}

	// The four that are allowed do reach it, or the refusals above are only
	// proof that the route refuses everything.
	for _, good := range []string{"always", "never", "unattended", "default"} {
		w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/lock-after", token, `{"after":"`+good+`"}`)
		if w.Code == http.StatusBadRequest {
			t.Fatalf("%q was refused by the server: %s", good, w.Body.String())
		}
	}
	said, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine was never asked anything: %v", err)
	}
	if !strings.Contains(string(said), "lock-after") {
		t.Fatalf("the engine was asked for something else: %s", said)
	}
}

/*
A wake reaches the engine as the verb and a fingerprint, and nothing else.

The route exists because the engine has had `wake` since Phase 12 and only a
terminal could send one — a device asleep on the network was a device the
window could see and not reach. What is checked here is the shape of what runs:
no destination, whatever the request says.
*/
func TestAWakeReachesTheEngineWithNoDestination(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/wake", token, "")
	if w.Code != http.StatusOK {
		t.Fatalf("a wake answered %d: %s", w.Code, w.Body.String())
	}
	said, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine was never asked to wake anything: %v", err)
	}
	ran := strings.Fields(string(said))
	if len(ran) != 2 || ran[0] != "wake" || ran[1] != fp {
		t.Fatalf("the engine was asked %v, and a wake is the verb and the device", ran)
	}

	// A body naming somewhere to send it changes nothing, because the route
	// reads no body at all. This is the claim that matters: a caller cannot
	// use this machine to put a packet on an address of its choosing.
	if err := os.Remove(log); err != nil {
		t.Fatalf("could not clear the record: %v", err)
	}
	w = askWith(t, s, http.MethodPost, "/devices/"+fp+"/wake", token, `{"to":"10.0.0.1"}`)
	if w.Code != http.StatusOK {
		t.Fatalf("a wake with a body answered %d: %s", w.Code, w.Body.String())
	}
	said, err = os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine was never asked: %v", err)
	}
	if strings.Contains(string(said), "10.0.0.1") || strings.Contains(string(said), "--to") {
		t.Fatalf("an address from the request reached the engine: %s", said)
	}
}

// And a fingerprint that is not one is refused before anything runs.
func TestAWakeNeedsAWholeFingerprint(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, _ := serverThatCanWrite(t, engine)

	w := askWith(t, s, http.MethodPost, "/devices/827aabec/wake", token, "")
	if w.Code != http.StatusNotFound {
		t.Fatalf("a fingerprint prefix answered %d: %s", w.Code, w.Body.String())
	}
	if _, err := os.Stat(log); err == nil {
		t.Error("a fingerprint the server refused was handed to the engine anyway")
	}
}

/*
Each of the three reaches the engine as the verb and a fingerprint.

They are the only routes here that act on *another* machine, so what runs
matters more than usual: `--via` and `--broker-key` are where a request could
name where this machine connects to, and neither is passed.
*/
func TestTheThreeRemoteActionsReachTheEngineBare(t *testing.T) {
	for path, verb := range map[string]string{
		"lock":   "lock",
		"logoff": "logoff",
		"reboot": "reboot",
	} {
		engine, log := anEngineThat(t, 0, "")
		s, token, store := serverThatCanWrite(t, engine)
		fp := theFingerprint(t, store)

		// A body naming somewhere to connect changes nothing, because the
		// route reads no body at all.
		w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/"+path, token, `{"via":"10.0.0.1:1"}`)
		if w.Code != http.StatusOK {
			t.Fatalf("%s answered %d: %s", path, w.Code, w.Body.String())
		}
		said, err := os.ReadFile(log)
		if err != nil {
			t.Fatalf("the engine was never asked to %s: %v", path, err)
		}
		ran := strings.Fields(string(said))
		if len(ran) != 2 || ran[0] != verb || ran[1] != fp {
			t.Fatalf("%s asked the engine %v, and it is the verb and the device", path, ran)
		}
		if strings.Contains(string(said), "10.0.0.1") || strings.Contains(string(said), "--via") {
			t.Fatalf("an address from the request reached the engine: %s", said)
		}
	}
}

/*
And what the far host said about a refusal is what the caller is told.

These reach a machine whose trust store this one does not have a copy of, so
"not allowed" can only come from there — and its sentence names the permission
that is missing, which is the whole of what to do about it.
*/
func TestAFarHostsRefusalIsWhatComesBack(t *testing.T) {
	engine, _ := anEngineThat(t, 1, "the host does not allow it: reboot is not granted")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/reboot", token, "")
	if w.Code != http.StatusBadGateway {
		t.Fatalf("a refused restart answered %d: %s", w.Code, w.Body.String())
	}
	if !strings.Contains(w.Body.String(), "reboot is not granted") {
		t.Fatalf("the host's own reason was lost: %s", w.Body.String())
	}
}

/*
A group change reaches the engine with the group first, and only a name does.

The engine reads `group <name> <fingerprint>...`, so the group is in the
position a flag would occupy — a different trap from every other route here,
where the fingerprint comes first and is checked to be hex.
*/
func TestAGroupIsCheckedBeforeItReachesTheEngine(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/groups", token, `{"name":"ops"}`)
	if w.Code != http.StatusOK {
		t.Fatalf("adding to a group answered %d: %s", w.Code, w.Body.String())
	}
	ran := strings.Fields(func() string {
		said, err := os.ReadFile(log)
		if err != nil {
			t.Fatalf("the engine was never asked: %v", err)
		}
		return string(said)
	}())
	if len(ran) != 3 || ran[0] != "group" || ran[1] != "ops" || ran[2] != fp {
		t.Fatalf("the engine was asked %v, and it is the verb, the group and the device", ran)
	}

	if err := os.Remove(log); err != nil {
		t.Fatalf("clearing the record: %v", err)
	}
	w = askWith(t, s, http.MethodPost, "/devices/"+fp+"/groups", token, `{"name":"ops","remove":true}`)
	if w.Code != http.StatusOK {
		t.Fatalf("removing from a group answered %d: %s", w.Code, w.Body.String())
	}
	said, _ := os.ReadFile(log)
	if !strings.Contains(string(said), "--remove") {
		t.Fatalf("removing did not ask the engine to remove: %q", said)
	}

	if err := os.Remove(log); err != nil {
		t.Fatalf("clearing the record: %v", err)
	}
	for _, bad := range []struct {
		what string
		body string
	}{
		{"a field this build does not know", `{"group":"ops"}`},
		{"an unknown field beside a known one", `{"name":"ops","group":"x"}`},
		{"a name that is a flag", `{"name":"--remove"}`},
		{"a name with a shell in it", `{"name":"ops; rm -rf /"}`},
		{"a name that is two arguments", `{"name":"ops elythion"}`},
		{"no name at all", `{"name":""}`},
		{"not an object", `"ops"`},
	} {
		w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/groups", token, bad.body)
		if w.Code == http.StatusOK {
			t.Fatalf("%s (%s) was accepted", bad.what, bad.body)
		}
		if w.Code >= 500 && w.Code != http.StatusBadGateway {
			t.Fatalf("%s answered %d, which blames the server for the request", bad.what, w.Code)
		}
	}
	if _, err := os.Stat(log); err == nil {
		t.Error("a group name the server refused was handed to the engine anyway")
	}
}
