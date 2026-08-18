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
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unicode/utf8"

	"github.com/elythi0n/esper/internal/devices"
	"github.com/elythi0n/esper/internal/engine"
	"github.com/elythi0n/esper/internal/home"
	"github.com/elythi0n/esper/internal/proto"
	"github.com/elythi0n/esper/internal/server"
	"github.com/elythi0n/esper/internal/service"
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
	case "api":
		return cmdAPI(args[1:])
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
	// Locking a host's screen is its own verb because it is its own
	// permission: a device may hold `lock` without holding `view`, so this
	// works on a host it may not watch.
	"lock": true,
	// Logging a host out and restarting it are sensitive permissions of
	// their own: "may see the screen" and "may take the machine away" are
	// not the same grant.
	"logoff": true,
	"reboot": true,
	// A group is a label on the device records, so applying a policy to
	// several devices at once is one write and no second lifecycle.
	"group": true,
	// Waking a machine is the one verb that does not need the far end to be
	// running, which is exactly why it exists.
	"wake": true,
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
func cmdDoctor(args []string) int {
	deep := false
	for _, a := range args {
		if a == "--session" {
			deep = true
		}
	}
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
	fmt.Printf("  paired   %d device(s)\n", status.Paired)
	fmt.Println()

	reachTiers(status)

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
	if deep {
		// The engine this process started holds the control socket; the self
		// test starts its own, so let this one go first.
		_ = sup.Stop()
		return selfTest(sup.Path())
	}
	fmt.Println("✓ ready")
	fmt.Println()
	fmt.Println("  `esper doctor --session` holds a real session with itself — pairing,")
	fmt.Println("  transport, capture, encode and decode — and times it. A few seconds.")
	return 0
}

// reachTiers says which rungs of the connectivity ladder exist on this
// machine, and which therefore do not.
//
// **The lines above say what was found; this says what it means.** A person
// reading "no overlay found" and "no broker configured" has two facts and
// still has to know the ladder to work out that this machine can only be
// reached from its own network. Saying so is the whole job of a diagnostic —
// and an unavailable rung is not a fault, so each says *how* to have it rather
// than being marked wrong.
//
// The order is the order they are tried, which is the order `esper connect`
// walks: an address you typed, this network, where it was last time, a private
// network you already run, an introduction.
func reachTiers(status *proto.Status) {
	fmt.Println("Ways in, in the order they are tried")

	tier := func(name, detail string, have bool, without string) {
		if have {
			fmt.Printf("  ✓ %-10s %s\n", name, detail)
			return
		}
		fmt.Printf("  · %-10s unavailable — %s\n", name, without)
	}

	// Always available: it needs nothing of this machine but a socket, which
	// is why it is the rung everything else is a fallback from.
	tier("given", "an address somebody types always works, if it routes", true, "")
	tier("lan", "found by asking this network", status.MDNS,
		"discovery.mdns is off, so this machine answers no broadcast")
	// Remembered is a property of the *controller's* store, not of this
	// machine — a host cannot know whether somebody else remembers it.
	fmt.Printf("  · %-10s %s\n", "remembered",
		"decided by the controller, which is where the address is kept")
	tier("overlay", strings.Join(status.Overlays, ", "), len(status.Overlays) > 0,
		"no WireGuard, Tailscale or other overlay interface on this machine")
	tier("broker", brokerOf(status), status.Broker != nil,
		"none configured, and Esper ships no default: run one, or be reachable another way")

	if !status.MDNS && len(status.Overlays) == 0 && status.Broker == nil {
		fmt.Println()
		fmt.Println("⚠  the only way in is an address somebody already knows. That is a")
		fmt.Println("   working setup on a fixed address and nothing else — there is no")
		fmt.Println("   rung left if this machine moves.")
	}
	fmt.Println()
}

