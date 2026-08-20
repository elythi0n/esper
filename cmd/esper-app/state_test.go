package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

/*
Where a remembered window may be put back.

The failure this rule exists to prevent is a window that opens where nothing
can be seen — unplug the monitor a window was on, and the coordinates in the
file put it off the desktop, which looks exactly like an application that
failed to start. So the position is honoured only if it lands somewhere a
person could still reach it.

It is a **bound, not a hit test**: Wails 2's screens carry sizes and no origin,
so there is no way to know that the second monitor starts at x=1920. The edges
are what this covers, because a rule made of four comparisons and a margin is a
rule with four places to be off by one — and none of them is reachable through
a runtime that only exists inside a running window.
*/
func TestAWindowIsPutBackOnlyWhereItCanBeReached(t *testing.T) {
	one := []screenSize{{Width: 1920, Height: 1080}}
	two := []screenSize{{Width: 1920, Height: 1080}, {Width: 2560, Height: 1440}}

	for _, c := range []struct {
		name    string
		state   windowState
		screens []screenSize
		want    bool
	}{
		{"in the middle of one screen", windowState{X: 400, Y: 200}, one, true},
		{"at the origin", windowState{X: 0, Y: 0}, one, true},
		{"on the second of two", windowState{X: 2400, Y: 100}, two, true},

		// 64px of the title bar has to be on the desktop, because dragging it
		// is the only way to move the window back.
		{"just enough of the bar to grab", windowState{X: -63, Y: 0}, one, true},
		{"the bar exactly off the edge", windowState{X: -64, Y: 0}, one, false},
		{"well off to the left", windowState{X: -2000, Y: 0}, one, false},
		{"past the right of every screen", windowState{X: 1900, Y: 0}, one, false},
		{"past the right of both", windowState{X: 4500, Y: 0}, two, false},

		// Above the desktop there is no title bar to reach at all.
		{"above the top", windowState{X: 100, Y: -1}, one, false},
		{"below the tallest, stacked", windowState{X: 100, Y: 2159}, one, false},

		// Nothing to check against is not the same as a bad position: the
		// common case for an empty answer is the same machine with the same
		// monitors, and moving the window then is the failure, not the fix.
		{"no screens answered", windowState{X: 5000, Y: 5000}, nil, true},
		{"screens that reported nothing", windowState{X: 5000, Y: 5000}, []screenSize{{}}, true},
	} {
		t.Run(c.name, func(t *testing.T) {
			if got := withinScreens(c.state, c.screens); got != c.want {
				t.Errorf("withinScreens(%+v, %v) = %v, want %v", c.state, c.screens, got, c.want)
			}
		})
	}
}

// A remembered size that no window manager would honour is not honoured here
// either — and unlike a position, a size that is merely wrong is clamped
// rather than dangerous, so the rule is only about the absurd.
func TestASizeIsSaneOrIgnored(t *testing.T) {
	for _, c := range []struct {
		name  string
		state windowState
		want  bool
	}{
		{"an ordinary window", windowState{Width: 1200, Height: 800}, true},
		{"nothing remembered", windowState{}, false},
		{"a negative size", windowState{Width: -1200, Height: 800}, false},
		{"a window one pixel tall", windowState{Width: 1200, Height: 1}, false},
	} {
		t.Run(c.name, func(t *testing.T) {
			if got := c.state.sane(); got != c.want {
				t.Errorf("%+v sane() = %v, want %v", c.state, got, c.want)
			}
		})
	}
}

