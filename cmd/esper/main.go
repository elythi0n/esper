// Command esper is the Esper control plane: the CLI, the device store, the
// local API, and the supervisor for the Rust engine that does the realtime work.
//
// Subcommands are dispatched by hand on flag rather than through a framework,
// matching the sibling projects. Running bare with no arguments prints help
// when not attached to a terminal, so scripts are never left waiting on input.
package main

import (
	"context"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/elythi0n/esper/internal/engine"
	"github.com/elythi0n/esper/internal/proto"
)

// version and buildTime are stamped at link time via
// -ldflags "-X main.version=... -X main.buildTime=...". The Rust binaries read
// the same VERSION file through their build script, so the two halves of
// Esper cannot disagree about which release they are.
var (
	version   = "dev"
	buildTime = ""
)

func main() { os.Exit(run(os.Args[1:])) }

// run dispatches argv without the program name and returns an exit code.
func run(args []string) int {
	if len(args) == 0 {
		printHelp()
		return 0
	}
	switch args[0] {
	case "-h", "--help", "help":
		printHelp()
		return 0
	case "-v", "--version", "version":
		return cmdVersion(args[1:])
	case "doctor":
		return cmdDoctor(args[1:])
	case "sessions":
		return cmdSessions(args[1:])
	case "broker":
		return handOff("esper-broker", args[1:])
	case "host":
		return cmdHost(args[1:])
	default:
		if isEngineCommand(args[0]) {
			return handOff("esper-engine", args)
		}
		fmt.Fprintf(os.Stderr, "esper: unknown command %q\n\n", args[0])
		printHelp()
		return 1
	}
}

// engineCommands are the verbs the engine owns.
//
// **A list, not "anything we have never heard of".** Passing unknown words
// through would turn a typo into the engine's error message about a command
// `esper` never had, and would make the two programs' vocabularies drift
// silently as either grew.
//
// Being in this list means the engine has the verb, not that the front door
// hands it straight over: `host` is here and is supervised instead, because it
// is the one verb that runs for weeks. One list with one meaning; the
// dispatcher above decides what to do with each.
var engineCommands = map[string]bool{
	"identity": true,
	"discover": true,
	"pair":     true,
	"config":   true,
	"devices":  true,
	"grant":    true,
	"revoke":   true,
	"schedule": true,
	"keep":     true,
	"forget":   true,
	"invite":   true,
	"join":     true,
	"host":     true,
	"connect":  true,
	"terminal": true,
	"forward":  true,
	"cp":       true,
}

func isEngineCommand(name string) bool { return engineCommands[name] }

// handOff replaces this process with another of Esper's binaries.
//
// The session verbs own a terminal — raw mode, a pipe that is somebody's ssh
// connection, Ctrl-C — so `esper` gets out of the way entirely rather than
// sitting in the middle forwarding signals it will eventually forward wrongly.
// On Windows, where there is no exec, it waits and passes the exit status back.
func handOff(binary string, args []string) int {
	path, err := engine.Locate(binary)
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper: %v\n", err)
		return 1
	}
	code, err := engine.Handoff(path, "esper", args)
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper: %v\n", err)
		return 1
	}
	return code
}

func cmdVersion(args []string) int {
	if len(args) > 0 && args[0] == "--json" {
		fmt.Printf("{\"version\":%q,\"buildTime\":%q,\"protocol\":%d}\n",
			version, buildTime, proto.ProtocolVersion)
		return 0
	}
	fmt.Printf("esper %s\n", version)
	fmt.Printf("  built     %s\n", displayOrDash(buildTime))
	fmt.Printf("  protocol  %d\n", proto.ProtocolVersion)
	return 0
}

// displayOrDash renders an unstamped field as an em dash rather than an empty
// column, so `version` output stays readable in a development build.
func displayOrDash(s string) string {
	if s == "" {
		return "—"
	}
	return s
}

