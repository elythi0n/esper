//go:build !linux

package main

import "errors"

// errNoAutostart is what a platform with nowhere to put an entry says, and
// the one place that sentence is written.
var errNoAutostart = errors.New("opening at login is not built on this platform yet")

// Autostart on Windows and macOS is not this file's to guess at.
//
// Windows means a registry Run key or the Startup folder, and macOS means a
// launch agent — and on macOS a launch agent is the same mechanism
// unattended access will need, with permissions and a signature attached to
// it. Writing either badly is worse than not offering the switch: an entry
// that points at a moved binary is a login-time error dialog every morning.
//
// So the switch is not drawn on those platforms, and the reason is said
// rather than the switch being drawn and refusing.
func autostartNow() autostartState {
	return autostartState{
		Available: false,
		Why:       errNoAutostart.Error(),
	}
}

func setAutostart(bool) error {
	return errNoAutostart
}
