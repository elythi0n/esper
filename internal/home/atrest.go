package home

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// AtRest is what protects this machine's Esper state while it is not running.
//
// # Why this is a question the product answers rather than a document
//
// `~/.esper` holds this machine's identity key, the local API's token, the
// device list and the record of what happened. **A copy of that directory is
// this machine**: anything holding the identity key is this machine to every
// device that has paired with it, and the pairing is what every other check
// rests on.
//
// File permissions are what protects it from another account. They protect it
// from nothing at all once the disk leaves the building — a stolen laptop, a
// disk image, a backup, a home directory synced somewhere. The answer to that
// is full-disk encryption, which is not Esper's to provide and *is* Esper's to
// say out loud, on the machine, to the person who can act on it.
type AtRest struct {
	// Dir is the directory in question.
	Dir string
	// Encrypted says whether it sits on an encrypted volume. Nil where this
	// machine cannot tell — which is an answer, and not the same as "no".
	Encrypted *bool
	// How names what was found, for somebody who wants to check it.
	How string
}

// Describe is the sentence `esper doctor` prints.
func (a AtRest) Describe() string {
	switch {
	case a.Encrypted == nil:
		return "on a volume this cannot classify — if it is not encrypted, a copy of this " +
			"directory is this machine"
	case *a.Encrypted:
		return "on an encrypted volume (" + a.How + ")"
	default:
		return "NOT on an encrypted volume — a copy of this directory is this machine, so a " +
			"stolen disk or a backup is this machine's identity"
	}
}

// StateAtRest answers the question for a directory.
//
// Linux only for now, and it says so by answering "cannot tell" elsewhere
// rather than guessing: a wrong "encrypted" is worse than no answer, because
// it is the one that stops somebody acting.
func StateAtRest(dir string) AtRest {
	at := AtRest{Dir: dir}
	device, mount, ok := backingDevice(dir)
	if !ok {
		return at
	}
	at.How = mount
	encrypted, how, known := isEncrypted(device)
	if !known {
		return at
	}
	at.Encrypted = &encrypted
	if how != "" {
		at.How = how
	}
	return at
}

// backingDevice finds the mount that contains a path, and the device under it.
//
// The longest matching mount point wins: `/home` and `/` both contain
// `/home/somebody`, and the answer is the one that is actually mounted there.
func backingDevice(dir string) (device, mount string, ok bool) {
	resolved, err := filepath.Abs(dir)
	if err != nil {
		return "", "", false
	}
	f, err := os.Open("/proc/self/mounts")
	if err != nil {
		return "", "", false
	}
	defer func() { _ = f.Close() }()

	type entry struct{ point, dev string }
	var found []entry
	scan := bufio.NewScanner(f)
	for scan.Scan() {
		fields := strings.Fields(scan.Text())
		if len(fields) < 2 {
			continue
		}
		point := unescapeMount(fields[1])
		if resolved == point || strings.HasPrefix(resolved, strings.TrimSuffix(point, "/")+"/") {
			found = append(found, entry{point: point, dev: fields[0]})
		}
	}
	if len(found) == 0 {
		return "", "", false
	}
	sort.Slice(found, func(i, j int) bool { return len(found[i].point) > len(found[j].point) })
	return found[0].dev, found[0].point, true
}

// unescapeMount undoes the octal escaping /proc/self/mounts applies to a path
// with a space or a tab in it.
func unescapeMount(s string) string {
	r := strings.NewReplacer(`\040`, " ", `\011`, "\t", `\012`, "\n", `\134`, `\`)
	return r.Replace(s)
}

// isEncrypted says whether a device is a dm-crypt mapping.
//
// **Asked of the kernel, not of a name.** A device called `/dev/mapper/crypt`
// may be anything; what decides it is the device-mapper UUID, which dm-crypt
// prefixes with `CRYPT-`. A device that is not device-mapper at all is a plain
// partition and is not encrypted; anything this cannot resolve is unknown.
func isEncrypted(device string) (encrypted bool, how string, known bool) {
	target, err := filepath.EvalSymlinks(device)
	if err != nil {
		return false, "", false
	}
	name := filepath.Base(target)
	if !strings.HasPrefix(name, "dm-") {
		// A plain block device. Not device-mapper, so not dm-crypt.
		if strings.HasPrefix(target, "/dev/") {
			return false, "", true
		}
		return false, "", false
	}
	uuid, err := os.ReadFile(filepath.Join("/sys/class/block", name, "dm", "uuid"))
	if err != nil {
		return false, "", false
	}
	text := strings.TrimSpace(string(uuid))
	if strings.HasPrefix(text, "CRYPT-") {
		return true, fmt.Sprintf("dm-crypt: %s", text), true
	}
	// Device-mapper, and something else — LVM, a RAID mapping. An LVM volume
	// may itself sit on dm-crypt, so this walks down before answering.
	if under, ok := underneath(name); ok {
		return isEncrypted(under)
	}
	return false, "", true
}

// underneath is the single device a mapping is built on, where there is one.
//
// Only one: a mapping over several devices — a stripe, a mirror — has no
// single answer, and guessing from the first would report a volume as
// encrypted when half of it is not.
func underneath(name string) (string, bool) {
	entries, err := os.ReadDir(filepath.Join("/sys/class/block", name, "slaves"))
	if err != nil || len(entries) != 1 {
		return "", false
	}
	return filepath.Join("/dev", entries[0].Name()), true
}
