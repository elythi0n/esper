package settings

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

/*
A key from a request is a key, and never a flag or a command.

The key reaches an argv, and the position it lands in is where the engine
reads a subcommand's argument — so a key that began with `-` would be a
*flag* rather than a name, and one carrying a shell's punctuation would be a
value somebody hoped a shell would read. There is no shell here, which is the
first answer; this is the second, because relying on one answer to a question
about argv is how the first version of the device store's writer was written.
*/
func TestOnlyASettingShapedKeyReachesTheEngine(t *testing.T) {
	for _, good := range []string{
		"host.fps",
		"device.name",
		"connectivity.overlay_interfaces",
		"session.idle_timeout",
	} {
		if err := allowed(good); err != nil {
			t.Fatalf("%q was refused: %v", good, err)
		}
	}

	for _, bad := range []struct {
		what string
		key  string
	}{
		{"empty", ""},
		{"a flag", "--json"},
		{"a flag with no dashes of its own", "-f"},
		{"shell metacharacters", "host.fps; rm -rf /"},
		{"a pipe", "host.fps|sh"},
		{"a newline and a second argument", "host.fps\nunset host.listen"},
		{"a nul", "host.fps\x00"},
		{"backticks", "`id`"},
		{"a dollar", "$(id)"},
		{"uppercase, which no setting uses", "HOST.FPS"},
		{"a path", "../../etc/passwd"},
		{"longer than any setting's name", strings.Repeat("a", MostKeyChars+1)},
	} {
		err := allowed(bad.key)
		if err == nil {
			t.Fatalf("%s (%q) was accepted onto a command line", bad.what, bad.key)
		}
		if !errors.Is(err, ErrNoSuchSetting) {
			t.Fatalf("%s was refused for the wrong reason: %v", bad.what, err)
		}
	}
}

/*
The settings a request may not change, and the reason each one gives.

This is the boundary, so it is worth stating in a test rather than only in a
map: each of these either takes away the ability of whoever is at the machine
to know what is happening to it, or widens what a paired device may reach
without a grant. A change to this list should be a change somebody argued for.
*/
func TestTheSettingsThatNeedSomebodyAtTheMachine(t *testing.T) {
	want := []string{
		"host.serve_without_indicator",
		"audit.retain",
		"audit.max_size",
		"files.root",
		"forward.allow",
		"broker.key",
	}
	for _, key := range want {
		why, only := HostOnly(key)
		if !only {
			t.Fatalf("%s may be changed over the control plane", key)
		}
		if why == "" {
			t.Fatalf("%s is refused with no reason, which is a refusal nobody can act on", key)
		}
		err := allowed(key)
		if !errors.Is(err, ErrHostOnly) {
			t.Fatalf("%s was refused as %v, not as a setting for the machine", key, err)
		}
		// The refusal names the command, because that is the whole of what to
		// do about it.
		if !strings.Contains(err.Error(), "esper config set "+key) {
			t.Fatalf("the refusal for %s does not name the command: %v", key, err)
		}
		if !strings.Contains(err.Error(), why) {
			t.Fatalf("the refusal for %s does not say why: %v", key, err)
		}
	}
	if len(hostOnly) != len(want) {
		t.Fatalf("the list holds %d settings and this names %d — one of them changed",
			len(hostOnly), len(want))
	}

	// And an ordinary setting is not in it, or the list would be a way of
	// refusing everything.
	for _, ordinary := range []string{"host.fps", "device.name", "discovery.mdns"} {
		if _, only := HostOnly(ordinary); only {
			t.Fatalf("%s needs somebody at the machine, which it should not", ordinary)
		}
		if err := allowed(ordinary); err != nil {
			t.Fatalf("%s was refused: %v", ordinary, err)
		}
	}
}

// A writer with no engine, or no home, says so rather than guessing.
func TestAWriterWithNothingToAskSaysSo(t *testing.T) {
	var none Writer
	if _, err := none.Read(context.Background()); err == nil {
		t.Fatal("a writer with no engine answered")
	}
	only := Writer{Engine: "/nonexistent/esper-engine"}
	if err := only.Set(context.Background(), "host.fps", "45"); err == nil {
		t.Fatal("a writer with no home wrote somewhere")
	} else if !strings.Contains(err.Error(), "home") {
		t.Fatalf("refused for the wrong reason: %v", err)
	}
}