// cmdDoctor answers "why doesn't it work". Failure in this product has a dozen
// causes, so the diagnostic grows with each phase; today it covers the seam
// between the control plane and the engine, which is the only moving part.
func cmdDoctor(_ []string) int {
	fmt.Println("🩺  esper doctor")
	fmt.Println()

	fmt.Println("Control plane")
	fmt.Printf("  version   %s\n", version)
	fmt.Printf("  built     %s\n", displayOrDash(buildTime))
	fmt.Printf("  protocol  %d\n", proto.ProtocolVersion)
	fmt.Println()

	fmt.Println("Engine")
	// Engine output goes to stderr with its origin on it, so the report on
	// stdout stays a report — `esper doctor > file` should not have a codec's
	// warning in the middle of it.
	sup, err := engine.New(engine.Options{Log: logToStderr})
	if err != nil {
		fmt.Printf("  ✗ %v\n", err)
		fmt.Println()
		fmt.Println("⚠  the engine does the realtime work; without it esper can only")
		fmt.Println("   report its own version.")
		return 1
	}
	fmt.Printf("  path      %s\n", sup.Path())

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	local := proto.NewHello(proto.RoleController, nil, hostname(), version)
	if err := sup.Start(ctx, local); err != nil {
		fmt.Printf("  ✗ handshake failed: %v\n", err)
		return 1
	}
	defer func() { _ = sup.Stop() }()

	h := sup.Hello()
	fmt.Printf("  version   %s\n", h.Engine)
	fmt.Printf("  protocol  %d\n", h.Proto)
	fmt.Printf("  ✓ handshake ok over the control socket\n")
	fmt.Println()

	status, err := sup.Status(ctx)
	if err != nil {
		fmt.Printf("  ✗ %v\n", err)
		return 1
	}

	fmt.Println("This machine")
	ready := true
	ready = capability("screen  ", status.Capture) && ready
	ready = capability("encoder ", status.Encoder) && ready
	// Input is not required to be a useful host: a machine that can be watched
	// and not driven is a machine somebody can still read a log on. Reported,
	// and not counted against readiness.
	_ = capability("input   ", status.Input)
	fmt.Println()

	fmt.Println("Reaching this machine")
	if status.MDNS {
		fmt.Println("  network   announcing itself on the local network")
	} else {
		fmt.Println("  network   not announcing itself (discovery.mdns is off)")
	}
	if len(status.Overlays) == 0 {
		fmt.Println("  private   none — no WireGuard, Tailscale or other overlay found")
	} else {
		fmt.Printf("  private   %s\n", strings.Join(status.Overlays, ", "))
	}
	if status.Broker == nil {
		// Not a fault. There is no default broker anywhere in Esper, and
		// saying so is the honest report rather than a warning about a
		// missing setting.
		fmt.Println("  broker    none configured — the broker tiers do not exist here")
	} else {
		fmt.Printf("  broker    %s\n", *status.Broker)
	}
	fmt.Printf("  paired    %d device(s)\n", status.Paired)
	fmt.Println()

	// A second `esper` earlier on PATH is the fault that makes every other
	// answer here wrong, and it is invisible until somebody notices the
	// version they are running is not the one they installed.
	if others := otherBinariesOnPath("esper"); len(others) > 1 {
		fmt.Println("⚠  more than one `esper` is on PATH:")
		for _, p := range others {
			fmt.Printf("     %s\n", p)
		}
		fmt.Println("   the first one wins, which may not be the one you installed")
		fmt.Println()
	}

	if !ready {
		fmt.Println("⚠  this machine cannot host a screen session. It can still be a")
		fmt.Println("   controller, and terminal mode and port forwarding need none of")
		fmt.Println("   the above.")
		return 1
	}
	fmt.Println("✓ ready")
	return 0
}

// capability prints one line and says whether it was available.
func capability(label string, c proto.Capability) bool {
	mark := "✓"
	if !c.Available {
		mark = "✗"
	}
	fmt.Printf("  %s %s %s\n", label, mark, c.Detail)
	return c.Available
}

// otherBinariesOnPath finds every executable called name on PATH, in order.
//
// `exec.LookPath` answers which one wins. The question a diagnostic has to
// answer is whether there is more than one, because that is the fault where
// every other line of output is about a different program than the user
// thinks.
func otherBinariesOnPath(name string) []string {
	var found []string
	for _, dir := range filepath.SplitList(os.Getenv("PATH")) {
		if dir == "" {
			continue
		}
		p := filepath.Join(dir, name)
		info, err := os.Stat(p)
		if err != nil || info.IsDir() || info.Mode()&0o111 == 0 {
			continue
		}
		found = append(found, p)
	}
	return found
}

// cmdHost runs the engine's host, and keeps it running.
//
// **The one verb that is supervised rather than handed over.** Everything else
// is a person doing something for a minute; this one runs for weeks on a
// machine nobody is looking at, and the difference between a crash that comes
// back and a crash that is discovered next Tuesday is a supervisor.
//
// The engine keeps its own stdio: `esper host` prints a live report of what it
// is serving, and putting that through a logger would turn a layout into a
// transcript.
func cmdHost(args []string) int {
	sup, err := engine.New(engine.Options{Log: logToStderr})
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper host: %v\n", err)
		return 1
	}
	// The engine is told the name it was reached under like every other verb,
	// so its messages say `esper` rather than the name of a binary.
	if err := sup.Supervise(context.Background(), append([]string{"host"}, args...), engine.Watch{}); err != nil {
		fmt.Fprintf(os.Stderr, "esper host: %v\n", err)
		return 1
	}
	return 0
}