// brokerOf is the configured broker, for a line that only prints when there is
// one.
func brokerOf(status *proto.Status) string {
	if status.Broker == nil {
		return ""
	}
	return *status.Broker
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
	// **`enable` and `disable` are taken before the engine is looked for.**
	// Turning the host on at login is a thing to do on a machine where the
	// engine is not running, which is most of them — and asking for an engine
	// first would refuse the command on exactly the machine somebody is
	// setting up.
	if len(args) > 0 {
		switch args[0] {
		case "enable", "disable":
			return cmdHostService(args[0], args[1:])
		}
	}
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

// cmdHostService turns the host on or off through the machine's own service
// manager.
//
// The wrapper prints every command before it runs it, so somebody on a machine
// where this fails can finish by hand — and so the next machine does not need
// this at all. See `internal/service`.
func cmdHostService(verb string, rest []string) int {
	// **The unit, on stdout, for anybody who would rather install it
	// themselves** — and for the smoke check, because a `go:embed` of the
	// wrong file compiles perfectly and the only way to know which file is in
	// the binary is to ask the binary.
	if verb == "enable" && len(rest) == 1 && rest[0] == "--show-unit" {
		fmt.Print(service.Unit)
		return 0
	}
	if len(rest) > 0 {
		fmt.Fprintf(os.Stderr, "esper host %s takes no arguments\n", verb)
		return 2
	}
	fmt.Printf("🔌 esper host %s  (%s)\n\n", verb, service.Manager)
	report := service.Report{To: os.Stdout}

	var err error
	switch verb {
	case "enable":
		err = service.Enable(report)
	case "disable":
		err = service.Disable(report)
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "\nesper host %s: %v\n", verb, err)
		return 1
	}
	fmt.Println()
	if verb == "enable" {
		fmt.Println("  the host starts now and at login")
	} else {
		fmt.Println("  the host is stopped and will not start at login")
	}
	return 0
}

// cmdSessions prints what has happened on this machine.
//
// Recent, from the audit log. **Not live** — this machine's host runs as its
// own process today and nothing here can see inside it, and a list that
// silently omitted a session in progress would be worse than one that says
// which question it is answering.
// cmdAPI serves the local API until interrupted.
//
// **What a window and an adapter both need**, and the one command in this
// product that opens a listener on the machine it runs on — which is why the
// output says, every time, what is reachable and by whom.
func cmdAPI(args []string) int {
	addr := server.DefaultAddr
	exposed := false
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--listen":
			if i+1 >= len(args) {
				fmt.Fprintln(os.Stderr, "esper api: --listen wants an address, like 127.0.0.1:8687")
				return 1
			}
			addr = args[i+1]
			i++
		case "--exposed":
			exposed = true
		case "--help", "-h":
			fmt.Print(apiUsage())
			return 0
		default:
			fmt.Fprintf(os.Stderr, "esper api: %q is not an option\n\n%s", args[i], apiUsage())
			return 1
		}
	}

	home, err := esperHome()
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper api: %v\n", err)
		return 1
	}
	token, err := server.LoadOrCreateToken(home)
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper api: %v\n", err)
		return 1
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	// **The API listens even if the engine will not run.**
	//
	// Most of what it answers comes from a running engine, but not all: which
	// devices are paired is read from the store, and that is exactly the
	// answer somebody needs when nothing is connected — or when the install is
	// broken and they are working out why. An API that refused to listen
	// because the engine would not start would be a control plane that cannot
	// tell you what is wrong with it.
	//
	// Both failures are the same failure to a caller — no engine — so both
	// take the same path: say so loudly, keep serving what does not need one,
	// and report the rest as an outage rather than as zeroes.
	var asking server.Engine
	var engineTrouble error
	// Where the engine binary is, for the one thing that needs it even when
	// nothing is running: changing the device store, which only the engine
	// writes. Empty when there is none, and a change then refuses itself
	// rather than the API opening the trust store for writing.
	enginePath := ""
	sup, err := engine.New(engine.Options{Log: logToStderr})
	switch {
	case err != nil:
		engineTrouble = err
		asking = engine.Missing(err)
	default:
		enginePath = sup.Path()
		defer func() { _ = sup.Stop() }()
		asking = sup
		if started := sup.Start(
			ctx, proto.NewHello(proto.RoleController, nil, hostname(), version),
		); started != nil {
			engineTrouble = started
			asking = engine.Missing(started)
		}
	}

	api, err := server.New(server.Options{
		Addr:  addr,
		Token: token,
		// Passed rather than left to the API to resolve, so the token and the
		// device store are read out of the same home this command already
		// decided on.
		Store:        devices.PathIn(home),
		EngineBinary: enginePath,
		Engine:       asking,
		Exposed:      exposed,
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "esper api: %v\n", err)
		return 1
	}

	fmt.Println("🔌 esper api")
	fmt.Println()
	fmt.Printf("  listening  %s\n", addr)
	fmt.Printf("  token      %s/%s (mode 0600)\n", home, server.TokenFile)
	fmt.Println("  answers    what this machine can do, which devices are paired,")
	fmt.Println("             and what has happened on it")
	if engineTrouble != nil {
		fmt.Println()
		fmt.Printf("  ⚠ the engine did not start: %v\n", engineTrouble)
		fmt.Println("    Which devices are paired is still answered — it is read from the")
		fmt.Println("    store. What this machine can do, and what has happened on it, are")
		fmt.Println("    not: those come from the engine, and will report it as not running.")
	}
	if exposed {
		// **Said loudly, every time.** Somebody who exposed this deliberately
		// still deserves to see what they exposed, and somebody reading a log
		// six months later needs to know it was a decision.
		fmt.Println()
		fmt.Println("  ⚠ NOT LOOPBACK. Anything that can reach this address and read the")
		fmt.Println("    token file can ask this machine those questions. The token is the")
		fmt.Println("    only thing between them and the answers.")
	} else {
		fmt.Println("  reachable  from this machine only, by whatever can read the token")
	}
	fmt.Println()

	if err := api.Serve(ctx); err != nil {
		fmt.Fprintf(os.Stderr, "esper api: %v\n", err)
		return 1
	}
	fmt.Println("  · stopped")
	return 0
}

