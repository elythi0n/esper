//go:build !windows

package engine

import (
	"fmt"
	"os"
	"syscall"
)

// Handoff replaces this process with another program, running the given
// arguments.
//
// **Replaces, rather than spawns and waits.** A session verb owns a terminal:
// `terminal` puts it in raw mode, `forward --stdio` hands its stdin and stdout
// to somebody's ssh connection, and every one of them wants signals, job
// control and the exit status to behave exactly as if the user had run that
// program themselves. A parent process in the middle has to forward all of
// that correctly, and will eventually forward some of it wrongly — Ctrl-C
// reaching one of the two and not the other is the classic result.
//
// It does not return on success, because there is nothing left to return to.
// The int in the signature is for Windows, which cannot do this.
func Handoff(path string, args []string) (int, error) {
	argv := append([]string{path}, args...)
	if err := syscall.Exec(path, argv, os.Environ()); err != nil {
		return 1, fmt.Errorf("running %s: %w", path, err)
	}
	// Unreachable: a successful Exec does not return.
	return 0, nil
}
