package server

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"time"

	"github.com/elythi0n/esper/internal/devices"
)

// howLongAChangeMayTake bounds the engine's own run.
//
// Generous, because the engine reads and rewrites the store atomically and a
// slow disk is not a fault — and bounded at all, because a request that never
// answers holds a connection and tells nobody anything.
const howLongAChangeMayTake = 20 * time.Second

// howLongReachingMayTake bounds an action against another machine.
//
// Finding a host, opening a session and being answered is a network round
// trip and several handshakes, not a file write. Still bounded: a request that
// never answers holds a connection and tells nobody anything, and a host that
// is not there has to become a sentence rather than a wait.
const howLongReachingMayTake = 90 * time.Second

// whatToChange is the body of a grant or a revoke.
type whatToChange struct {
	Permissions []devices.Permission `json:"permissions"`
}

func (s *Server) grant(w http.ResponseWriter, r *http.Request) {
	s.change(w, r, devices.Grant)
}

func (s *Server) revoke(w http.ResponseWriter, r *http.Request) {
	s.change(w, r, devices.Revoke)
}

func (s *Server) forget(w http.ResponseWriter, r *http.Request) {
	s.change(w, r, devices.Forget)
}

/*
scheduleDevice bounds when a device's unattended access applies.

The engine has had the verb since Phase 14 and no route reached it, so a
device's hours could be *seen* in the app and changed only in a terminal —
which is the thing this window exists to stop being necessary.

**Not gated the way a grant is.** `unattended` itself is granted at the
machine and stays that way; this narrows the hours inside which it counts, and
every schedule is a subset of the `always` a bare grant means. There is no
version of this that gives a device more than it already had.
*/
func (s *Server) scheduleDevice(w http.ResponseWriter, r *http.Request) {
	var asked struct {
		// The engine's own vocabulary, passed through: `weekdays 08:00-18:00`,
		// `daily 22:00-06:00`, `always`, `clear`. A second grammar here would
		// be a second thing to keep in step with the parser that decides.
		When string `json:"when"`
	}
	decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&asked); err != nil {
		http.Error(w, "the body should be {\"when\": \"weekdays 08:00-18:00\"}: "+err.Error(),
			http.StatusBadRequest)
		return
	}
	s.changeWith(w, r, devices.Schedule, []devices.Permission{devices.Permission(asked.When)})
}

/*
lockAfterDevice sets what a session with one device leaves the screen as.

The host-wide setting is in `esper config` and applies to everything; this is
the per-device answer for the machine it is wrong about — a laptop connecting
to its owner's own desktop should not lock it under them.

This is the only route here that can leave a machine *less* protected, since
`never` is a real answer and the host's own setting may be `always`. It is
offered for the same reason `revoke` and `forget` are: the control plane
answers a loopback caller holding a token no page can read, which is to say
somebody already at this machine.
*/
func (s *Server) lockAfterDevice(w http.ResponseWriter, r *http.Request) {
	var asked struct {
		// One of `always`, `never`, `unattended`, or `default` to take the
		// per-device answer off and follow the host again.
		After string `json:"after"`
	}
	decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&asked); err != nil {
		http.Error(w, "the body should be {\"after\": \"always\"}: "+err.Error(),
			http.StatusBadRequest)
		return
	}
	s.changeWith(w, r, devices.LockAfterChange, []devices.Permission{devices.Permission(asked.After)})
}

/*
wakeDevice sends a magic packet to a device asleep on this network.

**Nothing answers a magic packet.** There is no reply to wait for and no way
to tell a machine that woke from one that never heard, so a 200 here means the
packet left this machine and nothing more. Whatever says so to a person has to
say that much, or the window is claiming something it cannot know.

The engine's own `--to <address>` is not passed on: it exists for a segment
this machine is not on, and a caller that could name where a packet goes is a
caller using this machine to send packets somewhere of its choosing. The
broadcast address is the only destination a request can reach.
*/
func (s *Server) wakeDevice(w http.ResponseWriter, r *http.Request) {
	s.change(w, r, devices.Wake)
}

/*
Three actions on a paired host, each behind its own permission at that end.

# Why there is nothing for this end to gate

These are the only routes here that reach *outward*. What they may do is
decided at the other machine: `lock`, `logoff` and `reboot` are permissions
that host granted this one, and the two consequential ones needed confirming
in front of it. So this end cannot know in advance whether an action will be
allowed, and pretending to would mean keeping a copy of somebody else's trust
store.

It asks, and what comes back is the host's own sentence — which names the
permission that is missing, and is the thing to do something about.

# Why they are not one route with an action in the body

Because a route is the thing `TestEveryRouteRefusesARequestWithNoToken`
enumerates, and three verbs behind one path are one route to that test and
three to an attacker. Three paths is three rows in the guard's own list.
*/
func (s *Server) lockHost(w http.ResponseWriter, r *http.Request) {
	s.change(w, r, devices.LockScreen)
}

func (s *Server) logOffHost(w http.ResponseWriter, r *http.Request) {
	s.change(w, r, devices.LogOff)
}

