package main

import (
	"regexp"
	"strings"
)

// linkShape is what an `esper://` link may look like.
//
// The same allowlist the control plane holds an invitation to, for the same
// reason: whatever this admits ends up in a URL fragment in a window and, on
// the far side, on the engine's command line. An allowlist rather than a URL
// parse, because a parser admits whatever the standard does and this only
// has to admit what the engine prints.
var linkShape = regexp.MustCompile(`^esper://join/[A-Za-z0-9_-]{16,512}$`)

// routeForLink turns an `esper://` link into a route the window can open.
//
// # Why a link becomes a route rather than an action
//
// A link that paired the machine the moment it was clicked would be a
// one-click way to trust a stranger, and the whole of Esper's identity model
// is that trust is established deliberately. So the link opens the screen
// where the invitation is *shown*, with a person in front of it, and pairing
// happens when they say so.
//
// Returns "" for anything that is not an invitation, which the caller treats
// as a link it will not follow.
func routeForLink(arg string) string {
	link := strings.TrimSpace(arg)
	if !linkShape.MatchString(link) {
		return ""
	}
	// The token is the path's last segment, and it is already base64url —
	// nothing in it needs escaping to sit in a fragment.
	token := strings.TrimPrefix(link, "esper://join/")
	return "/pair/invited/" + token
}

// looksLikeALink says whether an argument is trying to be one.
//
// Kept apart from routeForLink so a malformed `esper://` argument can be
// reported as a bad link rather than silently ignored, which is the
// difference between a scheme handler that says no and one that appears not
// to have been asked.
func looksLikeALink(arg string) bool {
	return strings.HasPrefix(strings.TrimSpace(arg), "esper://")
}
