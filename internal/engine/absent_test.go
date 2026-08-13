package engine

import (
	"context"
	"errors"
	"strings"
	"testing"
)

// TestAnAbsentEngineReportsTheOutageAndNeverAZeroValue is the whole point of
// this type. The defect it guards against is somebody helpfully returning an
// empty slice: "nothing has happened on this machine" and "nothing can tell
// you what happened on this machine" are different answers, and a diagnostic
// screen full of zeroes reads as a measurement.
func TestAnAbsentEngineReportsTheOutageAndNeverAZeroValue(t *testing.T) {
	why := errors.New("looked in [/opt/esper/esper-engine $PATH]")
	absent := Missing(why)

	if absent.Running() {
		t.Error("an engine that is not there is not running")
	}
	if !errors.Is(absent.Why(), why) {
		t.Error("the reason it is not there should survive")
	}

	status, err := absent.Status(context.Background())
	if err == nil {
		t.Error("status must report the outage rather than answer")
	}
	if status != nil {
		t.Errorf("status must not be a zero value, got %+v", status)
	}

	happenings, err := absent.Sessions(context.Background(), 20)
	if err == nil {
		t.Error("sessions must report the outage rather than answer")
	}
	if happenings != nil {
		t.Errorf("sessions must not be an empty list, got %v", happenings)
	}
}

// TestTheOutageNamesWhyItHappened: somebody reading `502 the engine is not
// running` needs the next sentence, or they go looking in the wrong place.
func TestTheOutageNamesWhyItHappened(t *testing.T) {
	absent := Missing(errors.New("a partial install"))
	_, err := absent.Status(context.Background())
	if err == nil {
		t.Fatal("expected an error")
	}
	if !strings.Contains(err.Error(), "a partial install") {
		t.Errorf("the reason should be carried through, got %q", err)
	}
	if !strings.Contains(err.Error(), "not running") {
		t.Errorf("the answer should say the engine is not running, got %q", err)
	}
}

// TestAnAbsentEngineSatisfiesTheInterfaceTheAPIAsks is what makes the local API
// able to listen at all when the engine will not start. A compile-time check,
// because the failure it prevents is the API refusing to serve a device list
// that does not need an engine.
func TestAnAbsentEngineSatisfiesTheInterfaceTheAPIAsks(t *testing.T) {
	var asking interface {
		Running() bool
	} = Missing(errors.New("nothing there"))
	if asking.Running() {
		t.Error("still not running")
	}
}
