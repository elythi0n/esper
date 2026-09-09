package main

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"
)

// howLongASelfTestMayTake bounds the whole thing.
//
// Generous: it pairs two identities, starts a host, opens a session and
// samples it. On a slow machine each of those is seconds, and a diagnostic
// that gave up early would report a fault that was its own impatience.
const howLongASelfTestMayTake = 90 * time.Second

// howLongToWatch is how much of the session is sampled.
//
// Long enough for the encoder to settle past its first keyframe and produce a
// figure that means something, short enough that nobody wonders whether
// `esper doctor` has hung.
const howLongToWatch = 3 * time.Second

// Where a self test says what it just did.
//
// Two of these: one prints each step as it happens, because the test takes
// several seconds and silence in the middle of it reads as a hang; one
// collects them for `--json`. Everything the test says goes through here, so
// the two reports cannot come to disagree about what it found.
type sayer interface {
	// step is one thing the test did, and how it went. A detail may carry
	// newlines, and every line after the first sits under the first.
	step(name string, ok bool, detail string)
	// evidence is the raw output a failed step produced, unedited.
	evidence(lines []string)
	// figures are the numbers behind the step just reported, for a reader
	// that would otherwise have to parse a sentence written for a person.
	figures(of map[string]float64)
	// warn is what a person needs to know after a failure: which half of the
	// path is proven, and which half is the thing to look into.
	warn(text string)
	// verdict is the sentence at the end, and the whole test's answer.
	verdict(ok bool, text string)
}

// printing is the self test as a person watching it reads it.
type printing struct{}

func (printing) step(name string, ok bool, detail string) {
	if !ok {
		fmt.Printf("  ✗ %s\n", detail)
		return
	}
	lines := strings.Split(detail, "\n")
	fmt.Printf("  ✓ %-9s %s\n", name, lines[0])
	for _, line := range lines[1:] {
		fmt.Printf("              %s\n", line)
	}
}

func (printing) evidence(lines []string) { showSome(lines) }

// A person reads the numbers in the sentence.
func (printing) figures(map[string]float64) {}

func (printing) warn(text string) {
	fmt.Println()
	for i, line := range strings.Split(text, "\n") {
		if i == 0 {
			fmt.Printf("⚠  %s\n", line)
			continue
		}
		fmt.Printf("   %s\n", line)
	}
}

func (printing) verdict(ok bool, text string) {
	if !ok {
		return
	}
	fmt.Println()
	for i, line := range strings.Split(text, "\n") {
		if i == 0 {
			fmt.Printf("✓ %s\n", line)
			continue
		}
		fmt.Printf("  %s\n", line)
	}
}

// One thing the self test did, and how it went.
type selfStep struct {
	Name   string `json:"name"`
	OK     bool   `json:"ok"`
	Detail string `json:"detail"`
	// The raw output behind a failed step, where there was any.
	Evidence []string `json:"evidence,omitempty"`
	// The numbers behind the detail, where the step measured any.
	Figures map[string]float64 `json:"figures,omitempty"`
}

// collecting is the same test, kept rather than printed.
type collecting struct {
	Steps    []selfStep `json:"steps"`
	Warnings []string   `json:"warnings"`
	Verdict  string     `json:"verdict"`
	OK       bool       `json:"ok"`
}

func (c *collecting) step(name string, ok bool, detail string) {
	c.Steps = append(c.Steps, selfStep{Name: name, OK: ok, Detail: oneLine(detail)})
}

// figures belong to the step they measure, which is always the last one.
func (c *collecting) figures(of map[string]float64) {
	if len(c.Steps) == 0 {
		return
	}
	c.Steps[len(c.Steps)-1].Figures = of
}

// oneLine folds a detail written to be read down a column into one sentence.
func oneLine(text string) string {
	return strings.Join(strings.Fields(text), " ")
}

