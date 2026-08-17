package main

import (
	"encoding/json"
	"net/http"
)

// Autostart is whether this window opens when somebody logs in.
//
// # Why it is a file and not a setting
//
// Every desktop already has a place that means "start this at login", and it
// is the place a person can see and remove without this application's help.
// Writing our own preference and honouring it ourselves would put the truth
// somewhere only Esper can read — and would not work, because nothing would
// launch us to read it.
//
// So the answer to "is autostart on" is "does that file exist", asked every
// time. There is no state here to fall out of step.

// autostartState is what the page needs to draw the switch.
type autostartState struct {
	// On is whether the entry is there now.
	On bool `json:"on"`
	// Available is whether this platform has somewhere to put one. False is
	// not a failure: it is a switch the page should not draw.
	Available bool `json:"available"`
	// Where it lives, for a person who would rather look themselves.
	Path string `json:"path,omitempty"`
	// Why, when it is unavailable.
	Why string `json:"why,omitempty"`
}

// autostartRoutes reads and writes it.
func autostartRoutes(mux *http.ServeMux) {
	mux.HandleFunc("GET /shell/autostart", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, autostartNow())
	})

	mux.HandleFunc("POST /shell/autostart", func(w http.ResponseWriter, r *http.Request) {
		var asked struct {
			On bool `json:"on"`
		}
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1024)).Decode(&asked); err != nil {
			http.Error(w, `the body should be {"on": true}`, http.StatusBadRequest)
			return
		}
		if err := setAutostart(asked.On); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		// The new state, read back from the same place the switch reads it,
		// so the page never shows a value nothing on disk agrees with.
		writeJSON(w, autostartNow())
	})
}
