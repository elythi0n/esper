package engine

import (
	"context"
	"fmt"

	"github.com/elythi0n/esper/internal/proto"
)

// Absent stands in for an engine that could not be started.
//
// # Why an engine that is not there is still an object
//
// Not every answer the control plane gives comes from the engine. Which
// devices are paired is read from the store, and that is exactly the answer
// somebody needs when nothing is connected — or when the install is broken and
// they are trying to find out why. A control plane that would not start
// because the thing it supervises would not start is a control plane that
// cannot tell you what is wrong.
//
// So the API keeps a supervisor-shaped thing that says, in full, what happened
// instead of returning a zero value. **A diagnostic screen full of zeroes
// reads as a measurement**, which is worse than one that reports an outage.
type Absent struct {
	// why is the failure this stands in for, repeated in every answer.
	why error
}

// Missing wraps the reason an engine could not be started.
func Missing(why error) *Absent { return &Absent{why: why} }

// Status reports the outage rather than an empty status.
func (a *Absent) Status(context.Context) (*proto.Status, error) {
	return nil, a.reason()
}

// Sessions reports the outage rather than an empty history.
//
// Emphatically not an empty list: "nothing has happened on this machine" and
// "nothing can tell you what happened on this machine" are different answers,
// and only one of them is reassuring.
func (a *Absent) Sessions(context.Context, uint32) ([]proto.Happening, error) {
	return nil, a.reason()
}

// Running is false, which is the one thing that can be said for certain.
func (a *Absent) Running() bool { return false }

// Why is the failure this stands in for.
func (a *Absent) Why() error { return a.why }

func (a *Absent) reason() error {
	return fmt.Errorf("the engine is not running: %w", a.why)
}
