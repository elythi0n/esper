// Package home answers where this machine's Esper state lives.
//
// It is a package for one function because the function is a rule, and the
// rule has to have exactly one answer. The token, the device store, the trail
// and the engine's own state all sit in the same directory, and a build where
// two of them disagreed about which directory that is would look like a
// machine that had forgotten who it trusts.
package home

import (
	"fmt"
	"os"
	"path/filepath"
)

// Variable is the environment variable that overrides the default.
//
// It exists so that two Espers can run on one machine — which is how every
// end-to-end check here arranges a host and a controller — and it is honored
// by every part of the product for that reason.
const Variable = "ESPER_HOME"

// Dir is where this machine's Esper state lives: `$ESPER_HOME`, else
// `$HOME/.esper`.
//
// It is not created here. A token belongs beside a device store that pairing
// made, and a home that does not exist yet is a machine that has not been
// paired.
//
// # Errors
//
// If neither the variable nor a home directory can be found.
func Dir() (string, error) {
	if set := os.Getenv(Variable); set != "" {
		return set, nil
	}
	dir, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("finding your home directory: %w", err)
	}
	return filepath.Join(dir, ".esper"), nil
}