/*
The whole path, with a stand-in for the engine.

What is being checked is the argv: the key and the value arrive as two
arguments in that order, and nothing else is added. A stand-in records what it
was given, which is the only way to see an argv from outside a process.
*/
func TestWhatReachesTheEngine(t *testing.T) {
	dir := t.TempDir()
	engine := filepath.Join(dir, "esper-engine")
	record := filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > " + record + "\n"
	if err := os.WriteFile(engine, []byte(script), 0o700); err != nil {
		t.Fatalf("could not write the stand-in engine: %v", err)
	}
	w := Writer{Engine: engine, Home: dir}

	if err := w.Set(context.Background(), "host.fps", "45"); err != nil {
		t.Fatalf("a setting was refused: %v", err)
	}
	said, err := os.ReadFile(record)
	if err != nil {
		t.Fatalf("the engine was never asked: %v", err)
	}
	want := "config\nset\nhost.fps\n45\n"
	if string(said) != want {
		t.Fatalf("the engine was asked %q, and it should be %q", said, want)
	}

	if err := w.Unset(context.Background(), "host.fps"); err != nil {
		t.Fatalf("unsetting was refused: %v", err)
	}
	said, _ = os.ReadFile(record)
	if string(said) != "config\nunset\nhost.fps\n" {
		t.Fatalf("unsetting asked %q", said)
	}

	// And a value with a space in it is still one argument: an interface list
	// or a name is a real value, and splitting it would change what was set.
	if err := w.Set(context.Background(), "device.name", "the one under the stairs"); err != nil {
		t.Fatalf("a value with spaces was refused: %v", err)
	}
	said, _ = os.ReadFile(record)
	if string(said) != "config\nset\ndevice.name\nthe one under the stairs\n" {
		t.Fatalf("a value with spaces became %q", said)
	}
}

// An answer with nothing in it is a failure, not an empty settings screen.
func TestAnEmptyRegistryIsRefused(t *testing.T) {
	dir := t.TempDir()
	engine := filepath.Join(dir, "esper-engine")
	script := "#!/bin/sh\necho '{\"file\":null,\"present\":false,\"settings\":[]}'\n"
	if err := os.WriteFile(engine, []byte(script), 0o700); err != nil {
		t.Fatalf("could not write the stand-in engine: %v", err)
	}
	w := Writer{Engine: engine, Home: dir}
	if _, err := w.Read(context.Background()); err == nil {
		t.Fatal("an engine that listed no settings was believed")
	}
}

// And what the engine says about a refusal is what the caller is told.
func TestTheEnginesOwnRefusalIsWhatComesBack(t *testing.T) {
	dir := t.TempDir()
	engine := filepath.Join(dir, "esper-engine")
	script := "#!/bin/sh\necho 'host.fps is outside 1 to 240.' >&2\nexit 1\n"
	if err := os.WriteFile(engine, []byte(script), 0o700); err != nil {
		t.Fatalf("could not write the stand-in engine: %v", err)
	}
	w := Writer{Engine: engine, Home: dir}
	err := w.Set(context.Background(), "host.fps", "9999")
	if err == nil {
		t.Fatal("a value the engine refused was reported as set")
	}
	if !strings.Contains(err.Error(), "outside 1 to 240") {
		t.Fatalf("the engine's reason was lost: %v", err)
	}
}

/*
The threat model names the same six, and says why.

A security boundary described in one place and enforced in another drifts, and
the direction it drifts is always the same: the list grows or shrinks in the
code and the document keeps describing the old one. Somebody reading the
threat model to decide whether to deploy this is reading the document.
*/
func TestTheThreatModelNamesTheSameSettings(t *testing.T) {
	said, err := os.ReadFile(filepath.Join("..", "..", "docs", "threat-model.md"))
	if err != nil {
		t.Fatalf("reading the threat model: %v", err)
	}
	text := string(said)
	for key := range hostOnly {
		if !strings.Contains(text, key) {
			t.Errorf("the threat model does not name %s, which the control plane refuses", key)
		}
	}
	// And it says how many, so one added in the code and not in the document
	// is a number that stops matching.
	if !strings.Contains(text, "six settings") {
		t.Error("the threat model does not say how many settings need somebody at the machine")
	}
	if len(hostOnly) != 6 {
		t.Errorf("%d settings need somebody at the machine, and the threat model says six", len(hostOnly))
	}
}

/*
A file the engine will not read becomes a sentence, not an empty screen.

This is the case the settings screen is most needed for: somebody has written
something the loader refuses, and the machine will not start with it. The
engine says which line and what is wrong with it, and that sentence is what
has to reach them — not "could not read settings", and not a screen showing
thirty-two defaults as though the file were fine.
*/
func TestABrokenFileIsReportedWithTheEnginesOwnWords(t *testing.T) {
	dir := t.TempDir()
	engine := filepath.Join(dir, "esper-engine")
	script := "#!/bin/sh\n" +
		"echo 'esper-engine: /home/somebody/.esper/config.toml, line 2: sections are not used here' >&2\n" +
		"exit 1\n"
	if err := os.WriteFile(engine, []byte(script), 0o700); err != nil {
		t.Fatalf("could not write the stand-in engine: %v", err)
	}
	w := Writer{Engine: engine, Home: dir}

	_, err := w.Read(context.Background())
	if err == nil {
		t.Fatal("a file the engine refused was read as a settings list")
	}
	if !strings.Contains(err.Error(), "line 2") {
		t.Fatalf("which line is wrong was lost: %v", err)
	}
	if !strings.Contains(err.Error(), "sections are not used here") {
		t.Fatalf("what is wrong with it was lost: %v", err)
	}
}
