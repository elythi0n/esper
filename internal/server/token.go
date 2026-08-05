// Package server is the local API: what a dashboard, a desktop app or an MCP
// adapter asks this machine about itself.
//
// # Why a listener at all, when there is a CLI
//
// Because a window cannot shell out for every frame of its own state. The
// desktop app needs to know when a session opens without polling a command,
// and an MCP adapter needs a stable surface rather than screen-scraped output.
// Both are local, and both want the same three answers this serves.
//
// # Why the security is the interesting part
//
// A listener on a person's own machine is not a small thing. Three properties
// hold whatever else changes here:
//
//  1. **Loopback unless somebody says otherwise, out loud.** Binding anywhere
//     else takes an explicit address and prints what it has just exposed.
//  2. **A token on every request.** "It is only listening on localhost" is not
//     access control: every process and every user on the machine can reach
//     127.0.0.1, and on a shared machine that is everybody.
//  3. **A `Host` a browser cannot forge.** A page in a browser can make
//     requests to 127.0.0.1, and DNS rebinding lets it do so with its own
//     origin — so a name that resolves to loopback is refused, and only the
//     literal address this server bound is accepted.
//
// Together those mean the answer to "what can reach this" is: a program on
// this machine that can read the token file, which is the same set of programs
// that could read the session log directly.
package server

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// TokenFile is where the token lives, under the Esper home.
const TokenFile = "api.token"

// tokenBytes is how much randomness a token carries.
//
// Thirty-two bytes, which is what everything else in this product uses for a
// secret: enough that guessing is not a strategy, and the same number a reader
// of `esper-crypto` would expect to see.
const tokenBytes = 32

// Token is the shared secret a caller has to present.
type Token struct {
	value string
}

// String returns the token as a caller would send it.
//
// **Not for logging.** Nothing in this package prints it, and a caller that
// does has put a credential in a file somebody else can read.
func (t Token) String() string { return t.value }

// Matches says whether a presented token is this one, in constant time.
//
// Constant time because the comparison is against a secret and the caller is
// whoever can reach the socket: a byte-by-byte compare that returns early
// leaks the token's prefix to anything that can time it.
func (t Token) Matches(presented string) bool {
	return subtle.ConstantTimeCompare([]byte(t.value), []byte(presented)) == 1
}

// LoadOrCreateToken reads the token under home, making one if there is none.
//
// The file is 0600 and is created with those permissions rather than chmodded
// afterwards: between the create and the chmod is a window in which another
// user can open it, and a window is all it takes.
//
// A file that exists with wider permissions is an error rather than a warning.
// A token any user on the machine can read is not a token, and carrying on
// with one would make this package's promise false while appearing to keep it.
func LoadOrCreateToken(home string) (Token, error) {
	path := filepath.Join(home, TokenFile)
	existing, err := os.ReadFile(path)
	switch {
	case err == nil:
		if problem := permissionsAreTight(path); problem != nil {
			return Token{}, problem
		}
		value := strings.TrimSpace(string(existing))
		if value == "" {
			return Token{}, fmt.Errorf("the token file at %s is empty; delete it and it will be made again", path)
		}
		return Token{value: value}, nil
	case errors.Is(err, fs.ErrNotExist):
		return createToken(path)
	default:
		return Token{}, fmt.Errorf("reading the API token at %s: %w", path, err)
	}
}

func createToken(path string) (Token, error) {
	raw := make([]byte, tokenBytes)
	if _, err := rand.Read(raw); err != nil {
		return Token{}, fmt.Errorf("making an API token: %w", err)
	}
	value := base64.RawURLEncoding.EncodeToString(raw)
	// O_EXCL so two processes starting at once cannot each believe they made
	// it, and 0600 at creation for the reason above.
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		if errors.Is(err, fs.ErrExist) {
			// Somebody else won the race. Theirs is as good as ours.
			return LoadOrCreateToken(filepath.Dir(path))
		}
		return Token{}, fmt.Errorf("writing the API token to %s: %w", path, err)
	}
	if _, err := file.WriteString(value + "\n"); err != nil {
		_ = file.Close()
		return Token{}, fmt.Errorf("writing the API token to %s: %w", path, err)
	}
	// **The close is checked.** A buffered write that fails only on close is a
	// token the next start will not find, and a server that then refuses every
	// request from a caller holding the token it was just given.
	if err := file.Close(); err != nil {
		return Token{}, fmt.Errorf("finishing the API token at %s: %w", path, err)
	}
	return Token{value: value}, nil
}

// permissionsAreTight refuses a token file others can read.
func permissionsAreTight(path string) error {
	info, err := os.Stat(path)
	if err != nil {
		return fmt.Errorf("checking the API token at %s: %w", path, err)
	}
	if mode := info.Mode().Perm(); mode&0o077 != 0 {
		return fmt.Errorf(
			"the API token at %s is readable by others (mode %04o): "+
				"a token anybody on this machine can read is not a token. "+
				"Run chmod 600 on it, or delete it and it will be made again",
			path, mode,
		)
	}
	return nil
}