// restartHost is the most consequential of the three: the effect outlives the
// session that asked for it, and nothing answers a restart because the machine
// is gone before it could.
func (s *Server) restartHost(w http.ResponseWriter, r *http.Request) {
	s.change(w, r, devices.Restart)
}

/*
groupDevice puts a device in a named group, or takes it out of one.

A group is an address rather than a grant: `esper grant --group ops control`
resolves the members and writes the permission onto each, so membership by
itself gives a device nothing. That is why this is not gated the way a grant
is — what it changes is who a later command is about.

The engine has had the verb since groups existed and nothing reached it, so
the device panel could show a device's groups and not change one.
*/
func (s *Server) groupDevice(w http.ResponseWriter, r *http.Request) {
	var asked struct {
		// The group's name. Letters, digits, `-` and `_`.
		Name string `json:"name"`
		// Take the device out of it instead of putting it in.
		Remove bool `json:"remove"`
	}
	decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&asked); err != nil {
		http.Error(w, "the body should be {\"name\": \"ops\"}: "+err.Error(), http.StatusBadRequest)
		return
	}
	what := devices.Group
	if asked.Remove {
		what = devices.Ungroup
	}
	s.changeWith(w, r, what, []devices.Permission{devices.Permission(asked.Name)})
}

// keepDevice turns a one-session pairing into a lasting one.
//
// The Devices screen has shown `once` and `spent` since it was written and
// could do nothing about either — the only way to make a pairing last was
// `esper keep` in a terminal, which is the thing this window exists to stop
// being necessary.
func (s *Server) keepDevice(w http.ResponseWriter, r *http.Request) {
	s.change(w, r, devices.Keep)
}

// change asks the engine to change one device, then reports the device as the
// store now has it.
//
// **The answer is read back from the file rather than assembled from the
// request.** A caller told "granted: view, control" by the thing it just asked
// has learnt nothing; a caller shown what the store says has learnt whether it
// worked. It is also the only way a partial change would be visible.
func (s *Server) change(w http.ResponseWriter, r *http.Request, what devices.Change) {
	s.changeWith(w, r, what, nil)
}

// changeWith is `change` for a caller that has already read the body.
//
// A schedule's body is a `when` rather than a list of permissions, and the
// alternative to this seam was a second copy of everything below it: the
// timeout, the four ways it can fail, and reading the device back from the
// store rather than describing what was asked for.
func (s *Server) changeWith(
	w http.ResponseWriter,
	r *http.Request,
	what devices.Change,
	already []devices.Permission,
) {
	fingerprint := r.PathValue("fingerprint")

	wanted := whatToChange{Permissions: already}
	if already == nil && what != devices.Forget && what != devices.Keep && !what.Reaching() {
		// A field this does not know is a request this does not understand.
		// Without it, `{"permissons": [...]}` decoded to an empty list, the
		// engine refused with "needs at least one permission", and the caller
		// was told the *engine* had a problem with a body the server had not
		// read properly.
		decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096))
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&wanted); err != nil {
			http.Error(w, "the body should be {\"permissions\": [...]}: "+err.Error(),
				http.StatusBadRequest)
			return
		}
	}

	// **Longer for the ones that reach another machine.** A store write is a
	// file; an action on a paired host is finding it, opening a session and
	// being answered — or not, in the case of a restart. A twenty-second
	// bound would report a working host as a failure on a slow network.
	patience := howLongAChangeMayTake
	if what.Reaching() {
		patience = howLongReachingMayTake
	}
	ctx, cancel := context.WithTimeout(r.Context(), patience)
	defer cancel()

	err := s.writer.Change(ctx, what, fingerprint, wanted.Permissions)
	switch {
	case err == nil:
	case errors.Is(err, devices.ErrHostOnly):
		// **403 rather than 400.** The request is well formed and understood;
		// what it lacks is somebody standing at the machine, and a client that
		// saw "bad request" would try to fix the request.
		http.Error(w, err.Error(), http.StatusForbidden)
		return
	case errors.Is(err, devices.ErrNoSuchDevice):
		http.Error(w, err.Error(), http.StatusNotFound)
		return
	default:
		// Whatever the engine said, which is the half that says what to do
		// next.
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}

	if what == devices.Forget {
		writeJSON(w, map[string]any{
			"forgotten": fingerprint,
			"read_from": s.store,
		})
		return
	}

	list, err := devices.Load(s.store)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	now := uint64(time.Now().Unix()) //nolint:gosec // Unix seconds, after 1970
	for _, device := range list {
		if device.Fingerprint().Hex != fingerprint {
			continue
		}
		writeJSON(w, listed{
			Device:         device,
			Fingerprint:    device.Fingerprint(),
			AdmitsASession: device.Pairing.Admits(now),
		})
		return
	}
	// The engine reported success and the device is not there. Not a 200 with
	// nothing in it: something is wrong and saying so beats an empty answer.
	http.Error(w,
		"the change was made and the device is no longer in the store — "+
			"something else changed it at the same moment",
		http.StatusConflict)
}
