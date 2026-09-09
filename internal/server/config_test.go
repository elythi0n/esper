package server

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// aSettingsEngine stands in for the real one: it prints the registry for a
// read and records its argv for a write.
func aSettingsEngine(t *testing.T) (path, log string) {
	t.Helper()
	dir := t.TempDir()
	path = filepath.Join(dir, "engine")
	log = filepath.Join(dir, "args")
	registry := `{"file":"/tmp/config.toml","present":true,"settings":[` +
		`{"key":"host.fps","about":"The most frames a second to send.","shape":"a whole number, 1 to 240",` +
		`"kind":"whole","source":"default","value":30,"raw":"30","default":30,"least":1,"most":240},` +
		`{"key":"host.serve_without_indicator","about":"Serve without showing it.","shape":"true or false",` +
		`"kind":"flag","source":"default","value":false,"raw":"false","default":false}` +
		`]}`
	script := "#!/bin/sh\n" +
		"printf '%s\\n' \"$@\" > " + log + "\n" +
		"case \"$2\" in --json) echo '" + registry + "' ;; esac\n"
	if err := os.WriteFile(path, []byte(script), 0o700); err != nil {
		t.Fatalf("writing the stand-in engine: %v", err)
	}
	return path, log
}

/*
The whole registry reaches the caller, with the bounds and the source.

A settings screen draws a control per setting and what control depends on the
kind — so an answer carrying only values would make the page declare the kinds
and the bounds a second time. A bound tightened in the engine and not in the
page is a screen offering a value the file will not take.
*/
func TestTheSettingsAnswerCarriesEnoughToDrawAScreen(t *testing.T) {
	engine, _ := aSettingsEngine(t)
	s, token, _ := serverThatCanWrite(t, engine)

	w := askWith(t, s, http.MethodGet, "/config", token, "")
	if w.Code != http.StatusOK {
		t.Fatalf("reading the settings answered %d: %s", w.Code, w.Body.String())
	}
	var answer struct {
		File     *string `json:"file"`
		Present  bool    `json:"present"`
		Settings []struct {
			Key          string `json:"key"`
			Kind         string `json:"kind"`
			Shape        string `json:"shape"`
			About        string `json:"about"`
			Source       string `json:"source"`
			Raw          string `json:"raw"`
			Least        *uint64
			Most         *uint64
			AtTheMachine bool   `json:"at_the_machine"`
			WhyAtMachine string `json:"why_at_the_machine"`
		} `json:"settings"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &answer); err != nil {
		t.Fatalf("the answer is not the registry: %v — %s", err, w.Body.String())
	}
	if len(answer.Settings) != 2 {
		t.Fatalf("the answer carries %d settings", len(answer.Settings))
	}
	first := answer.Settings[0]
	for what, got := range map[string]string{
		"kind": first.Kind, "shape": first.Shape, "about": first.About,
		"source": first.Source, "raw": first.Raw,
	} {
		if got == "" {
			t.Errorf("host.fps came back with no %s, so a screen cannot draw it", what)
		}
	}
	if first.Least == nil || first.Most == nil {
		t.Error("a number came back with no bounds, so a screen would offer a value the file refuses")
	}

	// **And which of them a request may not change, with the reason.** The
	// screen has to say so rather than offering a control that always fails.
	second := answer.Settings[1]
	if !second.AtTheMachine {
		t.Fatal("host.serve_without_indicator is offered as changeable from here")
	}
	if second.WhyAtMachine == "" {
		t.Fatal("it is refused with no reason, which is a refusal nobody can act on")
	}
	if first.AtTheMachine {
		t.Fatal("host.fps needs somebody at the machine, which it should not")
	}
}

/*
A change reaches the engine, and the ones for the machine do not.

The refusal is a 403 rather than a 400: the request is well formed and
understood, and what it lacks is somebody standing at the machine. A client
told "bad request" would try to fix the request.
*/
func TestASettingIsCheckedBeforeItReachesTheEngine(t *testing.T) {
	engine, log := aSettingsEngine(t)
	s, token, _ := serverThatCanWrite(t, engine)

	w := askWith(t, s, http.MethodPost, "/config", token, `{"key":"host.fps","value":"45"}`)
	if w.Code != http.StatusOK {
		t.Fatalf("an ordinary setting answered %d: %s", w.Code, w.Body.String())
	}
	said, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine was never asked: %v", err)
	}
	// The last thing it was asked is the read-back; the write is what matters
	// and is checked in `internal/settings`. Here: it ran, and it was config.
	if !strings.Contains(string(said), "config") {
		t.Fatalf("the engine was asked %q", said)
	}

	if err := os.Remove(log); err != nil {
		t.Fatalf("clearing the record: %v", err)
	}
	w = askWith(t, s, http.MethodPost, "/config", token,
		`{"key":"host.serve_without_indicator","value":"true"}`)
	if w.Code != http.StatusForbidden {
		t.Fatalf("a setting for the machine answered %d: %s", w.Code, w.Body.String())
	}
	if !strings.Contains(w.Body.String(), "esper config set") {
		t.Fatalf("the refusal does not name the command: %s", w.Body.String())
	}
	if _, err := os.Stat(log); err == nil {
		t.Error("a setting the server refused was handed to the engine anyway")
	}

	/*
		**And taking one out is refused too.**

		Unsetting `host.serve_without_indicator` puts it back to its default,
		which is `false` — so this particular one is harmless. The next one
		added to the list will not be, and a boundary enforced on one verb and
		not the other is the shape of that mistake. Both go through the same
		check; this is what says so.
	*/
	w = askWith(t, s, http.MethodPost, "/config", token,
		`{"key":"host.serve_without_indicator","unset":true}`)
	if w.Code != http.StatusForbidden {
		t.Fatalf("unsetting a setting for the machine answered %d: %s", w.Code, w.Body.String())
	}
	if _, err := os.Stat(log); err == nil {
		t.Error("an unset the server refused was handed to the engine anyway")
	}
}

// And a body this build does not understand is the request's fault.
func TestASettingsBodyIsReadStrictly(t *testing.T) {
	engine, log := aSettingsEngine(t)
	s, token, _ := serverThatCanWrite(t, engine)

	for _, bad := range []struct {
		what string
		body string
	}{
		{"a field this build does not know", `{"setting":"host.fps","value":"45"}`},
		{"an unknown field beside a known one", `{"key":"host.fps","value":"45","setting":"x"}`},
		{"a key that is a flag", `{"key":"--json","value":"1"}`},
		{"a key with a shell in it", `{"key":"host.fps; rm -rf /","value":"1"}`},
		{"no key at all", `{"key":"","value":"45"}`},
		{"not an object", `"host.fps"`},
	} {
		w := askWith(t, s, http.MethodPost, "/config", token, bad.body)
		if w.Code == http.StatusOK {
			t.Fatalf("%s (%s) was accepted", bad.what, bad.body)
		}
		if w.Code >= 500 && w.Code != http.StatusBadGateway {
			t.Fatalf("%s answered %d, which blames the server for the request", bad.what, w.Code)
		}
	}
	if _, err := os.Stat(log); err == nil {
		t.Error("a body the server refused was handed to the engine anyway")
	}
}

// Taking a setting out of the file is its own request, because "" is a real
// value for the settings whose kind is text.
func TestUnsettingIsAFieldAndNotAnEmptyValue(t *testing.T) {
	engine, log := aSettingsEngine(t)
	s, token, _ := serverThatCanWrite(t, engine)

	w := askWith(t, s, http.MethodPost, "/config", token, `{"key":"device.name","unset":true}`)
	if w.Code != http.StatusOK {
		t.Fatalf("unsetting answered %d: %s", w.Code, w.Body.String())
	}
	// The record holds the read-back; what matters is that `unset` was the
	// verb, which the engine would have recorded first.
	said, err := os.ReadFile(log)
	if err != nil {
		t.Fatalf("the engine was never asked: %v", err)
	}
	if strings.Contains(string(said), "set\ndevice.name\n\n") {
		t.Fatalf("unsetting was sent as an empty value: %q", said)
	}
}
