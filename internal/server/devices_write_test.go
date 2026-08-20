package server

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/elythi0n/esper/internal/devices"
)

const elythion = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"

// anEngineThat writes a stand-in `esper-engine` that records its arguments and
// exits how it is told.
//
// A script rather than a mock, because what is being checked is the *argv* the
// API builds — and the only honest way to see that is to be the program it
// runs.
func anEngineThat(t *testing.T, exit int, says string) (path, log string) {
	t.Helper()
	dir := t.TempDir()
	path = filepath.Join(dir, "engine")
	log = filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > " + log + "\n"
	if says != "" {
		script += "echo '" + says + "' >&2\n"
	}
	script += "exit " + itoa(exit) + "\n"
	if err := os.WriteFile(path, []byte(script), 0o700); err != nil {
		t.Fatalf("writing the stand-in engine: %v", err)
	}
	return path, log
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	return string(rune('0' + n))
}

func serverThatCanWrite(t *testing.T, engine string) (*Server, Token, string) {
	t.Helper()
	token, err := LoadOrCreateToken(t.TempDir())
	if err != nil {
		t.Fatalf("a token: %v", err)
	}
	store := filepath.Join(t.TempDir(), "devices.json")
	paired := `{"version":1,"devices":{"` + elythion + `":{"public_key":"` + elythion + `",` +
		`"name":"elythion","permissions":["view"],"paired_at":1,"last_seen":null,"pairing":"kept"}}}`
	if err := os.WriteFile(store, []byte(paired), 0o600); err != nil {
		t.Fatalf("writing the store: %v", err)
	}
	s, err := New(Options{
		Addr: "127.0.0.1:8687", Token: token, Store: store,
		EngineBinary: engine, Engine: &nothingRunning{},
	})
	if err != nil {
		t.Fatalf("a server: %v", err)
	}
	return s, token, store
}

// theFingerprint is elythion's, which the read tests already pin.
func theFingerprint(t *testing.T, store string) string {
	t.Helper()
	list, err := devices.Load(store)
	if err != nil || len(list) != 1 {
		t.Fatalf("the store should hold one device: %v", err)
	}
	return list[0].Fingerprint().Hex
}

func post(t *testing.T, s *Server, path string, token Token, body string) *http.Response {
	t.Helper()
	w := askWith(t, s, http.MethodPost, path, token, body)
	return w.Result()
}

// TestAGrantReachesTheEngineAsItsOwnCommand is the whole point: the API does
// not write the store, it asks the one program that does.
func TestAGrantReachesTheEngineAsItsOwnCommand(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	resp := post(t, s, "/devices/"+fp+"/grant", token, `{"permissions":["control","audio"]}`)
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusConflict {
		t.Fatalf("expected the change to be attempted, got %d", resp.StatusCode)
	}
	args, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine was not run: %v", err)
	}
	want := "grant\n" + fp + "\ncontrol\naudio\n"
	if string(args) != want {
		t.Fatalf("the engine got %q, want %q", args, want)
	}
}

// TestASensitiveGrantNeverReachesTheEngine is the security rule. `unattended`
// removes the question a session is held up by, and the host prompt is why it
// cannot be switched on silently — so a request from the network is refused
// here rather than answered with `--yes` on somebody's behalf.
func TestASensitiveGrantNeverReachesTheEngine(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	resp := post(t, s, "/devices/"+fp+"/grant", token, `{"permissions":["unattended"]}`)
	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("expected 403, got %d", resp.StatusCode)
	}
	if _, err := os.Stat(log); err == nil {
		t.Fatal("the engine was run for a permission that needs somebody at the machine")
	}
}

// TestOneSensitivePermissionStopsTheWholeGrant: a partial change would leave
// the caller working out which half happened.
func TestOneSensitivePermissionStopsTheWholeGrant(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	resp := post(t, s, "/devices/"+fp+"/grant", token, `{"permissions":["control","reboot"]}`)
	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("expected 403, got %d", resp.StatusCode)
	}
	if _, err := os.Stat(log); err == nil {
		t.Fatal("nothing should have been granted")
	}
}

// TestASensitivePermissionIsStillRevocable — taking access away needs no
// ceremony, and a revocation that could be refused is one somebody could be
// talked out of.
func TestASensitivePermissionIsStillRevocable(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	resp := post(t, s, "/devices/"+fp+"/revoke", token, `{"permissions":["unattended"]}`)
	if resp.StatusCode == http.StatusForbidden {
		t.Fatal("revoking must never be refused for being sensitive")
	}
	args, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine should have been asked: %v", err)
	}
	if !strings.HasPrefix(string(args), "revoke\n"+fp+"\nunattended") {
		t.Fatalf("the engine got %q", args)
	}
}

// TestAPrefixIsNotADevice — the engine accepts one and refuses an ambiguous
// one, which is right for a person typing; a prefix that is unambiguous today
// can match a second device tomorrow.
func TestAPrefixIsNotADevice(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	resp := post(t, s, "/devices/"+fp[:8]+"/grant", token, `{"permissions":["control"]}`)
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("expected 404 for a prefix, got %d", resp.StatusCode)
	}
	if _, err := os.Stat(log); err == nil {
		t.Fatal("a prefix should not have reached the engine")
	}
}

