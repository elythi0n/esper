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
	"os"
	"path/filepath"
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
	default:
		fmt.Fprintf(os.Stderr, "esper: unknown command %q\n\n", args[0])
		printHelp()
		return 1
	}
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
	sup, err := engine.New(engine.Options{
		Log: func(format string, args ...any) {
			fmt.Fprintf(os.Stderr, format+"\n", args...)
		},
	})
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

Commands:
  version [--json]   print the release, build time and protocol version
  doctor             check the engine is present and the seam works
  help               this text

Esper operates no infrastructure. There is no account, no password, and no
default server: devices pair once with a short code and know each other by
public key thereafter.
`)
}