// evidence belongs to the step it explains, which is always the last one.
func (c *collecting) evidence(lines []string) {
	if len(c.Steps) == 0 {
		return
	}
	kept := make([]string, 0, len(lines))
	for _, line := range lines {
		if strings.TrimSpace(line) != "" {
			kept = append(kept, line)
		}
	}
	c.Steps[len(c.Steps)-1].Evidence = kept
}

func (c *collecting) warn(text string) {
	c.Warnings = append(c.Warnings, oneLine(text))
}

func (c *collecting) verdict(ok bool, text string) {
	c.OK = ok
	c.Verdict = oneLine(text)
}

// selfTest runs a real session between two throwaway identities on this
// machine, and times it.
//
// # Why a real session and not more probes
//
// Every other line of `esper doctor` is a capability reported by the engine:
// there is a screen, there is an encoder, this is the input backend. All of
// them can be true on a machine where a session still does not work, because
// what they do not test is the *path* — the handshake, the pairing check, the
// transport, the gate, the encoder actually encoding, the decoder actually
// decoding. This runs it.
//
// # Two throwaway homes, and nothing of the user's touched
//
// A session needs two paired identities, and pairing writes to a device
// store. A diagnostic that paired something into the machine owner's own
// store — even briefly — would be a diagnostic that changes what it is
// diagnosing, and would leave a stranger's key in it if it were interrupted at
// the wrong moment. So both ends are temporary directories, created here and
// deleted afterwards, and the real store is never opened.
//
// The cost of that is what it does not test: whether *your* device list works.
// A machine that can hold a session with itself and refuses your laptop has a
// pairing problem, and this will say everything is fine. That limit is worth
// knowing and is why this is one section of the report rather than the whole
// of it.
//
// # Opt-in
//
// It starts four processes and takes several seconds. `esper doctor` is what
// somebody runs when something is wrong, and it should answer quickly; this is
// `esper doctor --session` for when the quick answers were all fine.
func selfTest(enginePath string, say sayer) int {
	hostHome, err := os.MkdirTemp("", "esper-doctor-host-")
	if err != nil {
		say.step("homes", false, fmt.Sprintf("nowhere to put a temporary identity: %v", err))
		return 1
	}
	defer func() { _ = os.RemoveAll(hostHome) }()
	viewerHome, err := os.MkdirTemp("", "esper-doctor-viewer-")
	if err != nil {
		say.step("homes", false, fmt.Sprintf("nowhere to put a temporary identity: %v", err))
		return 1
	}
	defer func() { _ = os.RemoveAll(viewerHome) }()

	ctx, cancel := context.WithTimeout(context.Background(), howLongASelfTestMayTake)
	defer cancel()

	began := time.Now()
	if err := pairTwo(ctx, enginePath, hostHome, viewerHome); err != nil {
		say.step("paired", false, fmt.Sprintf("the two ends could not pair: %v", err))
		say.warn("pairing is the first thing a session does, so nothing else here\nwould have worked either.")
		return 1
	}
	paired := time.Since(began)
	say.step("paired", true, fmt.Sprintf("two identities, in %s", took(paired)))
	say.figures(map[string]float64{"seconds": paired.Seconds()})

	// **`unattended`, on a store that exists for the next four seconds.**
	// Without it the host stops and asks whoever is at the machine to allow
	// the session, and there is nobody to ask — the person is watching
	// `esper doctor` run. `--yes` answers that question here, which is a
	// different act from answering it on behalf of a request that arrived
	// over the network: the store is a temporary directory this command made,
	// the device in it is this command's own, and both are deleted when it
	// returns.
	if err := allowUnattended(ctx, enginePath, hostHome); err != nil {
		say.step("allowed", false, fmt.Sprintf("the temporary host would not allow its own viewer: %v", err))
		return 1
	}

	// The host, on a port the kernel picks: a diagnostic that chose one would
	// fail on a machine where something else already had it.
	host := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"host", "--listen", "127.0.0.1:0", "--no-mdns", "--once")
	host.Env = append(os.Environ(), "ESPER_HOME="+hostHome)
	hostSaid, err := host.StdoutPipe()
	if err != nil {
		say.step("listening", false, fmt.Sprintf("could not read the host's output: %v", err))
		return 1
	}
	host.Stderr = nil
	if err := host.Start(); err != nil {
		say.step("listening", false, fmt.Sprintf("the host would not start: %v", err))
		return 1
	}
	defer func() {
		_ = host.Process.Kill()
		_ = host.Wait()
	}()

	// `\s+` and not a single space: the host aligns its output into columns,
	// so there are two of them, and a regex written from the sentence rather
	// than from the output does not match.
	listening := regexp.MustCompile(`listening\s+(?:on\s+)?(\S+:\d+)`)
	hostWatch := watch(hostSaid, listening)
	line := hostWatch.waitFor(ctx.Done())
	found := listening.FindStringSubmatch(line)
	addr := ""
	if len(found) == 2 {
		addr = found[1]
	}
	if addr == "" {
		say.step("listening", false, "the host never said what it was listening on")
		say.evidence(hostWatch.lines())
		return 1
	}
	say.step("listening", true, addr)

	opened := time.Now()
	viewer := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"connect", addr, "--no-mdns", "--seconds", fmt.Sprint(int(howLongToWatch.Seconds())))
	viewer.Env = append(os.Environ(), "ESPER_HOME="+viewerHome)
	out, err := viewer.CombinedOutput()
	said := string(out)
	if err != nil && !strings.Contains(said, "pictures") {
		say.step("session", false, fmt.Sprintf("the session did not open: %v", err))
		say.evidence(strings.Split(said, "\n"))
		return 1
	}

	shown, notShown := picturesIn(said)
	if shown == 0 {
		// **The host's own reason, not a guess at one.** The first version of
		// this printed "so it is the capture, the encoder or the decoder" —
		// which is a plausible sentence and was wrong the first time it ran:
		// the host had refused because it could not confirm the on-screen
		// indicator, which is none of those three. A diagnostic that invents
		// a cause is worse than one that reports a code.
		if why := endedBecause(said); why != "" {
			say.step("session", false, "the host ended it: "+why)
			// **And what the host itself said about it.** The code names the
			// kind of refusal; the host's own lines name the cause — which
			// screen, which permission, what was read back. Without them this
			// printed a number under a sentence calling it the thing to look
			// into.
			say.evidence(hostWatch.lines())
			say.warn("the two ends found each other and agreed a session, so the\nnetwork and the pairing are fine. The line above is the host\nsaying why it stopped, and is the thing to look into.")
			return 1
		}
		say.step("session", false, "the session opened and no picture ever arrived")
		say.evidence(strings.Split(said, "\n"))
		say.warn("the two ends found each other and agreed a session, and the host\ngave no reason for stopping — so this is the capture, the encoder\nor the decoder.")
		return 1
	}

	elapsed := time.Since(opened)
	say.step("session", true, fmt.Sprintf("%d picture(s) in %s, %d discarded", shown, took(elapsed), notShown))
	say.figures(map[string]float64{
		"pictures":  float64(shown),
		"discarded": float64(notShown),
		"seconds":   elapsed.Seconds(),
	})

	// **A low count on a still screen is the right answer, and has to say so.**
	// The damage gate sends nothing when nothing changes, so a machine with a
	// motionless desktop — which is every headless one, and this test's own
	// when it runs under a bare X server — legitimately produces one picture
	// in three seconds. Printing "0 pictures a second" beside a tick reads as
	// a failure that passed.
	if secs := elapsed.Seconds(); secs > 0 {
		rate := float64(shown) / secs
		if rate < 1.0 {
			say.step("rate", true, fmt.Sprintf("%.1f a second — the screen was not changing, which is\nwhat an idle desktop costs: the gate sends the periodic\nkeyframe and nothing else", rate))
		} else {
			say.step("rate", true, fmt.Sprintf("%.0f pictures a second, end to end on this machine", rate))
		}
		say.figures(map[string]float64{"a_second": rate})
	}
	say.verdict(true, "this machine can hold a session with itself: pairing, transport,\ncapture, encode and decode all work. What that does not prove is\nthat your own device list does — see `esper devices`.")
	return 0
}

