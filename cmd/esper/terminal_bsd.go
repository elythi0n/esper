//go:build darwin || freebsd || netbsd || openbsd || dragonfly

package main

import "golang.org/x/sys/unix"

// termiosRequest is what "tell me about this terminal" is called on macOS and
// the BSDs. Linux calls the same question `TCGETS`.
const termiosRequest = unix.TIOCGETA
