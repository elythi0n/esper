package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/elythi0n/esper/internal/engine"
	"github.com/elythi0n/esper/internal/home"
	"github.com/elythi0n/esper/internal/proto"
)

// The same diagnostic, in a shape a script can read.
//
// A monitoring check, a support script and a packaging test all want the
// answers `esper doctor` gathers, and all of them were parsing its prose —
// which is written for a person and therefore free to change. This is the
// contract instead: the field names are the ones the local API already uses
// for the same values, so a reader that knows one knows the other.
type doctorReport struct {
	// This program.
	Esper struct {
		Version  string `json:"version"`
		Built    string `json:"built"`
		Protocol uint16 `json:"protocol"`
	} `json:"esper"`
	// The engine beside it, and whether it could be reached at all.
	Engine doctorEngine `json:"engine"`
	// What this machine can do. Absent where the engine could not be reached.
	Machine *proto.Status `json:"machine,omitempty"`
	// Where this machine keeps its state, or why it has nowhere.
	State string `json:"state"`
	// The ladder, in the order `esper connect` walks it.
	WaysIn []rung `json:"ways_in,omitempty"`
	// Whether this machine can host a screen session.
	Ready bool `json:"ready"`
	// Everything the report would have said with a ⚠ beside it. Empty is the
	// ordinary answer.
	Warnings []string `json:"warnings"`
}

type doctorEngine struct {
	Path     string `json:"path"`
	Reached  bool   `json:"reached"`
	Version  string `json:"version,omitempty"`
	Protocol uint16 `json:"protocol,omitempty"`
	// Why it could not be reached, where it could not.
	Trouble string `json:"trouble,omitempty"`
}

// doctorAsJSON gathers the same answers as the printed report and writes them
// as one object.
//
// The exit status is the printed report's: nought when this machine can host,
// one when it cannot. A script that only wants the number never has to parse
// anything at all.
func doctorAsJSON() int {
	var report doctorReport
	report.Esper.Version = version
	report.Esper.Built = buildTime
	report.Esper.Protocol = proto.ProtocolVersion
	report.Warnings = []string{}
	report.State = whereTheStateIs()

	if others := otherBinariesOnPath("esper"); len(others) > 1 {
		report.Warnings = append(report.Warnings,
			fmt.Sprintf("more than one `esper` is on PATH (%v); the first one wins, which may not be the one you installed", others))
	}

	sup, err := engine.New(engine.Options{Log: logToStderr, ReachedAs: "esper"})
	if err != nil {
		report.Engine.Trouble = err.Error()
		report.Warnings = append(report.Warnings,
			"the engine does the realtime work; without it esper can only report its own version")
		return writeJSON(&report, 1)
	}
	report.Engine.Path = sup.Path()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	local := proto.NewHello(proto.RoleController, nil, hostname(), version)
	if err := sup.Start(ctx, local); err != nil {
		report.Engine.Trouble = err.Error()
		return writeJSON(&report, 1)
	}
	defer func() { _ = sup.Stop() }()

	h := sup.Hello()
	report.Engine.Reached = true
	report.Engine.Version = h.Engine
	report.Engine.Protocol = h.Proto

	status, err := sup.Status(ctx)
	if err != nil {
		report.Engine.Trouble = err.Error()
		return writeJSON(&report, 1)
	}
	report.Machine = status
	report.WaysIn = waysIn(status)
	report.Ready = hostReady(status)

	if onlyAnAddress(status) {
		report.Warnings = append(report.Warnings,
			"the only way in is an address somebody already knows; there is no rung left if this machine moves")
	}
	if !report.Ready {
		report.Warnings = append(report.Warnings,
			"this machine cannot host a screen session. It can still be a controller, and terminal mode and port forwarding need none of the above")
		return writeJSON(&report, 1)
	}
	return writeJSON(&report, 0)
}

// whereTheStateIs is the directory the printed report names, as one string.
func whereTheStateIs() string {
	dir, err := home.Dir()
	if err != nil {
		return ""
	}
	return dir
}

// The whole of `esper doctor --session`, as one object.
//
// The ordinary report and the session test in one answer rather than two
// commands: a script that wants the deep check wants the shallow one too, and
// running them separately would start the engine twice and report a machine
// that may have changed in between.
type doctorSessionReport struct {
	doctorReport
	Session *collecting `json:"session,omitempty"`
}

// doctorSessionAsJSON is `--session --json`: the same steps the printed test
// walks, kept rather than printed.
func doctorSessionAsJSON() int {
	var report doctorSessionReport
	report.Esper.Version = version
	report.Esper.Built = buildTime
	report.Esper.Protocol = proto.ProtocolVersion
	report.Warnings = []string{}
	report.State = whereTheStateIs()

	if others := otherBinariesOnPath("esper"); len(others) > 1 {
		report.Warnings = append(report.Warnings,
			fmt.Sprintf("more than one `esper` is on PATH (%v); the first one wins, which may not be the one you installed", others))
	}

	sup, err := engine.New(engine.Options{Log: logToStderr, ReachedAs: "esper"})
	if err != nil {
		report.Engine.Trouble = err.Error()
		return writeJSON(&report, 1)
	}
	report.Engine.Path = sup.Path()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	local := proto.NewHello(proto.RoleController, nil, hostname(), version)
	if err := sup.Start(ctx, local); err != nil {
		report.Engine.Trouble = err.Error()
		return writeJSON(&report, 1)
	}
	h := sup.Hello()
	report.Engine.Reached = true
	report.Engine.Version = h.Engine
	report.Engine.Protocol = h.Proto

	status, err := sup.Status(ctx)
	if err != nil {
		report.Engine.Trouble = err.Error()
		_ = sup.Stop()
		return writeJSON(&report, 1)
	}
	report.Machine = status
	report.WaysIn = waysIn(status)
	report.Ready = hostReady(status)
	if onlyAnAddress(status) {
		report.Warnings = append(report.Warnings,
			"the only way in is an address somebody already knows; there is no rung left if this machine moves")
	}
	if !report.Ready {
		report.Warnings = append(report.Warnings,
			"this machine cannot host a screen session. It can still be a controller, and terminal mode and port forwarding need none of the above")
		_ = sup.Stop()
		return writeJSON(&report, 1)
	}

	// The engine this process started holds the control socket; the self test
	// starts its own, so let this one go first.
	path := sup.Path()
	_ = sup.Stop()
	session := &collecting{Steps: []selfStep{}, Warnings: []string{}}
	code := selfTest(path, session)
	report.Session = session
	return writeJSON(&report, code)
}

// writeJSON prints one object and returns the status the caller asked for — or
// one, if the object could not be written at all.
func writeJSON(v any, status int) int {
	out := json.NewEncoder(os.Stdout)
	out.SetIndent("", "  ")
	if err := out.Encode(v); err != nil {
		fmt.Fprintf(os.Stderr, "esper doctor: %v\n", err)
		return 1
	}
	return status
}
