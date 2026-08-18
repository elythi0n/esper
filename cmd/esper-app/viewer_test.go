package main

import (
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// served starts a viewer on a temporary home and routes it.
func served(t *testing.T) (*viewer, *httptest.Server) {
	t.Helper()
	pictures, err := openViewer(t.TempDir(), false)
	if err != nil {
		t.Fatalf("opening the viewer socket: %v", err)
	}
	mux := http.NewServeMux()
	pictures.route(mux)
	server := httptest.NewServer(mux)
	t.Cleanup(func() {
		server.Close()
		pictures.close()
	})
	return pictures, server
}

func TestThePageIsToldWhereToPointASession(t *testing.T) {
	pictures, server := served(t)
	res, err := http.Get(server.URL + "/shell/video/socket")
	if err != nil {
		t.Fatalf("asking where the socket is: %v", err)
	}
	defer func() { _ = res.Body.Close() }()
	body, _ := io.ReadAll(res.Body)
	if !strings.Contains(string(body), pictures.path) {
		t.Fatalf("the window did not say where its socket is: %s", body)
	}
}

// A session's bytes reach the page, in order, without waiting for the end.
func TestPicturesReachThePageAsTheyArrive(t *testing.T) {
	pictures, server := served(t)

	res := make(chan *http.Response, 1)
	go func() {
		r, err := http.Get(server.URL + "/shell/video")
		if err != nil {
			t.Errorf("asking for the picture: %v", err)
			res <- nil
			return
		}
		res <- r
	}()

	// The engine's half: dial and write, as `esper connect --viewer` does.
	var conn net.Conn
	var err error
	for range 50 {
		conn, err = net.Dial("unix", pictures.path)
		if err == nil {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if err != nil {
		t.Fatalf("dialling the window: %v", err)
	}

	answer := <-res
	if answer == nil {
		t.Fatal("no answer")
	}
	defer func() { _ = answer.Body.Close() }()
	if answer.StatusCode != http.StatusOK {
		t.Fatalf("the window answered %d", answer.StatusCode)
	}
	if got := answer.Header.Get("Content-Type"); got != "application/octet-stream" {
		t.Fatalf("content type %q", got)
	}

	// **Read before the writer has finished.** This is the whole claim: a
	// picture reaches the page when it is written, not when the session ends.
	// A body that only arrived on close would pass a test that read to EOF.
	if _, err := conn.Write([]byte("ESPRVID\x00\x01first")); err != nil {
		t.Fatalf("writing: %v", err)
	}
	buf := make([]byte, 14)
	if _, err := io.ReadFull(answer.Body, buf); err != nil {
		t.Fatalf("reading what was written: %v", err)
	}
	if string(buf) != "ESPRVID\x00\x01first" {
		t.Fatalf("the page got %q", buf)
	}

	if _, err := conn.Write([]byte("second")); err != nil {
		t.Fatalf("writing again: %v", err)
	}
	buf = make([]byte, 6)
	if _, err := io.ReadFull(answer.Body, buf); err != nil {
		t.Fatalf("reading the second write: %v", err)
	}
	if string(buf) != "second" {
		t.Fatalf("the page got %q", buf)
	}

	// And the body ends when the session does, rather than hanging.
	_ = conn.Close()
	rest, err := io.ReadAll(answer.Body)
	if err != nil {
		t.Fatalf("draining: %v", err)
	}
	if len(rest) != 0 {
		t.Fatalf("unexpected trailing bytes %q", rest)
	}
}

func TestAPageThatAsksWithNoSessionIsToldSo(t *testing.T) {
	pictures, server := served(t)
	pictures.patience = 150 * time.Millisecond

	res, err := http.Get(server.URL + "/shell/video")
	if err != nil {
		t.Fatalf("asking for the picture: %v", err)
	}
	defer func() { _ = res.Body.Close() }()
	if res.StatusCode != http.StatusGatewayTimeout {
		t.Fatalf("a page waiting for a session that never came got %d", res.StatusCode)
	}
	body, _ := io.ReadAll(res.Body)
	// A status code is not an explanation, and this one is read by a person
	// when the Session screen says why there is no picture.
	if !strings.Contains(string(body), "no session") {
		t.Fatalf("the window said %q, which does not say what is missing", body)
	}
}

// What the page does reaches the session, down the same socket.
func TestWhatThePageDoesReachesTheSession(t *testing.T) {
	pictures, server := served(t)

	// The page asks for the picture, which is what accepts the session.
	go func() {
		res, err := http.Get(server.URL + "/shell/video")
		if err == nil {
			defer func() { _ = res.Body.Close() }()
			_, _ = io.ReadAll(res.Body)
		}
	}()

	var conn net.Conn
	var err error
	for range 50 {
		conn, err = net.Dial("unix", pictures.path)
		if err == nil {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if err != nil {
		t.Fatalf("dialling the window: %v", err)
	}
	defer func() { _ = conn.Close() }()

	// The route only has somewhere to put input once the session is accepted.
	var answer *http.Response
	for range 50 {
		answer, err = http.Post(server.URL+"/shell/video/input", "application/octet-stream",
			strings.NewReader("ESPRINP\x00\x01"))
		if err != nil {
			t.Fatalf("sending input: %v", err)
		}
		_ = answer.Body.Close()
		if answer.StatusCode != http.StatusConflict {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if answer.StatusCode != http.StatusNoContent {
		t.Fatalf("the window answered %d to a batch of input", answer.StatusCode)
	}

	buf := make([]byte, 9)
	_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	if _, err := io.ReadFull(conn, buf); err != nil {
		t.Fatalf("the session did not receive what the page sent: %v", err)
	}
	if string(buf) != "ESPRINP\x00\x01" {
		t.Fatalf("the session received %q", buf)
	}
}

// Input with no session is refused with a sentence rather than dropped.
func TestInputWithNoSessionIsRefused(t *testing.T) {
	_, server := served(t)
	res, err := http.Post(server.URL+"/shell/video/input", "application/octet-stream", strings.NewReader("x"))
	if err != nil {
		t.Fatalf("sending input: %v", err)
	}
	defer func() { _ = res.Body.Close() }()
	if res.StatusCode != http.StatusConflict {
		t.Fatalf("input with no session got %d", res.StatusCode)
	}
	body, _ := io.ReadAll(res.Body)
	if !strings.Contains(string(body), "no session") {
		t.Fatalf("the window said %q", body)
	}
}

// This path ends at somebody else's keyboard, so it is bounded.
func TestAnAbsurdBatchOfInputIsRefused(t *testing.T) {
	pictures, server := served(t)
	go func() {
		res, err := http.Get(server.URL + "/shell/video")
		if err == nil {
			defer func() { _ = res.Body.Close() }()
			_, _ = io.ReadAll(res.Body)
		}
	}()
	var conn net.Conn
	for range 50 {
		c, err := net.Dial("unix", pictures.path)
		if err == nil {
			conn = c
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if conn == nil {
		t.Fatal("could not dial the window")
	}
	defer func() { _ = conn.Close() }()

	var answer *http.Response
	for range 50 {
		res, err := http.Post(server.URL+"/shell/video/input", "application/octet-stream",
			strings.NewReader(strings.Repeat("k", mostInputBytes+1)))
		if err != nil {
			t.Fatalf("sending input: %v", err)
		}
		_ = res.Body.Close()
		answer = res
		if res.StatusCode != http.StatusConflict {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if answer.StatusCode != http.StatusRequestEntityTooLarge {
		t.Fatalf("an oversized batch got %d", answer.StatusCode)
	}
}

// A popped-out window does not take the main window's pictures.
func TestEachWindowHasItsOwnSocket(t *testing.T) {
	home := t.TempDir()
	main, err := openViewer(home, false)
	if err != nil {
		t.Fatalf("the main window: %v", err)
	}
	defer main.close()
	popout, err := openViewer(home, true)
	if err != nil {
		t.Fatalf("a popped-out window: %v", err)
	}
	defer popout.close()
	if main.path == popout.path {
		t.Fatalf("both windows listen on %s, so one would take the other's session", main.path)
	}
}
