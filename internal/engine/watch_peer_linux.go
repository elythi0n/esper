//go:build linux

package engine

import (
	"errors"
	"fmt"
	"net"
	"syscall"
)

// whoConnected returns the pid of the process at the other end of a Unix
// socket.
//
// The kernel is the one answering, which is the point: a process cannot claim
// somebody else's pid, so this is a check rather than a courtesy. The
// alternative — trusting whoever connects — would let any program belonging to
// the same person answer a supervisor's health check and hand it an extract of
// the audit trail.
func whoConnected(c net.Conn) (int, error) {
	unix, ok := c.(*net.UnixConn)
	if !ok {
		return 0, fmt.Errorf("%w: not a unix socket", errPeerUnknowable)
	}
	raw, err := unix.SyscallConn()
	if err != nil {
		return 0, fmt.Errorf("reaching the connection: %w", err)
	}
	var cred *syscall.Ucred
	var credErr error
	if err := raw.Control(func(fd uintptr) {
		cred, credErr = syscall.GetsockoptUcred(int(fd), syscall.SOL_SOCKET, syscall.SO_PEERCRED)
	}); err != nil {
		return 0, fmt.Errorf("reaching the connection: %w", err)
	}
	if credErr != nil {
		return 0, fmt.Errorf("asking who connected: %w", credErr)
	}
	if cred == nil {
		return 0, errors.New("the kernel said nothing about who connected")
	}
	return int(cred.Pid), nil
}
