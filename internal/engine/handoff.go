package engine

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

// Locate finds a binary that ships beside this one, preferring the directory
// holding the current executable so a development build never runs an
// installed binary of a different version.
//
// The same rule as the engine's own lookup, exported because `esper broker`
// hands off to a *third* binary that follows it too.
func Locate(name string) (string, error) {
	if self, err := os.Executable(); err == nil {
		beside := filepath.Join(filepath.Dir(self), name+exeSuffix())
		if isExecutable(beside) {
			return beside, nil
		}
	}
	if p, err := exec.LookPath(name + exeSuffix()); err == nil {
		return p, nil
	}
	return "", fmt.Errorf("%w: no %s beside %s or on PATH", ErrNotFound, name, selfDir())
}

// selfDir is where this binary lives, for an error message that says where we
// looked rather than only that we failed.
func selfDir() string {
	self, err := os.Executable()
	if err != nil {
		return "this program"
	}
	return filepath.Dir(self)
}
