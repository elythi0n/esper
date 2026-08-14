//go:build !linux

package service

import "fmt"

// Manager is what this platform's service manager is called, for a message.
const Manager = "no service manager"

// Enable refuses, naming what this platform would need.
//
// **A refusal rather than a guess.** launchd and the Windows service manager
// are not systemd with different words: launchd wants a property list in
// `~/Library/LaunchAgents` and a `launchctl bootstrap`, and Windows wants a
// service registered with the control manager, which needs privilege the rest
// of this product deliberately does not have. Each is a decision about how the
// host is started on that platform, and each arrives with that platform's host.
func Enable(r Report) error {
	return unsupported()
}

// Disable refuses, for the same reason.
func Disable(r Report) error {
	return unsupported()
}

func unsupported() error {
	return fmt.Errorf(
		"%w. `esper host` is the command to run, and whatever starts programs "+
			"at login on this machine is what should run it",
		ErrUnsupported,
	)
}
