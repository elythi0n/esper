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
	fingerprint := r.PathValue("fingerprint")

	var wanted whatToChange
	if what != devices.Forget && what != devices.Keep {
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096)).Decode(&wanted); err != nil {
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
