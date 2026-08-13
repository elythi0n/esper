package devices

import (
	"encoding/hex"
	"encoding/json"
	"fmt"
	"hash/fnv"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// fixture is the shared corpus both languages read.
func fixture(t *testing.T, name string) string {
	t.Helper()
	return filepath.Join("..", "..", "testdata", "fixtures", name)
}

func TestTheStoreFixtureReadsToTheSameDevices(t *testing.T) {
	list, err := Load(fixture(t, "device_store.json"))
	if err != nil {
		t.Fatalf("the fixture should load: %v", err)
	}
	if len(list) != 3 {
		t.Fatalf("the fixture holds 3 devices, got %d", len(list))
	}

	// Sorted by name: elythion, laptop, rack.
	names := []string{list[0].Name, list[1].Name, list[2].Name}
	want := []string{"elythion", "laptop", "rack"}
	for i := range want {
		if names[i] != want[i] {
			t.Fatalf("devices should be listed by name %v, got %v", want, names)
		}
	}

	elythion := list[0]
	if !elythion.Pairing.Kept() {
		t.Error("elythion was paired in person and should be kept")
	}
	if elythion.LastSeen != nil {
		t.Error("elythion has never connected and should have no last-seen")
	}
	if !elythion.May(View) || !elythion.May(Control) {
		t.Error("a freshly paired device holds view and control")
	}
	if elythion.May(Unattended) {
		t.Error("a freshly paired device must never hold unattended")
	}

	rack := list[2]
	if !rack.May(Unattended) || !rack.May(Audio) {
		t.Error("rack was granted unattended and audio")
	}
	if rack.LockAfter == nil || *rack.LockAfter != LockAlways {
		t.Errorf("rack locks after every session, got %v", rack.LockAfter)
	}
	if rack.UnattendedWhen == nil || *rack.UnattendedWhen != "weekdays 09:00-17:00" {
		t.Errorf("rack's schedule should survive the read, got %v", rack.UnattendedWhen)
	}
	if rack.LastAddress != "192.0.2.10:7420" || rack.OverlayAddress != "[100::2]:7420" {
		t.Error("both addresses are kept, because a machine reachable both ways is")
	}
	if rack.LastMAC != "02:00:5e:10:00:00" {
		t.Errorf("rack's hardware address should survive the read, got %q", rack.LastMAC)
	}
	if len(rack.Groups) != 1 || rack.Groups[0] != "ops" {
		t.Errorf("rack is in the ops group, got %v", rack.Groups)
	}

	laptop := list[1]
	if laptop.Pairing.Kept() {
		t.Fatal("laptop was invited for one session")
	}
	if laptop.Pairing.Once.Used {
		t.Error("the invitation in the fixture is unspent")
	}
	if !laptop.Pairing.Admits(1_700_000_400) {
		t.Error("an unspent invitation inside its window admits a session")
	}
	if laptop.Pairing.Admits(1_700_004_000) {
		t.Error("an invitation past its deadline admits nothing")
	}
}

func TestTheFingerprintFixtureIsWhatThisSideDerives(t *testing.T) {
	raw, err := os.ReadFile(fixture(t, "device_fingerprints.json"))
	if err != nil {
		t.Fatalf("the fixture is committed: %v", err)
	}
	var rows []struct {
		PublicKey string   `json:"public_key"`
		Hex       string   `json:"hex"`
		Words     []string `json:"words"`
		Inline    string   `json:"inline"`
		Display   string   `json:"display"`
	}
	if err := json.Unmarshal(raw, &rows); err != nil {
		t.Fatalf("parsing the fixture: %v", err)
	}
	if len(rows) != 16 {
		t.Fatalf("the fixture should hold 16 keys, got %d", len(rows))
	}
	for _, row := range rows {
		key, err := hex.DecodeString(row.PublicKey)
		if err != nil {
			t.Fatalf("the fixture key %q is not hex: %v", row.PublicKey, err)
		}
		got := FingerprintOf(key)
		if got.Hex != row.Hex {
			t.Errorf("fingerprint of %s: Rust says %s, this side says %s",
				row.PublicKey[:16], row.Hex, got.Hex)
		}
		if strings.Join(got.Words, " ") != row.Inline {
			t.Errorf("words of %s: Rust says %q, this side says %q",
				row.PublicKey[:16], row.Inline, strings.Join(got.Words, " "))
		}
		if len(got.Words) != FingerprintWords {
			t.Errorf("a fingerprint is %d words, got %d", FingerprintWords, len(got.Words))
		}
	}
}

func TestTheWordListIsTheSameListAsRusts(t *testing.T) {
	// FNV-1a over every word with 0xff after each, exactly as `esper-crypto`
	// computes it. Change a word on either side and both suites fail.
	sum := fnv.New64a()
	for _, word := range words {
		if _, err := sum.Write([]byte(word)); err != nil {
			t.Fatalf("hashing: %v", err)
		}
		if _, err := sum.Write([]byte{0xff}); err != nil {
			t.Fatalf("hashing: %v", err)
		}
	}
	if got := sum.Sum64(); got != WordlistChecksum {
		t.Fatalf("the word list has changed: %#x, expected %#x", got, WordlistChecksum)
	}
	seen := make(map[string]bool, len(words))
	for i, word := range words {
		if word == "" {
			t.Fatalf("word %d is empty", i)
		}
		if seen[word] {
			t.Fatalf("%q appears twice: two keys would read out the same", word)
		}
		seen[word] = true
	}
}

func TestAMissingStoreIsAnEmptyOneAndNotAnError(t *testing.T) {
	list, err := Load(filepath.Join(t.TempDir(), "devices.json"))
	if err != nil {
		t.Fatalf("a machine that has never paired is not a failure: %v", err)
	}
	if len(list) != 0 {
		t.Fatalf("expected no devices, got %d", len(list))
	}
}

// TestAStoreThatCannotBeReadIsAnError is the other half of the one above: the
// two answers must never be confused, because a device list that looks empty
// invites somebody to pair again rather than to look at the file.
func TestAStoreThatCannotBeReadIsAnError(t *testing.T) {
	for _, broken := range []struct {
		what string
		text string
	}{
		{"not json at all", `this is not a store`},
		{"a later format", `{"version": 2, "devices": {}}`},
		{"an earlier format", `{"version": 0, "devices": {}}`},
		{"a key that is short", `{"version":1,"devices":{"ab":{"public_key":"ab",` +
			`"name":"x","permissions":["view"],"paired_at":1,"last_seen":null,"pairing":"kept"}}}`},
		{"a key that is not hex", `{"version":1,"devices":{"zz":{"public_key":` +
			`"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",` +
			`"name":"x","permissions":["view"],"paired_at":1,"last_seen":null,"pairing":"kept"}}}`},
		{"a permission nobody knows", storeWith(`"permissions":["view","root"]`)},
		{"a lock-after nobody knows", storeWith(`"permissions":["view"],"lock_after":"sometimes"`)},
		{"a pairing nobody knows", storeWith(`"permissions":["view"],"pairing":"forever"`)},
		{"two stores in one file", `{"version":1,"devices":{}}{"version":1,"devices":{}}`},
	} {
		t.Run(broken.what, func(t *testing.T) {
			if _, err := Read([]byte(broken.text), "the store"); err == nil {
				t.Fatalf("%s should be refused, not read past", broken.what)
			}
		})
	}
}

// storeWith builds a one-device store with the given fields spliced in, so a
// case above can say only what it is testing.
func storeWith(fields string) string {
	const key = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
	return fmt.Sprintf(
		`{"version":1,"devices":{%q:{"public_key":%q,"name":"x",%s,`+
			`"paired_at":1,"last_seen":null}}}`,
		key, key, fields,
	)
}

// TestAFieldThisBuildCannotSeeStillReads is the deliberate other side of the
// refusals above. Rust adds device fields without moving the format version,
// so refusing an unknown one would blank the device list the next time
// anybody did.
func TestAFieldThisBuildCannotSeeStillReads(t *testing.T) {
	text := storeWith(`"permissions":["view"],"pairing":"kept","invented_later":{"a":1}`)
	list, err := Read([]byte(text), "the store")
	if err != nil {
		t.Fatalf("a field added under the same version must still read: %v", err)
	}
	if len(list) != 1 || list[0].Name != "x" {
		t.Fatalf("expected the one device, got %v", list)
	}
}

// TestTheRecordIsTheIdentityAndTheMapKeyIsAnIndex matches Rust's `normalize`:
// an entry filed under the wrong key is re-keyed from its own `public_key`
// rather than dropped, so neither side ever lists a device the other does not.
func TestTheRecordIsTheIdentityAndTheMapKeyIsAnIndex(t *testing.T) {
	const real = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
	const misfiled = "0000000000000000000000000000000000000000000000000000000000000000"
	text := fmt.Sprintf(
		`{"version":1,"devices":{%q:{"public_key":%q,"name":"x","permissions":["view"],`+
			`"paired_at":1,"last_seen":null,"pairing":"kept"}}}`,
		misfiled, real,
	)
	list, err := Read([]byte(text), "the store")
	if err != nil {
		t.Fatalf("a misfiled entry is corrected, not refused: %v", err)
	}
	if len(list) != 1 {
		t.Fatalf("expected the one device, got %d", len(list))
	}
	if got := list[0].PublicKey.String(); got != real {
		t.Fatalf("the record's own key is the identity; got %s", got)
	}
}

// TestTwoEntriesNamingOneKeyLeaveTheLaterOne is the one case where the map key
// changes the answer rather than only the spelling, so it is the one that
// actually pins re-keying. Rust's `normalize` rebuilds in map order and the
// later insert wins; `two_entries_naming_one_key_leave_the_later_one` in
// `esper-crypto` pins that, and this holds this side to it.
func TestTwoEntriesNamingOneKeyLeaveTheLaterOne(t *testing.T) {
	const real = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
	const first = "0000000000000000000000000000000000000000000000000000000000000000"
	const last = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
	entry := func(filed, name string) string {
		return fmt.Sprintf(
			`%q:{"public_key":%q,"name":%q,"permissions":["view"],`+
				`"paired_at":1,"last_seen":null,"pairing":"kept"}`,
			filed, real, name,
		)
	}
	text := fmt.Sprintf(`{"version":1,"devices":{%s,%s}}`,
		entry(first, "first"), entry(last, "second"))

	// Read repeatedly. Go randomizes map order on purpose, so a reader that
	// left the winner to the map would pass a single run about half the time —
	// which is how this test first failed to catch that exact defect.
	for attempt := range 64 {
		list, err := Read([]byte(text), "the store")
		if err != nil {
			t.Fatalf("reading: %v", err)
		}
		if len(list) != 1 {
			t.Fatalf("one key is one device, got %d", len(list))
		}
		if list[0].Name != "second" {
			t.Fatalf("entries are re-keyed in map order, so the later index wins; "+
				"got %q on attempt %d", list[0].Name, attempt)
		}
	}
}

// TestUppercaseHexIsTheSameKey holds this side to Rust's reader, which accepts
// it: neither side may refuse a file the other takes.
func TestUppercaseHexIsTheSameKey(t *testing.T) {
	const lower = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
	text := fmt.Sprintf(
		`{"version":1,"devices":{%q:{"public_key":%q,"name":"x","permissions":["view"],`+
			`"paired_at":1,"last_seen":null,"pairing":"kept"}}}`,
		strings.ToUpper(lower), strings.ToUpper(lower),
	)
	list, err := Read([]byte(text), "the store")
	if err != nil {
		t.Fatalf("uppercase hex is the same key: %v", err)
	}
	if got := list[0].PublicKey.String(); got != lower {
		t.Fatalf("a key should normalise to lowercase, got %s", got)
	}
}

// TestTheIndexIsOrderedAsTheFileWroteIt is why the store's map is keyed on the
// raw text rather than on a parsed key. Rust holds a `BTreeMap<String, _>`, so
// duplicate resolution follows the order of the hex **as written**, and
// uppercase sorts before lowercase. Parsing first would normalize the case away
// and pick the other entry.
//
// `the_index_is_ordered_as_the_file_wrote_it` in `esper-crypto` pins Rust's
// answer; this holds this side to it.
func TestTheIndexIsOrderedAsTheFileWroteIt(t *testing.T) {
	const lower = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
	upper := strings.ToUpper(lower)
	entry := func(filed, name string) string {
		return fmt.Sprintf(
			`%q:{"public_key":%q,"name":%q,"permissions":["view"],`+
				`"paired_at":1,"last_seen":null,"pairing":"kept"}`,
			filed, lower, name,
		)
	}
	text := fmt.Sprintf(`{"version":1,"devices":{%s,%s}}`,
		entry(upper, "filed in uppercase"), entry(lower, "filed in lowercase"))

	for attempt := range 64 {
		list, err := Read([]byte(text), "the store")
		if err != nil {
			t.Fatalf("reading: %v", err)
		}
		if len(list) != 1 {
			t.Fatalf("one key is one device, got %d", len(list))
		}
		if list[0].Name != "filed in lowercase" {
			t.Fatalf("uppercase hex sorts first, so the lowercase index is the later "+
				"one; got %q on attempt %d", list[0].Name, attempt)
		}
	}
}

// TestTheStoreSitsInTheEsperHome — the file's name and where it sits are one
// fact, and the engine resolves it the same way. Which directory *is* the
// Esper home is `internal/home`'s to answer and is tested there.
func TestTheStoreSitsInTheEsperHome(t *testing.T) {
	if got := PathIn("/somewhere/else"); got != "/somewhere/else/devices.json" {
		t.Fatalf("expected /somewhere/else/devices.json, got %s", got)
	}
}

func TestEveryPermissionIsKnownAndNamedOnce(t *testing.T) {
	if len(AllPermissions) != 14 {
		t.Fatalf("Rust has 14 permissions, this side has %d", len(AllPermissions))
	}
	seen := make(map[Permission]bool, len(AllPermissions))
	for _, p := range AllPermissions {
		if !p.Known() {
			t.Errorf("%q is in the list and not known by it", p)
		}
		if seen[p] {
			t.Errorf("%q appears twice", p)
		}
		seen[p] = true
		if p != Permission(strings.ToLower(string(p))) {
			t.Errorf("%q is not the snake_case word the wire uses", p)
		}
	}
	if Permission("root").Known() {
		t.Error("a word nobody granted must not be known")
	}
}

// TestALockAfterChoiceIsOneOfThree holds the smaller vocabulary to the same bar.
func TestALockAfterChoiceIsOneOfThree(t *testing.T) {
	if len(AllLockAfter) != 3 {
		t.Fatalf("Rust has 3 lock-after choices, this side has %d", len(AllLockAfter))
	}
	for _, l := range AllLockAfter {
		if !l.Known() {
			t.Errorf("%q is in the list and not known by it", l)
		}
	}
	if LockAfter("sometimes").Known() {
		t.Error("a word nobody chose must not be known")
	}
}
