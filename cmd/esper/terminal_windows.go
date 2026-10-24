//go:build windows

package main

import "golang.org/x/sys/windows"

// isATerminal asks whether a handle is a console. Windows has no termios; a
// handle has a console mode exactly when there is a console behind it, and
// `NUL` — the `/dev/null` of the trap this guards against — has none.
func isATerminal(fd uintptr) bool {
	var mode uint32
	return windows.GetConsoleMode(windows.Handle(fd), &mode) == nil
}
