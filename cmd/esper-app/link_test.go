package main

import "testing"

// TestOnlyAnInvitationIsFollowed is the allowlist, stated as cases.
//
// Whatever this accepts becomes a URL fragment in this window and, when
// somebody accepts it, an argument on the engine's command line. So the
// interesting rows are the ones that are *almost* right.
func TestOnlyAnInvitationIsFollowed(t *testing.T) {
	t.Parallel()
	const token = "ASAr1PzAc9V8d9poJUwW1ZxS02Vd-y5W"

	for _, c := range []struct {
		why   string
		arg   string
		route string
	}{
		{"an invitation", "esper://join/" + token, "/pair/invited/" + token},
		{"with whitespace around it", "  esper://join/" + token + "\n", "/pair/invited/" + token},
		{"another scheme", "https://join/" + token, ""},
		{"another verb", "esper://pair/" + token, ""},
		{"no token", "esper://join/", ""},
		{"a token too short to be one", "esper://join/abc", ""},
		{"a host instead of a path", "esper://join", ""},
		{"a shell metacharacter", "esper://join/" + token + ";rm -rf /", ""},
		{"a quote", `esper://join/` + token + `"`, ""},
		{"a traversal", "esper://join/../../etc/passwd", ""},
		{"a second segment", "esper://join/" + token + "/more", ""},
		{"a query", "esper://join/" + token + "?x=1", ""},
		{"nothing at all", "", ""},
	} {
		if got := routeForLink(c.arg); got != c.route {
			t.Errorf("%s: routeForLink(%q) = %q, wanted %q", c.why, c.arg, got, c.route)
		}
	}
}

// TestABadLinkIsStillALink keeps the difference between "not for us" and
// "for us and wrong": the second is worth a message, and the first must not
// be, or every argument becomes a complaint.
func TestABadLinkIsStillALink(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		arg  string
		ours bool
	}{
		{"esper://join/nonsense", true},
		{"esper://anything", true},
		{"https://example.com", false},
		{"/some/file", false},
		{"", false},
	} {
		if got := looksLikeALink(c.arg); got != c.ours {
			t.Errorf("looksLikeALink(%q) = %v, wanted %v", c.arg, got, c.ours)
		}
	}
}
