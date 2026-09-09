package server

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"

	"github.com/elythi0n/esper/internal/settings"
)

/*
config answers with every setting, what it is now, and where that came from.

# Why the whole registry and not the values

Because a screen has to draw a control per setting, and what control depends on
the kind: a switch for a flag, a list for a choice, a number with its bounds.
Sending only the values would make the screen declare all of that a second
time, and a bound tightened in the engine and not in the page is a screen
offering a value the file will not take.

The source travels too. A value the *environment* decides is one the file
cannot change, and a screen that let somebody edit it would be a screen whose
changes silently do nothing.
*/
func (s *Server) config(w http.ResponseWriter, r *http.Request) {
	ctx, cancel := context.WithTimeout(r.Context(), howLongAChangeMayTake)
	defer cancel()

	answer, err := s.settings.Read(ctx)
	if err != nil {
		// **Not an empty list.** A settings screen that looks empty invites
		// somebody to set everything again rather than to look at what is
		// wrong, so the failure says what the engine said.
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, answer)
}

/*
setSetting changes one setting, or takes it back out of the file.

# What it refuses, and why that is not a gap

Six settings may only be changed at the machine — see `internal/settings`. Each
one either removes the ability of whoever is in front of the screen to know
what is happening to it, or widens what a paired device may reach without
anybody granting it anything. A request arriving over a socket has nobody
standing at the machine, so it is answered with the command to run there.

Everything else is offered, because the alternative is a settings screen that
sends people to a terminal for `host.fps`.
*/
func (s *Server) setSetting(w http.ResponseWriter, r *http.Request) {
	var asked struct {
		Key string `json:"key"`
		// The value, as the file would write it — which is what `raw` in the
		// answer above is, so a caller can send back what it was given.
		Value string `json:"value"`
		// Take the setting out of the file instead, so the default decides.
		// A field rather than an empty value: "" is a real value for the
		// settings whose kind is text.
		Unset bool `json:"unset"`
	}
	decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&asked); err != nil {
		http.Error(w, "the body should be {\"key\": \"host.fps\", \"value\": \"45\"}: "+err.Error(),
			http.StatusBadRequest)
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), howLongAChangeMayTake)
	defer cancel()

	var err error
	if asked.Unset {
		err = s.settings.Unset(ctx, asked.Key)
	} else {
		err = s.settings.Set(ctx, asked.Key, asked.Value)
	}
	switch {
	case err == nil:
	case errors.Is(err, settings.ErrHostOnly):
		// **403 rather than 400.** The request is well formed and understood;
		// what it lacks is somebody standing at the machine, and a client told
		// "bad request" would try to fix the request.
		http.Error(w, err.Error(), http.StatusForbidden)
		return
	case errors.Is(err, settings.ErrNoSuchSetting):
		http.Error(w, err.Error(), http.StatusNotFound)
		return
	default:
		// Whatever the engine said, which is the half that names the bound or
		// the words this value was not one of.
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}

	// **Read back rather than assembled from the request.** A caller told
	// "host.fps is 45" by the thing it just asked has learnt nothing; one
	// shown what the engine now reports has learnt whether it worked — and
	// whether something else still decides.
	answer, err := s.settings.Read(ctx)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	writeJSON(w, answer)
}
