package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestAShapeSurvivesTheHome is the round trip, and the reason the file is
// JSON rather than anything cleverer: it is four numbers and a flag.
func TestAShapeSurvivesTheHome(t *testing.T) {
	t.Parallel()
	home := t.TempDir()

	if got := loadWindowState(home); got != (windowState{}) {
		t.Fatalf("a home with no file should remember nothing, got %+v", got)
	}

	want := windowState{Width: 1024, Height: 700, X: 40, Y: 60, Maximised: true}
	saveWindowState(home, want)
	if got := loadWindowState(home); got != want {
		t.Errorf("remembered %+v, wanted %+v", got, want)
	}

	// The home holds a token beside this. A file the rest of the directory
	// would not tolerate is a file somebody has to think about.
	info, err := os.Stat(filepath.Join(home, stateFile))
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0o600 {
		t.Errorf("the state file is %v, wanted 0600", perm)
	}
}

// TestNonsenseIsNotObeyed keeps the window openable.
//
// Each of these has a way of being written: a zero from a file that was
// never filled in, a sliver from a window that was minimised when it was
// read, and something enormous from a hand edit. Obeying any of them opens a
// window somebody cannot use, and the defaults are always better than that.
func TestNonsenseIsNotObeyed(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		why   string
		state windowState
		sane  bool
	}{
		{"nothing written yet", windowState{}, false},
		{"minimised to a sliver", windowState{Width: 160, Height: 28}, false},
		{"narrower than the minimum", windowState{Width: 479, Height: 400}, false},
		{"shorter than the minimum", windowState{Width: 480, Height: 399}, false},
		{"exactly the minimum", windowState{Width: 480, Height: 400}, true},
		{"a hand-edited absurdity", windowState{Width: 100000, Height: 100000}, false},
		{"an ordinary window", windowState{Width: 1280, Height: 840}, true},
	} {
		if got := c.state.sane(); got != c.sane {
			t.Errorf("%s: sane() = %v, wanted %v", c.why, got, c.sane)
		}
	}
}

// TestARuinedFileIsNotAFailure — a half-written or hand-mangled file should
// open the default window, not stop the application starting.
func TestARuinedFileIsNotAFailure(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if err := os.WriteFile(filepath.Join(home, stateFile), []byte(`{"width": nope`), 0o600); err != nil {
		t.Fatalf("writing: %v", err)
	}
	if got := loadWindowState(home); got != (windowState{}) {
		t.Errorf("a ruined file should remember nothing, got %+v", got)
	}
	if loadWindowState(home).sane() {
		t.Error("and what it remembers should not be obeyed")
	}
}

// TestALongHomeStillGetsALock is a bug found by running the window from a
// scratch directory a hundred and twenty characters deep: the socket path
// exceeded what a unix socket may be, `net.Listen` said
// `bind: invalid argument`, and **the window refused to start at all**.
//
// A home can be anywhere — a deeply nested directory, a container mount, a
// path with a long project name in it — so this is not an exotic case, and
// the failure it produced was both fatal and misleading.
func TestALongHomeStillGetsALock(t *testing.T) {
	t.Parallel()

	short := "/tmp/esper"
	if got := lockPath(short); got != filepath.Join(short, lockName) {
		t.Errorf("an ordinary home keeps its socket inside it, got %q", got)
	}

	deep := "/tmp/" + strings.Repeat("a-long-directory-name/", 8) + "home"
	if len(deep) <= longestSocketPath {
		t.Fatalf("the test's own path is only %d characters, so it proves nothing", len(deep))
	}
	moved := lockPath(deep)
	if len(moved) > longestSocketPath {
		t.Errorf("the socket for a deep home is still %d characters: %q", len(moved), moved)
	}
	if moved == filepath.Join(deep, lockName) {
		t.Error("the socket did not move")
	}

	// **And it still means one window per home.** A shortened name that
	// collided would let two homes fight over one lock, which is worse than
	// the bug it fixed.
	other := "/tmp/" + strings.Repeat("a-long-directory-name/", 8) + "another"
	if lockPath(other) == moved {
		t.Error("two different homes were given the same lock")
	}

	// The same home, twice, is the same lock — or a second launch would dial
	// a path nothing is listening on.
	if lockPath(deep) != moved {
		t.Error("the same home was given two different locks")
	}
}