// allowUnattended grants the one paired device everything the self test needs.
func allowUnattended(ctx context.Context, enginePath, hostHome string) error {
	// **The list as an object.** Scraping the printed one for the first
	// sixteen hex characters is a diagnostic that breaks the day somebody
	// changes an indent — and the thing it would then grant a permission to
	// is whatever else on the line happened to look like a fingerprint.
	list := exec.CommandContext(ctx, enginePath, "devices", "--json") // #nosec G204 -- the path is ours
	list.Env = append(os.Environ(), "ESPER_HOME="+hostHome)
	out, err := list.Output()
	if err != nil {
		return fmt.Errorf("reading the temporary device list: %w", err)
	}
	var listed struct {
		Devices []struct {
			Fingerprint struct {
				Hex string `json:"hex"`
			} `json:"fingerprint"`
		} `json:"devices"`
	}
	if err := json.Unmarshal(out, &listed); err != nil {
		return fmt.Errorf("the temporary device list could not be read: %w", err)
	}
	if len(listed.Devices) != 1 {
		return fmt.Errorf("the pairing left %d device(s) in the list, and it should leave one",
			len(listed.Devices))
	}
	who := listed.Devices[0].Fingerprint.Hex
	if who == "" {
		return fmt.Errorf("the device the pairing left has no fingerprint")
	}

	// `view` because the session is a screen session, `unattended` because
	// there is nobody to ask.
	grant := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"grant", who, "view", "unattended", "--yes")
	grant.Env = append(os.Environ(), "ESPER_HOME="+hostHome)
	if out, err := grant.CombinedOutput(); err != nil {
		return fmt.Errorf("%s", strings.TrimSpace(string(out)))
	}
	return nil
}

