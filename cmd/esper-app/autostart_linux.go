package main

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
)

// autostartEntry is the file every XDG desktop reads at login.
const autostartEntry = "esper.desktop"

// autostartDir is where it goes: the XDG config home, else the default.
func autostartDir() (string, error) {
	config := os.Getenv("XDG_CONFIG_HOME")
	if config == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", fmt.Errorf("finding your config directory: %w", err)
		}
		config = filepath.Join(home, ".config")
	}
	return filepath.Join(config, "autostart"), nil
}

func autostartNow() autostartState {
	dir, err := autostartDir()
	if err != nil {
		return autostartState{Why: err.Error()}
	}
	path := filepath.Join(dir, autostartEntry)
	_, statErr := os.Stat(path)
	return autostartState{On: statErr == nil, Available: true, Path: path}
}

// setAutostart writes or removes the entry.
//
// **The entry names this executable by its absolute path.** A development
// build and an installed one are different programs, and an entry that said
// `esper-app` would start whichever the login shell's PATH happened to find —
// which is how somebody ends up debugging why their changes are not running.
func setAutostart(on bool) error {
	dir, err := autostartDir()
	if err != nil {
		return err
	}
	path := filepath.Join(dir, autostartEntry)

	if !on {
		if err := os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
			return fmt.Errorf("removing %s: %w", path, err)
		}
		return nil
	}

	self, err := os.Executable()
	if err != nil {
		return fmt.Errorf("finding this program: %w", err)
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return fmt.Errorf("making %s: %w", dir, err)
	}

	// `%u` so a login-launched window can still be handed an `esper://` link
	// by whatever launches it, and the same entry serves both purposes.
	entry := "[Desktop Entry]\n" +
		"Type=Application\n" +
		"Name=Esper\n" +
		"Comment=Secure remote desktop and terminal, with no operated infrastructure\n" +
		"Exec=" + self + " %u\n" +
		"Icon=esper\n" +
		"Terminal=false\n" +
		"X-GNOME-Autostart-enabled=true\n"
	if err := os.WriteFile(path, []byte(entry), 0o644); err != nil {
		return fmt.Errorf("writing %s: %w", path, err)
	}
	return nil
}
