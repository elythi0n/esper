package home

import (
	"strings"
	"testing"
)

// **A mount point that contains the path is not the mount it is on.** `/` and
// `/home` both contain `/home/somebody/.esper`, and the device backing it is
// the longest match — so a machine with `/home` on its own encrypted volume and
// `/` on a plain one must not be answered from `/`.
func TestTheLongestMountWins(t *testing.T) {
	// Exercised through the real /proc on this machine: whatever it says, the
	// mount it names must actually contain the path, and no mount that also
	// contains it may be longer.
	const under = "/usr/share"
	device, mount, ok := backingDevice(under)
	if !ok {
		t.Skip("no /proc/self/mounts to read on this machine")
	}
	if device == "" || mount == "" {
		t.Fatalf("a mount was found with no device: %q at %q", device, mount)
	}
	within := strings.TrimSuffix(mount, "/") + "/"
	if mount != "/" && !strings.HasPrefix(under, within) {
		t.Errorf("%q does not contain %s", mount, under)
	}

	// And a path under a deeper mount resolves to the deeper one. `/proc` is
	// mounted on every Linux machine and is deeper than `/`.
	_, deeper, ok := backingDevice("/proc/self")
	if ok && deeper != "/proc" {
		t.Errorf("/proc/self resolved to %q rather than /proc", deeper)
	}
}

// A mount point with a space in it is octal-escaped in `/proc/self/mounts`;
// reading it literally would fail to match a path that is genuinely under it.
func TestAMountPointWithASpaceIsRead(t *testing.T) {
	for _, c := range []struct{ raw, want string }{
		{`/mnt/my\040disk`, "/mnt/my disk"},
		{`/mnt/tab\011here`, "/mnt/tab\there"},
		{`/mnt/back\134slash`, `/mnt/back\slash`},
		{"/ordinary", "/ordinary"},
	} {
		if got := unescapeMount(c.raw); got != c.want {
			t.Errorf("%q read as %q, want %q", c.raw, got, c.want)
		}
	}
}

// **"Cannot tell" is an answer, and it is not "no".** A wrong "encrypted" is
// the one that stops somebody acting, so anything unresolvable says so.
func TestWhatCannotBeClassifiedSaysSoRatherThanGuessing(t *testing.T) {
	at := AtRest{Dir: "/somewhere"}
	said := at.Describe()
	if strings.Contains(said, "NOT on an encrypted") {
		t.Errorf("an unknown volume was reported as unencrypted: %q", said)
	}
	if !strings.Contains(said, "cannot classify") {
		t.Errorf("an unknown volume did not say so: %q", said)
	}
	// And it still says what is at stake, because somebody reading "cannot
	// tell" needs to know why they should find out.
	if !strings.Contains(said, "is this machine") {
		t.Errorf("the sentence does not say what a copy of the directory is: %q", said)
	}
}

// The two definite answers read as different things, and the unencrypted one
// says what a copy of the directory is.
func TestTheTwoDefiniteAnswersDoNotReadAlike(t *testing.T) {
	yes, no := true, false
	encrypted := AtRest{Dir: "/d", Encrypted: &yes, How: "dm-crypt: CRYPT-LUKS2-…"}.Describe()
	plain := AtRest{Dir: "/d", Encrypted: &no}.Describe()

	if encrypted == plain {
		t.Fatal("an encrypted volume and a plain one read the same")
	}
	if !strings.Contains(encrypted, "dm-crypt") {
		t.Errorf("the encrypted answer does not say what was found: %q", encrypted)
	}
	if !strings.Contains(plain, "stolen disk") || !strings.Contains(plain, "backup") {
		t.Errorf("the plain answer does not say what it costs: %q", plain)
	}
}

// A device that is not device-mapper at all is a plain partition, and that is
// a definite "no" rather than an unknown — otherwise every ordinary machine
// would be told this cannot be established.
func TestAPlainPartitionIsADefiniteNo(t *testing.T) {
	encrypted, _, known := isEncrypted("/dev/null")
	if !known {
		t.Fatal("a plain device under /dev was not classified")
	}
	if encrypted {
		t.Error("/dev/null was reported as encrypted")
	}
	// And something that is not a device at all is unknown, not "no".
	if _, _, known := isEncrypted("tmpfs"); known {
		t.Error("a pseudo-filesystem was classified as a plain device")
	}
}
