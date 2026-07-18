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
	sup, err := engine.New(engine.Options{})
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
	fmt.Println("✓ ready")
	return 0
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
