//go:build !linux

package engine

import (
	"fmt"
	"net"
)

// whoConnected cannot say on this platform.
//
// Every Unix has an answer and no two agree on the spelling: macOS and the
// BSDs have `LOCAL_PEERCRED` and `getpeereid`, and neither is in Go's syscall
// package as anything portable. Rather than guess, this says it does not know
// and the caller says so out loud — the private directory is still guarding
// the socket, and a check that silently did nothing would be worse than an
// absent one.
func whoConnected(net.Conn) (int, error) {
	return 0, fmt.Errorf("%w: no portable way to ask on this platform", errPeerUnknowable)
}
