package main

import (
	"bufio"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"

	"github.com/elythi0n/esper/internal/devices"
)

/*
`esper` on its own, at a terminal: the machines, and one key to reach one.

# Why a list and not a menu system

Because the product's whole promise is one step from a machine's name to its
screen, and the desktop app's own Devices screen is a list of rows with a
Connect on each. A terminal that offered a nested menu would be a second idea
of what this program is for, reached by the people least likely to want it.

# Why it is only at a terminal

With no terminal there is nobody to ask, so `esper` prints its help — which is
what a script running it with no arguments needs, and what it did before this
existed. The distinction is whether *stdin* is a terminal, not stdout: a
`esper | less` is still somebody sitting there, and refusing to ask them
because their output is redirected would be answering the wrong question.

# Why it hands over rather than wrapping

`connect` owns a terminal, and a parent in the middle forwarding signals will
eventually forward one wrongly — the same reason every other verb here is an
exec rather than a subprocess. Choosing a machine *is* running `esper connect
<name>`, and from the moment the key is pressed this program is gone.
*/

// atATerminal is whether there is somebody to ask.
//
// A variable so a test can answer for itself: the alternative is a test that
// can only run under a pty, and this program's behaviour with no terminal is
// exactly as much worth checking as its behaviour with one.
//
// **`ModeCharDevice` is not the question**, which is what this asked first:
// `/dev/null` is a character device, so `esper < /dev/null` — a script, a
// service, a CI step — was offered a list and asked to pick from it. The
// question is whether the descriptor has a terminal behind it, and the only
// thing that answers that is asking the terminal driver.
var atATerminal = func() bool {
	return isATerminal(os.Stdin.Fd())
}

/*
interactive lists the paired machines and says which command was chosen.

It **decides and does not act**: what comes back is the argument list to hand
the engine, or nil for "nothing to run". Acting here would put the one thing
that cannot be tested — an `exec` that replaces this process — inside the one
thing most worth testing, which is what a person's answer means.

`handled` is false when there is nothing to be interactive about — no
terminal, or no machines — and the caller prints the help it always printed.
*/
func interactive(in io.Reader, out io.Writer) (run []string, handled bool) {
	if !atATerminal() {
		return nil, false
	}
	home, err := esperHome()
	if err != nil {
		return nil, false
	}
	paired, err := devices.Load(devices.PathIn(home))
	if err != nil || len(paired) == 0 {
		// A machine with nothing paired has nothing to offer, and the help is
		// where `pair` is named. Said by the help rather than by a screen that
		// exists to say it is empty.
		return nil, false
	}

	say(out, listing(paired))
	reader := bufio.NewReader(in)
	for {
		sayf(out, "\n  1–%d to connect · p to pair · q to quit: ", len(paired))
		line, err := reader.ReadString('\n')
		if err != nil && line == "" {
			// The terminal went away mid-question, which is an ordinary way
			// for this to end: somebody pressed Ctrl-D.
			say(out, "\n")
			return nil, true
		}
		switch answer := strings.ToLower(strings.TrimSpace(line)); answer {
		case "", "q", "quit", "exit":
			return nil, true
		case "p", "pair":
			return []string{"pair"}, true
		case "h", "?", "help":
			printHelp()
			return nil, true
		default:
			n, err := strconv.Atoi(answer)
			if err != nil || n < 1 || n > len(paired) {
				sayf(out, "  %q is not one of them.\n", answer)
				continue
			}
			// By name rather than by number: what is run is a command somebody
			// could have typed, and the next thing they type will be that.
			name := paired[n-1].Name
			sayf(out, "\n  esper connect %s\n\n", name)
			return []string{"connect", name}, true
		}
	}
}

/*
say writes to the terminal, and has nothing to do when it cannot.

`fmt.Fprintf`'s error here is a terminal that has gone away mid-question —
somebody closed the window, or the pipe on the other end of a `script` ended.
There is nothing to report and nowhere to report it, and the loop finds out
anyway on its next read. Named so that is a decision rather than an ignored
return value.
*/
func say(out io.Writer, text string) {
	_, _ = io.WriteString(out, text)
}

func sayf(out io.Writer, format string, args ...any) {
	_, _ = fmt.Fprintf(out, format, args...)
}

// listing is the machines, as a person reads them.
//
// Separate from the asking so it can be checked without a terminal: what a
// list looks like is worth pinning, and it is the part somebody reads.
func listing(paired []devices.Device) string {
	var b strings.Builder
	b.WriteString("\n🖥  esper\n\n  Paired machines\n")
	for i, d := range paired {
		name := d.Name
		if strings.TrimSpace(name) == "" {
			// A device that arrived with no name is still a device somebody
			// can reach; the fingerprint is what it is called until it has one.
			name = d.PublicKey.String()[:16]
		}
		// The fingerprint under the name, because two machines may share a
		// name and the key is what decides which one this is.
		fmt.Fprintf(&b, "    %d  %-24s %s\n", i+1, name, d.PublicKey.String()[:16])
	}
	return b.String()
}
