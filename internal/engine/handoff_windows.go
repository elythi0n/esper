//go:build windows

package engine

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
)

// Handoff runs another program with this process's stdio and returns its exit
// status.
//
// Windows has no `exec`: `syscall.Exec` exists and returns EWINDOWS, so a
// caller that used it would find the program had not run. This spawns and
// waits instead, which is the same shape with a process in the middle — worse
// for signals and job control, and the platform's answer rather than ours.
func Handoff(path string, args []string) (int, error) {
	cmd := exec.Command(path, args...)
	cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
	err := cmd.Run()
	if err == nil {
		return 0, nil
	}
	// An exit status is the program's answer, not a failure to run it.
	var exit *exec.ExitError
	if errors.As(err, &exit) {
		return exit.ExitCode(), nil
	}
	return 1, fmt.Errorf("running %s: %w", path, err)
}
