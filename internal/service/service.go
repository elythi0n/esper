// Package service turns the host on and off through the machine's own service
// manager.
//
// # Why the control plane wraps this at all
//
// Service management is the operating system's job, and wrapping one platform's
// tool means the other two are missing — which is a real argument against doing
// it. Two things answer it.
//
// **The unit file already exists and installing it is four commands from a
// comment block.** One of them is `loginctl enable-linger`, and it is the one
// people miss. Without it systemd stops user services when the last session
// ends, so on a headless machine Esper stops the moment you disconnect — which
// is precisely the machine somebody wanted it on, and the failure looks like
// Esper crashing rather than like a missing setting. A product can close that.
//
// **A screen cannot ask somebody to copy commands out of a comment.** Turning
// the host on is most of what the main screen is, and it has to work by being
// pressed.
//
// # What keeps it from being a black box
//
// **Every command is printed before it is run.** The wrapper is a shortcut for
// something a person could do themselves, so it shows them what that is —
// which means a machine where it fails is still a machine they can finish by
// hand, and the next one they can do without this.
//
// **It refuses rather than guesses.** On a platform with no arm here it says
// which service manager it would need and stops. A wrapper that half-worked
// would leave a state nobody could reason about.
//
// **It will not overwrite a unit it did not write.** A distribution package
// may have installed one, with different paths and different hardening, and
// replacing it silently would undo somebody's packaging.
package service

import (
	_ "embed"
	"errors"
	"fmt"
	"io"
)

// Unit is the user service the host runs as.
//
// Embedded, so a single binary can install it — and this is the **only** copy:
// the file lives here rather than in `packaging/` precisely so that the one
// the binary writes and the one `systemd-analyze` checks cannot drift apart.
//
//go:embed esper.service
var Unit string

// ErrUnsupported is returned where there is no arm for this platform.
var ErrUnsupported = errors.New("no service manager is wired up for this platform")

// Where the unit is installed, relative to the user's config directory.
const unitName = "esper.service"

// Report is where a wrapper says what it is doing.
//
// Every command is written here before it runs, so the person watching can do
// it themselves next time.
type Report struct {
	To io.Writer
}

func (r Report) say(format string, args ...any) {
	if r.To == nil {
		return
	}
	// Nothing useful to do if the report cannot be written: the work still
	// happened, and failing a service change over a broken stdout would be
	// worse than a change nobody saw.
	_, _ = fmt.Fprintf(r.To, format+"\n", args...)
}