// esperHome is where this machine's Esper state lives.
//
// The rule itself lives in `internal/home`, because two answers to "where is
// the token" is one too many and the device store has to reach the same one.
func esperHome() (string, error) { return home.Dir() }

func apiUsage() string {
	return "usage: esper api [--listen <addr>] [--exposed]\n\n" +
		"  --listen <addr>   where to listen. Default " + server.DefaultAddr + ",\n" +
		"                    which only this machine can reach\n" +
		"  --exposed         required to bind anywhere but loopback, and says\n" +
		"                    you know what that exposes\n"
}

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
	if h.Device == "" {
		// A prefix, not a cut name. Eight characters of a fingerprint is how
		// this product names a device everywhere, and a mark after them would
		// say the *key* was too long rather than that a name is missing.
		return fmt.Sprintf("%-*s", whoWide, firstRunes(h.Key, 8))
	}
	return fmt.Sprintf("%-*s", whoWide, clip(h.Device, whoWide))
}

// How wide the name column is, in characters.
const whoWide = 14

/*
clip shortens a name to fit a column, by characters and not by bytes.

A device name is text somebody chose, and this one is printed in a fixed
column, so long ones have to be cut. Cutting at a **byte** offset cuts a
multi-byte character in half: a machine called デスクトップ came out of this
column as `デスクト\xe3\x83`, which is two replacement characters on any
terminal and reads as a broken program rather than as a long name.

The tail is marked, because a name silently losing its end is a name somebody
reads as the whole one — the same rule the window's own layout gate enforces
for text it clips. `%-*s` then pads to width, which `fmt` counts in characters
too, so the two halves agree.

What this does **not** do is measure display width: a character like デ takes
two columns in a terminal and one here, so a name in a script like that still
runs wider than the column. Fixing it needs an East Asian width table, which
is a dependency and a maintenance burden for one column of one subcommand —
and unlike the mojibake above, a wide name is still legible.
*/
func clip(s string, wide int) string {
	if utf8.RuneCountInString(s) <= wide {
		return s
	}
	// One character of the budget goes to the mark, so the result is the
	// column's width and not one past it.
	return firstRunes(s, wide-1) + "…"
}

// firstRunes takes the first n characters of a string, whole ones only.
func firstRunes(s string, n int) string {
	var b strings.Builder
	for i, r := range s {
		_ = i
		if n <= 0 {
			break
		}
		b.WriteRune(r)
		n--
	}
	return b.String()
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
  host enable        start the host now and at login
  host disable       stop it, and stop it starting at login

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