/*
Nothing put into the inline script can end it.

The window serves its own page and writes two facts into it before the bundle
loads — the API's address, and the route a popped-out window opens on. The
route can come from an `esper://` link the operating system handed over, which
any page can ask a browser to open, so what reaches this has to be assumed to
be somebody else's text even though `linkShape` refuses all of it today.

The fatal case is not a broken quote: it is `</script>`. A browser looks for
the closing tag before it looks for the end of any string, so a value carrying
one ends the script and everything after it is markup the page will run.
*/
func TestNothingQuotedCanEndTheScript(t *testing.T) {
	for _, value := range []string{
		`</script><img src=x onerror=alert(1)>`,
		`</SCRIPT >`,
		`<!--<script>`,
		`"; alert(1); var x="`,
		`back\slash`,
		"a\nnewline",
		"a separator",
	} {
		got := quote(value, "#")

		if strings.Contains(strings.ToLower(got), "</script") {
			t.Errorf("quote(%q) leaves a closing tag in the script: %s", value, got)
		}
		for _, forbidden := range []string{"<", ">", "\n", "\r", " ", " "} {
			if strings.Contains(got, forbidden) {
				t.Errorf("quote(%q) leaves %q in the script: %s", value, forbidden, got)
			}
		}

		// And it is still a JavaScript string literal: one quote at each end
		// and none loose in the middle.
		if len(got) < 2 || got[0] != '"' || got[len(got)-1] != '"' {
			t.Fatalf("quote(%q) is not a quoted string: %s", value, got)
		}
		inner := got[1 : len(got)-1]
		for i := 0; i < len(inner); i++ {
			if inner[i] == '\\' {
				i++
				continue
			}
			if inner[i] == '"' {
				t.Errorf("quote(%q) leaves an unescaped quote: %s", value, got)
			}
		}
	}

	// The prefix is the caller's own and goes in ahead of the value, which is
	// how a route becomes a fragment.
	if got := quote("/pair/invited/abc", "#"); got != `"#/pair/invited/abc"` {
		t.Errorf("an ordinary route quoted to %s", got)
	}
}

// The page is the page, and everything with an extension is a file.
func TestOnlyADocumentIsRewritten(t *testing.T) {
	for _, c := range []struct {
		path string
		want bool
	}{
		{"", true},
		{"/", true},
		{"/index.html", true},
		{"/devices", true},
		{"/sessions/019a1125", true},
		{"/assets/index-a1b2c3.js", false},
		{"/assets/index-a1b2c3.css", false},
		{"/favicon.svg", false},
		{"/fonts/inter.woff2", false},
	} {
		if got := isDocument(c.path); got != c.want {
			t.Errorf("isDocument(%q) = %v, want %v", c.path, got, c.want)
		}
	}
}

/*
The shape survives a restart, and nothing about it is an error.

A window that has never been moved, a file somebody truncated, a home that is
read-only — none of those are failures worth a message: they are a window that
opens where the manager puts it, which is what happens the first time anyway.
What must not happen is a window that will not start because a remembered
shape could not be read.
*/
func TestTheWindowShapeSurvivesARestart(t *testing.T) {
	home := t.TempDir()

	if got := loadWindowState(home); got != (windowState{}) {
		t.Errorf("a home with no remembered shape produced %+v", got)
	}

	want := windowState{Width: 1600, Height: 900, X: 120, Y: 64, Maximised: true}
	saveWindowState(home, want)
	if got := loadWindowState(home); got != want {
		t.Errorf("saved %+v and read back %+v", want, got)
	}

	// 0600 like everything else under the home. It holds no secret, and a home
	// where one file is world-readable and the rest are not is a home somebody
	// has to think about.
	info, err := os.Stat(filepath.Join(home, stateFile))
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0o600 {
		t.Errorf("the remembered shape is %04o and everything under the home is 0600", perm)
	}

	// Half a file is not half a window.
	if err := os.WriteFile(filepath.Join(home, stateFile), []byte(`{"width":16`), 0o600); err != nil {
		t.Fatalf("writing a truncated file: %v", err)
	}
	if got := loadWindowState(home); got != (windowState{}) {
		t.Errorf("a truncated file produced %+v rather than nothing", got)
	}

	// And a home that cannot be written is not a window that fails to open.
	saveWindowState(filepath.Join(home, "no", "such", "place"), want)
}
