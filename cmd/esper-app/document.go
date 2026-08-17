package main

import (
	"bytes"
	"net/http"
	"path"
	"strings"
)

// isDocument says whether a request is for the page rather than an asset.
func isDocument(p string) bool {
	if p == "" || p == "/" {
		return true
	}
	return path.Ext(p) == "" || path.Ext(p) == ".html"
}

// serveDocument hands the page the two things only its host knows.
//
// # Why the document is rewritten
//
// **That it is in a frameless window.** The window has no system chrome, so
// the page draws the bar with the buttons that close it. Asking over HTTP
// works but arrives a frame late: the first paint has no bar, the layout
// then shifts by its height, and — the part that matters — a window whose
// capabilities request ever fails has no close button and cannot be shut. A
// fact the host knows at serve time belongs in the document it serves.
//
// **Where it is.** A popped-out window has to open on one session, and a
// webview is given a URL by its host at start-up with no way to add a
// fragment afterwards: Wails serves `/` and `location.hash` is empty.
//
// So: one script tag, before the bundle loads, setting a data attribute the
// stylesheet already keys on and the hash the router reads on its first
// render. No query strings (the router is hash-based and a second scheme
// would be a second thing to keep in step) and no globals the page has to
// know about. `/shell/capabilities` still answers the rest — whether this
// window may open another — which no first paint depends on.
//
// A capturing ResponseWriter is used rather than reading the file directly,
// because the asset server owns how the bundle is read and this should not
// grow a second copy of that.
func serveDocument(w http.ResponseWriter, r *http.Request, next http.Handler, route, apiAddr string) {
	rec := &captured{header: http.Header{}}
	next.ServeHTTP(rec, r)

	body := rec.body.Bytes()
	if rec.status == 0 {
		rec.status = http.StatusOK
	}
	if rec.status == http.StatusOK && bytes.Contains(body, []byte("</head>")) {
		script := `<script>var d=document.documentElement.dataset;d.desktop="true";d.api=` + quote(apiAddr, "")
		if route != "" {
			script += `;location.hash=` + quote(route, "#")
		}
		script += `;</script></head>`
		body = bytes.Replace(body, []byte("</head>"), []byte(script), 1)
	}

	for k, v := range rec.header {
		w.Header()[k] = v
	}
	w.Header().Del("Content-Length")
	w.WriteHeader(rec.status)
	_, _ = w.Write(body)
}

// quote makes a JavaScript string literal, with an optional prefix.
//
// A route is already held to `safeRoute` and an address to being one, so
// neither can carry a quote, a backslash or an angle bracket; this is the
// belt to that's braces, so a future caller loosening either does not also
// open a script injection.
func quote(value, prefix string) string {
	r := strings.NewReplacer(`\`, `\\`, `"`, `\"`, `<`, `<`, `>`, `>`, "\n", "", "\r", "")
	return `"` + prefix + r.Replace(value) + `"`
}

// captured buffers a response so it can be rewritten before it is sent.
type captured struct {
	header http.Header
	body   bytes.Buffer
	status int
}

func (c *captured) Header() http.Header { return c.header }
func (c *captured) WriteHeader(s int)   { c.status = s }
func (c *captured) Write(b []byte) (int, error) {
	if c.status == 0 {
		c.status = http.StatusOK
	}
	return c.body.Write(b)
}
