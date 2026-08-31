package main

import (
	"strings"
	"testing"
)

/*
What may reach a command line.

`start` puts the device on `esper connect <device> --viewer <socket>`, so the
check that it is a fingerprint is the one thing here worth attacking. It is an
allowlist rather than an escape, and it runs *before* anything is resolved or
spawned — which is what makes these tests possible without a binary to run.
*/
func TestOnlyAFingerprintReachesACommandLine(t *testing.T) {
	refused := []struct {
		what   string
		device string
	}{
		{"empty", ""},
		{"a flag", "--record"},
		{"a flag stuck to a fingerprint", "-x827aabec450ddf13"},
		{"shell metacharacters", "827aabec450ddf13; rm -rf /"},
		{"a pipe", "827aabec450ddf13|sh"},
		{"a newline and a second argument", "827aabec450ddf13\n--record /tmp/x"},
		{"a path", "../../etc/passwd"},
		{"an absolute path", "/etc/passwd"},
		{"too short", "827aabec450ddf1"},
		{"too long", "827aabec450ddf130"},
		{"uppercase, which a fingerprint is not", "827AABEC450DDF13"},
		{"not hexadecimal", "827aabec450ddfzz"},
		{"a space in the middle", "827aabec 450ddf13"},
		{"a nul", "827aabec450ddf13\x00"},
	}

	var sessions running
	for _, c := range refused {
		t.Run(c.what, func(t *testing.T) {
			err := sessions.start(c.device, "/tmp/socket")
			if err == nil {
				t.Fatalf("%q was accepted onto a command line", c.device)
			}
			// The refusal must be *this* one. A device that is refused later,
			// because no `esper` was found beside the test binary, would pass
			// a test that only checked for an error — and would mean the
			// allowlist had let it through.
			if !strings.Contains(err.Error(), "that is not a fingerprint") {
				t.Fatalf("%q was refused for the wrong reason: %v", c.device, err)
			}
		})
	}
}

/*
And a real one gets past the allowlist.

It does not get a session — there is no `esper` next to a test binary — and
that is the point: the error has to come from *after* the check, or the check
is refusing everything and the tests above prove nothing.
*/
func TestAFingerprintGetsPastTheAllowlist(t *testing.T) {
	var sessions running
	err := sessions.start("827aabec450ddf13", "/tmp/socket")
	if err == nil {
		t.Skip("something called `esper` is next to the test binary, so this went further than expected")
	}
	if strings.Contains(err.Error(), "that is not a fingerprint") {
		t.Fatalf("a real fingerprint was refused by the allowlist: %v", err)
	}
	if !strings.Contains(err.Error(), "no `esper` next to this window") {
		t.Fatalf("refused for an unexpected reason: %v", err)
	}
}

// Stopping a window that never started anything is not an error.
//
// The page calls this when a session screen goes away, which it does whether
// or not one was ever running.
func TestStoppingNothingIsFine(t *testing.T) {
	var sessions running
	sessions.stop()
	sessions.stop()
}