// TestWhatTheEngineSaidIsWhatTheCallerGets — "exit status 1" tells nobody
// anything; the engine's own words say what to do next.
func TestWhatTheEngineSaidIsWhatTheCallerGets(t *testing.T) {
	engine, _ := anEngineThat(t, 1, "no paired device starts with that")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	resp := post(t, s, "/devices/"+fp+"/grant", token, `{"permissions":["control"]}`)
	if resp.StatusCode != http.StatusBadGateway {
		t.Fatalf("expected 502, got %d", resp.StatusCode)
	}
	body := make([]byte, 512)
	n, _ := resp.Body.Read(body)
	if !strings.Contains(string(body[:n]), "no paired device") {
		t.Fatalf("the engine's own words should reach the caller, got %q", body[:n])
	}
}

// TestForgettingTakesNoBody and reaches the engine as `forget`.
func TestForgettingTakesNoBody(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	w := askWith(t, s, http.MethodDelete, "/devices/"+fp, token, "")
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d: %s", w.Code, w.Body.String())
	}
	args, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine was not run: %v", err)
	}
	if string(args) != "forget\n"+fp+"\n" {
		t.Fatalf("the engine got %q", args)
	}
}

// TestAChangeStillNeedsTheToken — a state-changing request is guarded by
// exactly what the reads are, and that is deliberate: the token is a header,
// which a form cannot set and a page cannot read.
func TestAChangeStillNeedsTheToken(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, _, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	for _, call := range []struct{ method, path string }{
		{http.MethodPost, "/devices/" + fp + "/grant"},
		{http.MethodPost, "/devices/" + fp + "/revoke"},
		{http.MethodDelete, "/devices/" + fp},
	} {
		w := askWith(t, s, call.method, call.path, Token{}, `{"permissions":["view"]}`)
		if w.Code != http.StatusUnauthorized {
			t.Errorf("%s %s answered %d without a token", call.method, call.path, w.Code)
		}
	}
	if _, err := os.Stat(log); err == nil {
		t.Fatal("an unauthenticated request reached the engine")
	}
}

// TestTheAnswerIsReadBackFromTheStore — a caller told "granted: control" by
// the thing it just asked has learnt nothing.
func TestTheAnswerIsReadBackFromTheStore(t *testing.T) {
	// The stand-in engine changes nothing, so the store still says `view`
	// alone — and that is what the answer must show.
	engine, _ := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	resp := post(t, s, "/devices/"+fp+"/grant", token, `{"permissions":["control"]}`)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}
	var got struct {
		Permissions []string `json:"permissions"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatalf("parsing: %v", err)
	}
	if len(got.Permissions) != 1 || got.Permissions[0] != "view" {
		t.Fatalf("the answer should be what the store says, not what was asked: %v",
			got.Permissions)
	}
}

/*
Keeping a one-session pairing, which is the route the screen could not reach.

The Devices screen has shown `once` and `spent` since it was written and could
do nothing about either: the only way to make a pairing last was `esper keep`
in a terminal, which is the thing this window exists to stop being necessary.
It is the newest of the four changes and the only one nothing held.

Like forgetting, it takes no body — there is nothing to say about it beyond
which device — and like all four, it is guarded by the token.
*/
func TestKeepingAPairingReachesTheEngineAndNeedsTheToken(t *testing.T) {
	engine, log := anEngineThat(t, 0, "")
	s, token, store := serverThatCanWrite(t, engine)
	fp := theFingerprint(t, store)

	// A body is neither required nor a reason to refuse: a client that sends
	// one is asking for the same thing.
	w := askWith(t, s, http.MethodPost, "/devices/"+fp+"/keep", token, "")
	if w.Code != http.StatusOK {
		t.Fatalf("keeping answered %d: %s", w.Code, w.Body.String())
	}

	args, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine was not run: %v", err)
	}
	if string(args) != "keep\n"+fp+"\n" {
		t.Fatalf("the engine got %q, and the command is `keep <fingerprint>`", args)
	}

	// The answer is the store's own account of the device, not an echo of the
	// request — which is the only way a change that did not take is visible.
	var said struct {
		Name        string   `json:"name"`
		Permissions []string `json:"permissions"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &said); err != nil {
		t.Fatalf("the answer is not a device: %v — %s", err, w.Body.String())
	}
	if said.Name == "" {
		t.Errorf("keeping answered with no device: %s", w.Body.String())
	}

	if err := os.Remove(log); err != nil {
		t.Fatalf("clearing the engine's log: %v", err)
	}
	unauthorised := askWith(t, s, http.MethodPost, "/devices/"+fp+"/keep", Token{}, "")
	if unauthorised.Code != http.StatusUnauthorized {
		t.Errorf("keeping answered %d without a token", unauthorised.Code)
	}
	if _, err := os.Stat(log); err == nil {
		t.Fatal("an unauthenticated keep reached the engine")
	}
}
