package main

import (
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
)

// onStderr runs f with stderr captured, and gives back what it wrote.
func onStderr(t *testing.T, f func()) string {
	t.Helper()
	was := os.Stderr
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("a pipe: %v", err)
	}
	os.Stderr = w
	done := make(chan string, 1)
	go func() {
		var b strings.Builder
		_, _ = io.Copy(&b, r)
		done <- b.String()
	}()
	f()
	_ = w.Close()
	os.Stderr = was
	return <-done
}

/*
The window says when it was ready, once.

Nothing on this side knows when the window was usable: the process is up long
before the window is mapped, and the window is mapped before anything is on it.
The page has just painted, so the page says so — and the shell turns that into a
duration from `exec`.

The `sync.Once` is the whole of it. A page reloads: the dev server restarts, a
session window navigates, somebody presses the shortcut. Reporting on every
paint would print a line each time, and every line after the first measures the
time since *the process started*, not since anything a person waited for. The
number stops meaning what it says and nothing about it looks wrong.
*/
func TestTheWindowSaysItIsReadyExactlyOnce(t *testing.T) {
	var paint firstPaint
	mux := http.NewServeMux()
	paint.route(mux)

	post := func(body string) *httptest.ResponseRecorder {
		w := httptest.NewRecorder()
		mux.ServeHTTP(w, httptest.NewRequest(http.MethodPost, "/shell/painted", strings.NewReader(body)))
		return w
	}

	said := onStderr(t, func() {
		if w := post(`{"screen":"devices","reduced":true,"base":"0.01ms","theme":"dark"}`); w.Code != http.StatusNoContent {
			t.Errorf("the first paint answered %d", w.Code)
		}
		// Three more paints: a reload, a navigation, a second window.
		for range 3 {
			if w := post(`{"screen":"sessions"}`); w.Code != http.StatusNoContent {
				t.Errorf("a later paint answered %d", w.Code)
			}
		}
	})

	if n := strings.Count(said, "window ready in"); n != 1 {
		t.Errorf("four paints reported %d times, and the first is the only one that measures anything:\n%s", n, said)
	}
	// What it says is what the page told it, so the line can be read without
	// the page beside it.
	for _, part := range []string{"devices", "motion reduced", "--dur-base 0.01ms", "theme dark"} {
		if !strings.Contains(said, part) {
			t.Errorf("the line does not carry %q:\n%s", part, said)
		}
	}
	// And not the second paint's screen, which is the tell for a Once that is
	// not doing its job.
	if strings.Contains(said, "sessions") {
		t.Errorf("a later paint's screen reached the line:\n%s", said)
	}
}

/*
A body is a courtesy; the timing is the point.

The page sends what it knows, and a page that sent nothing — an older bundle, a
request cut short — still gets a 204 and still produces a line, because the
duration is the thing being measured and it does not depend on the body.
*/
func TestAPaintWithNothingToSayIsStillAPaint(t *testing.T) {
	var paint firstPaint
	mux := http.NewServeMux()
	paint.route(mux)

	said := onStderr(t, func() {
		w := httptest.NewRecorder()
		mux.ServeHTTP(w, httptest.NewRequest(http.MethodPost, "/shell/painted", nil))
		if w.Code != http.StatusNoContent {
			t.Errorf("a paint with no body answered %d", w.Code)
		}
	})
	if !strings.Contains(said, "window ready in") {
		t.Errorf("a paint with no body reported nothing:\n%s", said)
	}
	if !strings.Contains(said, "a screen") {
		t.Errorf("a paint that did not name a screen should say so plainly:\n%s", said)
	}
}
