package proto

import "testing"

/*
Which way a session went, and what an old line means.

`esper sessions` puts an arrow on every line so somebody scanning it can tell
"they came in here" from "I went out there" — the question they are actually
deciding between. The field is newer than the log, so **every line written
before it existed has no direction at all**, and those are inbound: the log
began as a record of who connected to this machine.

Reading them as outbound instead would quietly rewrite history in the one place
somebody looks to find out whether an access was theirs.
*/
func TestAnArrowSaysWhichWayAndAnOldLineIsInbound(t *testing.T) {
	for _, c := range []struct {
		name      string
		direction string
		want      string
	}{
		{"somebody connected to this machine", "in", "←"},
		{"this machine connected to somebody", "out", "→"},
		{"a line written before the field existed", "", "←"},
		{"a direction a later version writes", "sideways", "←"},
	} {
		t.Run(c.name, func(t *testing.T) {
			if got := (Happening{Direction: c.direction}).Arrow(); got != c.want {
				t.Errorf("a %q line points %q, want %q", c.direction, got, c.want)
			}
		})
	}
}

/*
An empty report is not an answer.

The engine's report is Rust's externally tagged enum: a map with exactly one
key, held here as a struct of pointers with exactly one set. A report with
*none* set is what arrives when the engine answered with something this build
does not know — a variant added on the other side — and a caller that treated
it as an answer would show a machine with no capabilities, no sessions and no
error, which reads as a working machine that does nothing.
*/
func TestAReportWithNothingInItSaysSo(t *testing.T) {
	if !(Report{}).IsEmpty() {
		t.Error("a report with nothing set did not say it was empty")
	}

	failed := "the engine could not open a display"
	for _, c := range []struct {
		name string
		of   Report
	}{
		{"a status", Report{Status: &Status{}}},
		{"sessions", Report{Sessions: &[]Happening{}}},
		{"an alive", Report{Alive: &Alive{}}},
		{"a failure", Report{Failed: &Failure{Detail: failed}}},
	} {
		t.Run(c.name, func(t *testing.T) {
			if c.of.IsEmpty() {
				t.Errorf("a report carrying %s was called empty", c.name)
			}
		})
	}

	// An empty *list* of sessions is an answer — "nothing has happened here"
	// — and it is not the same as no answer at all.
	none := []Happening{}
	if (Report{Sessions: &none}).IsEmpty() {
		t.Error("a machine that answered with no sessions was treated as not having answered")
	}
}
