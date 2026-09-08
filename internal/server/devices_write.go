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
	if already == nil && what != devices.Forget && what != devices.Keep {
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

	ctx, cancel := context.WithTimeout(r.Context(), howLongAChangeMayTake)
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