// pairTwo pairs two homes over loopback, the way a person would.
func pairTwo(ctx context.Context, enginePath, hostHome, viewerHome string) error {
	listener := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"pair", "--listen", "127.0.0.1:0", "--json")
	listener.Env = append(os.Environ(), "ESPER_HOME="+hostHome)
	said, err := listener.StdoutPipe()
	if err != nil {
		return fmt.Errorf("reading the pairing output: %w", err)
	}
	if err := listener.Start(); err != nil {
		return fmt.Errorf("starting the pairing listener: %w", err)
	}
	defer func() {
		_ = listener.Process.Kill()
		_ = listener.Wait()
	}()

	// **The offer as an object, not as the sentence a person reads.** The
	// listener's printed form is the command to run on the *other machine*,
	// so the host in it is a placeholder rather than an address anything can
	// dial — and a diagnostic parsing prose is the thing that breaks the day
	// somebody rewords it. Read rather than chosen: picking a port here would
	// race whatever else is on this machine, and inventing a code is not how
	// pairing works.
	anOffer := regexp.MustCompile(`"event"\s*:\s*"offering"`)
	offering := watch(said, anOffer)
	line := offering.waitFor(ctx.Done())
	if line == "" {
		return fmt.Errorf("no code was offered: %s", strings.Join(lastFew(offering.lines()), "; "))
	}
	var offer struct {
		Code string `json:"code"`
		Port int    `json:"port"`
	}
	if err := json.Unmarshal([]byte(line), &offer); err != nil {
		return fmt.Errorf("the pairing offer could not be read: %q", line)
	}
	if offer.Code == "" || offer.Port == 0 {
		return fmt.Errorf("the pairing offer is missing part of itself: %q", line)
	}

	joiner := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"pair", "127.0.0.1:"+strconv.Itoa(offer.Port), "--code", offer.Code)
	joiner.Env = append(os.Environ(), "ESPER_HOME="+viewerHome)
	if out, err := joiner.CombinedOutput(); err != nil {
		return fmt.Errorf("%s", strings.TrimSpace(string(out)))
	}
	return nil
}

