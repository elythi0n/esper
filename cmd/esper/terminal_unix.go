//go:build linux || darwin || freebsd || netbsd || openbsd || dragonfly

package main

import "golang.org/x/sys/unix"

// isATerminal asks the terminal driver whether a descriptor has a terminal
// behind it. The error *is* the answer: no termios, no terminal.
func isATerminal(fd uintptr) bool {
	_, err := unix.IoctlGetTermios(int(fd), termiosRequest)
	return err == nil
}