// cmdSessions prints what has happened on this machine.
//
// Recent, from the audit log. **Not live** — this machine's host runs as its
// own process today and nothing here can see inside it, and a list that
// silently omitted a session in progress would be worse than one that says
// which question it is answering.
func cmdSessions(args []string) int {
	most := uint32(20)
	if len(args) > 0 {
		n, err := strconv.ParseUint(args[0], 10, 32)
		if err != nil || n == 0 {
			fmt.Fprintf(os.Stderr, "esper sessions: %q is not a number of lines\n", args[0])
			return 1
		}
		most = uint32(n)
	}

	sup, err := engine.New(engine.Options{Log: logToStderr})
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper sessions: %v\n", err)
		return 1
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := sup.Start(ctx, proto.NewHello(proto.RoleController, nil, hostname(), version)); err != nil {
		fmt.Fprintf(os.Stderr, "esper sessions: %v\n", err)
		return 1
	}
	defer func() { _ = sup.Stop() }()

	happenings, err := sup.Sessions(ctx, most)
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper sessions: %v\n", err)
		return 1
	}

	fmt.Println("🗂  esper sessions")
	fmt.Println()
	if len(happenings) == 0 {
		fmt.Println("  nothing has happened on this machine yet")
		fmt.Println()
		fmt.Println("  Sessions, pairings and refusals are recorded as they happen.")
		return 0
	}
	for _, h := range happenings {
		// The arrow first, because which way a session went is the thing a
		// person scanning this list is deciding between: somebody came in
		// here, or somebody here went out there.
		fmt.Printf("  %s  %s %s  %s\n", when(h.At), h.Arrow(), who(h), h.Detail)
	}
	fmt.Println()
	fmt.Println("  This is what was recorded, not what is happening now: a session in")
	fmt.Println("  progress appears here when it ends.")
	return 0
}

// who names the device a line is about.
//
// The name it has now, with enough of its key to tell two of the same name
// apart — and where a device has been unpaired since, the key alone, because
// the honest answer to "who was that" is that this machine no longer knows.
func who(h proto.Happening) string {
	short := h.Key
	if len(short) > 8 {
		short = short[:8]
	}
	if h.Device == "" {
		return fmt.Sprintf("%-14s", short)
	}
	name := h.Device
	if len(name) > 14 {
		name = name[:14]
	}
	return fmt.Sprintf("%-14s", name)
}

// when renders a timestamp in local time, to the minute.
//
// To the minute because that is the resolution somebody reads a log at, and
// local because the question is "was that me, an hour ago".
func when(at uint64) string {
	if at > math.MaxInt64 {
		return "—"
	}
	return time.Unix(int64(at), 0).Format("2006-01-02 15:04")
}

// logToStderr sends supervision events and the engine's own output to stderr,
// so a report on stdout stays a report.
func logToStderr(format string, args ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
}

func hostname() string {
	n, err := os.Hostname()
	if err != nil {
		return "unknown"
	}
	return n
}

func printHelp() {
	fmt.Print(`esper — secure, private remote desktop

Usage:
  esper <command> [flags]

Getting connected
  pair               pair with another device, in either direction
  devices            list paired devices and what they may do
  discover           list the Esper devices on this network
  invite             offer somebody a way in: one link, through your broker
  join               take somebody up on an invitation

Using a machine
  connect            show a paired host's screen
  terminal           open a shell on a paired host
  forward            reach a port on a paired host, ssh -L style
  cp                 copy one file to or from a paired host
  host               serve this machine's screen to a paired device

Deciding who may
  grant              give a paired device a permission
  revoke             take a permission away
  schedule           when a device may connect unattended, in local time
  keep               make a one-session pairing last
  forget             unpair a device entirely

This machine
  config             show every setting and where its value came from
  identity           print this device's fingerprint
  doctor             check what this machine can do, and what it cannot
  sessions [n]       what has happened here: sessions, pairings, refusals
  broker             run an introducer for your own machines
  version [--json]   print the release, build time and protocol version
  help               this text

Commands that take options will list them: esper connect --help

Esper operates no infrastructure. There is no account, no password, and no
default server: devices pair once with a short code and know each other by
public key thereafter.
`)
}