/*
listener reads a child's output, for a match now and for its reasons later.

# Why it keeps reading after the match

Two things went wrong when it stopped. The obvious one is a **full pipe**: a
host writes a line for every permission, every screen and every refusal, and a
process whose output nobody is reading blocks once the kernel's buffer fills —
which looks from here like a host that hung.

The other is worse, because it was silent. The only line this waits for is
"listening", and everything the host says about *why it stopped* comes after
it. So a diagnostic whose whole purpose is to report the host's own reason was
throwing that reason away and printing a numeric code instead, right under a
sentence telling somebody it was the thing to look into.
*/
type listener struct {
	mu    sync.Mutex
	seen  []string
	found chan string
}

// How many of a child's lines to keep. Enough for the tail a person is shown,
// bounded because a session that ran for an hour must not be held in memory.
const mostLinesKept = 200

func watch(from io.Reader, what *regexp.Regexp) *listener {
	l := &listener{found: make(chan string, 1)}
	go func() {
		matched := false
		scanner := bufio.NewScanner(from)
		for scanner.Scan() {
			line := scanner.Text()
			l.mu.Lock()
			l.seen = append(l.seen, line)
			if len(l.seen) > mostLinesKept {
				l.seen = l.seen[len(l.seen)-mostLinesKept:]
			}
			l.mu.Unlock()
			if !matched && what.MatchString(line) {
				matched = true
				l.found <- line
			}
		}
		if !matched {
			l.found <- ""
		}
	}()
	return l
}

// waitFor returns the matching line, or "" if the output ended or time ran out.
func (l *listener) waitFor(done <-chan struct{}) string {
	select {
	case line := <-l.found:
		return line
	case <-done:
		return ""
	}
}

// lines is what has been read so far, copied so the reader can carry on.
func (l *listener) lines() []string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]string(nil), l.seen...)
}

// picturesIn reads the controller's own summary line.
var picturesLine = regexp.MustCompile(`(?m)^\s*pictures\s+(\d+) shown, (\d+) not shown`)

func picturesIn(said string) (int, int) {
	found := picturesLine.FindStringSubmatch(said)
	if len(found) != 3 {
		return 0, 0
	}
	shown, notShown := 0, 0
	_, _ = fmt.Sscanf(found[1], "%d", &shown)
	_, _ = fmt.Sscanf(found[2], "%d", &notShown)
	return shown, notShown
}

// endedBecause is the host's own reason for ending a session, when it gave one.
var hostEnded = regexp.MustCompile(`the host (?:ended the session|refused): (.+)`)

func endedBecause(said string) string {
	found := hostEnded.FindStringSubmatch(said)
	if len(found) != 2 {
		return ""
	}
	return strings.TrimSpace(found[1])
}

// took formats a duration the way a person reads one.
func took(d time.Duration) string {
	if d < time.Second {
		return fmt.Sprintf("%d ms", d.Milliseconds())
	}
	return fmt.Sprintf("%.1f s", d.Seconds())
}

// showSome prints the tail of what a process said, indented.
func showSome(lines []string) {
	for _, l := range lastFew(lines) {
		fmt.Printf("      %s\n", l)
	}
}

// lastFew is the last handful of non-empty lines.
func lastFew(lines []string) []string {
	var kept []string
	for _, l := range lines {
		if strings.TrimSpace(l) != "" {
			kept = append(kept, l)
		}
	}
	if len(kept) > 6 {
		kept = kept[len(kept)-6:]
	}
	return kept
}
