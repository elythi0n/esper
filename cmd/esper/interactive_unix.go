//go:build linux

package main

import "golang.org/x/sys/unix"

// termiosRequest is what "tell me about this terminal" is called on Linux.
//
// Per-platform because the number is: macOS and the BSDs call it `TIOCGETA`.
// Named here rather than inlined so adding a platform is one file and not a
// search for a constant.
const termiosRequest = unix.TCGETS
