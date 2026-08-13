package server

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/elythi0n/esper/internal/devices"
	"github.com/elythi0n/esper/internal/proto"
)

// nothingRunning is an engine that is not there — which is the ordinary state
// of a machine nobody is connected to, and the state the device list has to
// work in.
type nothingRunning struct{ asked int }

func (n *nothingRunning) Status(context.Context) (*proto.Status, error) {
	n.asked++
	return nil, errNotRunning
}

func (n *nothingRunning) Sessions(context.Context, uint32) ([]proto.Happening, error) {
	n.asked++
	return nil, errNotRunning
}

func (n *nothingRunning) Running() bool { return false }

var errNotRunning = errors.New("the engine is not running")

// serverOverStore builds an API whose device store is the given file, with no
// engine behind it.
func serverOverStore(t *testing.T, store string) (*Server, Token, *nothingRunning) {
	t.Helper()
	token, err := LoadOrCreateToken(t.TempDir())
	if err != nil {
		t.Fatalf("a token: %v", err)
	}
	engine := &nothingRunning{}
	s, err := New(Options{Addr: "127.0.0.1:8687", Token: token, Store: store, Engine: engine})
	if err != nil {
		t.Fatalf("a server: %v", err)
	}
	return s, token, engine
}

// withToken presents the token, which every request has to.
func withToken(token Token) func(*http.Request) {
	return func(r *http.Request) { r.Header.Set("Authorization", "Bearer "+token.String()) }
}

// TestTheDeviceListIsAnsweredWithNoEngineRunning is the whole point of reading
// the store here. A machine with nothing connected has no engine, and that is
// exactly when somebody opens the device list.
func TestTheDeviceListIsAnsweredWithNoEngineRunning(t *testing.T) {
	fixture, err := os.ReadFile(filepath.Join("..", "..", "testdata", "fixtures", "device_store.json"))
	if err != nil {
		t.Fatalf("the fixture is committed: %v", err)
	}
	store := filepath.Join(t.TempDir(), "devices.json")
	if err := os.WriteFile(store, fixture, 0o600); err != nil {
		t.Fatalf("writing the store: %v", err)
	}

	s, token, engine := serverOverStore(t, store)
	w := ask(t, s, "/devices", withToken(token))
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 with no engine running, got %d: %s", w.Code, w.Body.String())
	}
	if engine.asked != 0 {
		t.Errorf("the device list must not need the engine; it was asked %d times", engine.asked)
	}

	var body struct {
		Devices []struct {
			Name           string              `json:"name"`
			PublicKey      string              `json:"public_key"`
			Permissions    []string            `json:"permissions"`
			Fingerprint    devices.Fingerprint `json:"fingerprint"`
			AdmitsASession bool                `json:"admits_a_session"`
		} `json:"devices"`
		ReadFrom string `json:"read_from"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &body); err != nil {
		t.Fatalf("parsing the answer: %v", err)
	}
	if len(body.Devices) != 3 {
		t.Fatalf("the fixture holds 3 devices, the API reported %d", len(body.Devices))
	}
	if body.ReadFrom != store {
		t.Errorf("the answer should say which file it came from, got %q", body.ReadFrom)
	}
	for _, device := range body.Devices {
		if len(device.Fingerprint.Words) != devices.FingerprintWords {
			t.Errorf("%s should carry %d fingerprint words, got %d",
				device.Name, devices.FingerprintWords, len(device.Fingerprint.Words))
		}
		if device.Fingerprint.Hex == "" {
			t.Errorf("%s has no fingerprint", device.Name)
		}
		if device.PublicKey == "" {
			t.Errorf("%s has no key", device.Name)
		}
	}
}

// TestAMachineWithNoStoreListsNoDevicesRatherThanFailing is the first-run case:
// nothing has been paired, and that is not a fault.
func TestAMachineWithNoStoreListsNoDevicesRatherThanFailing(t *testing.T) {
	s, token, _ := serverOverStore(t, filepath.Join(t.TempDir(), "devices.json"))
	w := ask(t, s, "/devices", withToken(token))
	if w.Code != http.StatusOK {
		t.Fatalf("a machine that has never paired is not a failure, got %d", w.Code)
	}
	var body struct {
		Devices []json.RawMessage `json:"devices"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &body); err != nil {
		t.Fatalf("parsing: %v", err)
	}
	if len(body.Devices) != 0 {
		t.Fatalf("expected no devices, got %d", len(body.Devices))
	}
}

// TestAStoreThatCannotBeReadIsNotReportedAsEmpty is the other half. The two
// answers must never look the same: a device list that looks empty invites
// somebody to pair everything again rather than to look at the broken file.
func TestAStoreThatCannotBeReadIsNotReportedAsEmpty(t *testing.T) {
	store := filepath.Join(t.TempDir(), "devices.json")
	if err := os.WriteFile(store, []byte(`{"version": 99, "devices": {}}`), 0o600); err != nil {
		t.Fatalf("writing the store: %v", err)
	}
	s, token, _ := serverOverStore(t, store)
	w := ask(t, s, "/devices", withToken(token))
	if w.Code == http.StatusOK {
		t.Fatalf("a store this build cannot read must not answer 200: %s", w.Body.String())
	}
	said := w.Body.String()
	if !strings.Contains(said, store) || !strings.Contains(said, "different version") {
		t.Errorf("the refusal should name the file and what is wrong with it, got %q", said)
	}
}

// TestTheDeviceListStillNeedsTheToken: the store is not a secret, but who this
// machine trusts and what each may do is a map of the way in.
func TestTheDeviceListStillNeedsTheToken(t *testing.T) {
	s, _, _ := serverOverStore(t, filepath.Join(t.TempDir(), "devices.json"))
	w := ask(t, s, "/devices", nil)
	if w.Code != http.StatusUnauthorized {
		t.Fatalf("expected the device list to be guarded, got %d", w.Code)
	}
}

// TestTheDeviceListIsReReadOnEveryRequest: the engine re-reads the same file
// while it runs, which is what makes a revocation reach a session already in
// progress. An API holding a copy from start-up would be the one place left
// that still believed a device was trusted.
func TestTheDeviceListIsReReadOnEveryRequest(t *testing.T) {
	const key = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
	store := filepath.Join(t.TempDir(), "devices.json")
	paired := `{"version":1,"devices":{"` + key + `":{"public_key":"` + key + `",` +
		`"name":"elythion","permissions":["view","control"],"paired_at":1,` +
		`"last_seen":null,"pairing":"kept"}}}`
	if err := os.WriteFile(store, []byte(paired), 0o600); err != nil {
		t.Fatalf("writing the store: %v", err)
	}
	s, token, _ := serverOverStore(t, store)

	count := func() int {
		t.Helper()
		w := ask(t, s, "/devices", withToken(token))
		if w.Code != http.StatusOK {
			t.Fatalf("expected 200, got %d: %s", w.Code, w.Body.String())
		}
		var body struct {
			Devices []json.RawMessage `json:"devices"`
		}
		if err := json.Unmarshal(w.Body.Bytes(), &body); err != nil {
			t.Fatalf("parsing: %v", err)
		}
		return len(body.Devices)
	}

	if got := count(); got != 1 {
		t.Fatalf("expected the paired device, got %d", got)
	}
	if err := os.WriteFile(store, []byte(`{"version":1,"devices":{}}`), 0o600); err != nil {
		t.Fatalf("revoking: %v", err)
	}
	if got := count(); got != 0 {
		t.Fatalf("a revocation should show at once, still got %d", got)
	}
}
