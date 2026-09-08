package main

import (
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strconv"
	"strings"

	"github.com/elythi0n/esper/internal/server"
)

// apiPrefix is what the webview asks for and this forwards.
const apiPrefix = "/api/"

// proxyToAPI forwards `/api/*` to the local API, adding the token.
//
// # Why the window proxies rather than handing the token over
//
// The API takes a bearer token that lives in a 0600 file, and it refuses any
// request addressed by a name rather than by the literal address it bound —
// a DNS-rebinding defence. A webview given the token would be making a
// cross-origin request the API has no reason to allow, and would be holding a
// credential that one scripting bug puts on somebody else's machine.
//
// Forwarding solves both and costs about forty lines. The page makes
// same-origin requests with nothing secret in them; the token never leaves
// this process; and the API's posture is untouched, because what reaches it
// is an ordinary loopback request addressed by address.
//
// # Streaming
//
// `/events` is Server-Sent Events, so the transport must not buffer.
// `FlushInterval: -1` makes the reverse proxy flush as it reads, which is
// what keeps a session appearing in Activity the moment it happens rather
// than whenever a buffer fills.
func proxyToAPI(addr string, token server.Token) (http.Handler, error) {
	if _, _, err := net.SplitHostPort(addr); err != nil {
		return nil, fmt.Errorf("%q is not an address and a port: %w", addr, err)
	}
	target, err := url.Parse("http://" + addr)
	if err != nil {
		return nil, fmt.Errorf("the API address %q cannot be a URL: %w", addr, err)
	}

	proxy := &httputil.ReverseProxy{
		FlushInterval: -1,
		Rewrite: func(r *httputil.ProxyRequest) {
			r.SetURL(target)
			// **Addressed as the address.** SetURL leaves the inbound Host
			// alone, and the API refuses a name; this is the one header that
			// decides whether the request is answered at all.
			r.Out.Host = addr
			r.Out.Header.Set("Authorization", "Bearer "+token.String())
		},
		// **A refusal is not an absence.** The token this process attaches is
		// the one from this machine's Esper home, so a 401 from upstream can
		// only mean the thing on the address is not the API that token
		// belongs to — another Esper, from another home. Passed through
		// untouched it reaches the page as one more failed request and the
		// window shows "nothing is answering" for ever, which is both wrong
		// and unactionable. The window knows what happened; it says so here.
		ModifyResponse: func(answer *http.Response) error {
			if answer.StatusCode != http.StatusUnauthorized {
				return nil
			}
			said := "Another Esper is answering on " + addr + " and it does not take this machine's token."
			// Replaced below, so the only thing this close can fail at is a
			// body that was already finished with.
			_ = answer.Body.Close()
			answer.Body = io.NopCloser(strings.NewReader(said))
			answer.ContentLength = int64(len(said))
			answer.Header.Set("Content-Type", "text/plain; charset=utf-8")
			answer.Header.Set("Content-Length", strconv.Itoa(len(said)))
			answer.Header.Del("Www-Authenticate")
			return nil
		},
		ErrorHandler: func(w http.ResponseWriter, _ *http.Request, err error) {
			// A sentence the screens can show. The window is running and the
			// control plane is not, which is a different fault from a refusal
			// and reads differently on screen.
			http.Error(w,
				"The control plane did not answer at "+addr+". Is `esper api` running?",
				http.StatusBadGateway)
			_ = err
		},
	}

	return http.StripPrefix(strings.TrimSuffix(apiPrefix, "/"), proxy), nil
}
