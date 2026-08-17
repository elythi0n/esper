package main

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"os/exec"
	"regexp"
	"strings"
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
func selfTest(enginePath string) int {
	fmt.Println("A session with itself")

	hostHome, err := os.MkdirTemp("", "esper-doctor-host-")
	if err != nil {
		fmt.Printf("  ✗ nowhere to put a temporary identity: %v\n", err)
		return 1
	}
	defer func() { _ = os.RemoveAll(hostHome) }()
	viewerHome, err := os.MkdirTemp("", "esper-doctor-viewer-")
	if err != nil {
		fmt.Printf("  ✗ nowhere to put a temporary identity: %v\n", err)
		return 1
	}
	defer func() { _ = os.RemoveAll(viewerHome) }()

	ctx, cancel := context.WithTimeout(context.Background(), howLongASelfTestMayTake)
	defer cancel()

	began := time.Now()
	if err := pairTwo(ctx, enginePath, hostHome, viewerHome); err != nil {
		fmt.Printf("  ✗ the two ends could not pair: %v\n", err)
		fmt.Println()
		fmt.Println("⚠  pairing is the first thing a session does, so nothing else here")
		fmt.Println("   would have worked either.")
		return 1
	}
	fmt.Printf("  ✓ paired    two identities, in %s\n", took(time.Since(began)))

	// **`unattended`, on a store that exists for the next four seconds.**
	// Without it the host stops and asks whoever is at the machine to allow
	// the session, and there is nobody to ask — the person is watching
	// `esper doctor` run. `--yes` answers that question here, which is a
	// different act from answering it on behalf of a request that arrived
	// over the network: the store is a temporary directory this command made,
	// the device in it is this command's own, and both are deleted when it
	// returns.
	if err := allowUnattended(ctx, enginePath, hostHome); err != nil {
		fmt.Printf("  ✗ the temporary host would not allow its own viewer: %v\n", err)
		return 1
	}

	// The host, on a port the kernel picks: a diagnostic that chose one would
	// fail on a machine where something else already had it.
	host := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"host", "--listen", "127.0.0.1:0", "--no-mdns", "--once")
	host.Env = append(os.Environ(), "ESPER_HOME="+hostHome)
	hostSaid, err := host.StdoutPipe()
	if err != nil {
		fmt.Printf("  ✗ could not read the host's output: %v\n", err)
		return 1
	}
	host.Stderr = nil
	if err := host.Start(); err != nil {
		fmt.Printf("  ✗ the host would not start: %v\n", err)
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
	line, hostLines := waitForMatch(hostSaid, listening, ctx.Done())
	found := listening.FindStringSubmatch(line)
	addr := ""
	if len(found) == 2 {
		addr = found[1]
	}
	if addr == "" {
		fmt.Println("  ✗ the host never said what it was listening on")
		showSome(hostLines)
		return 1
	}
	fmt.Printf("  ✓ listening %s\n", addr)

	opened := time.Now()
	viewer := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"connect", addr, "--no-mdns", "--seconds", fmt.Sprint(int(howLongToWatch.Seconds())))
	viewer.Env = append(os.Environ(), "ESPER_HOME="+viewerHome)
	out, err := viewer.CombinedOutput()
	said := string(out)
	if err != nil && !strings.Contains(said, "pictures") {
		fmt.Printf("  ✗ the session did not open: %v\n", err)
		showSome(strings.Split(said, "\n"))
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
			fmt.Printf("  ✗ the host ended it: %s\n", why)
			fmt.Println()
			fmt.Println("⚠  the two ends found each other and agreed a session, so the")
			fmt.Println("   network and the pairing are fine. The line above is the host")
			fmt.Println("   saying why it stopped, and is the thing to look into.")
			return 1
		}
		fmt.Println("  ✗ the session opened and no picture ever arrived")
		showSome(strings.Split(said, "\n"))
		fmt.Println()
		fmt.Println("⚠  the two ends found each other and agreed a session, and the host")
		fmt.Println("   gave no reason for stopping — so this is the capture, the encoder")
		fmt.Println("   or the decoder.")
		return 1
	}

	elapsed := time.Since(opened)
	fmt.Printf("  ✓ session   %d picture(s) in %s, %d discarded\n", shown, took(elapsed), notShown)

	// **A low count on a still screen is the right answer, and has to say so.**
	// The damage gate sends nothing when nothing changes, so a machine with a
	// motionless desktop — which is every headless one, and this test's own
	// when it runs under a bare X server — legitimately produces one picture
	// in three seconds. Printing "0 pictures a second" beside a tick reads as
	// a failure that passed.
	if secs := elapsed.Seconds(); secs > 0 {
		rate := float64(shown) / secs
		if rate < 1.0 {
			fmt.Printf("  ✓ rate      %.1f a second — the screen was not changing, which is\n", rate)
			fmt.Printf("              what an idle desktop costs: the gate sends the periodic\n")
			fmt.Printf("              keyframe and nothing else\n")
		} else {
			fmt.Printf("  ✓ rate      %.0f pictures a second, end to end on this machine\n", rate)
		}
	}
	fmt.Println()
	fmt.Println("✓ this machine can hold a session with itself: pairing, transport,")
	fmt.Println("  capture, encode and decode all work. What that does not prove is")
	fmt.Println("  that your own device list does — see `esper devices`.")
	return 0
}

// allowUnattended grants the one paired device everything the self test needs.
func allowUnattended(ctx context.Context, enginePath, hostHome string) error {
	list := exec.CommandContext(ctx, enginePath, "devices") // #nosec G204 -- the path is ours
	list.Env = append(os.Environ(), "ESPER_HOME="+hostHome)
	out, err := list.Output()
	if err != nil {
		return fmt.Errorf("reading the temporary device list: %w", err)
	}
	found := fingerprintLine.FindStringSubmatch(string(out))
	if len(found) != 2 {
		return fmt.Errorf("the pairing left no device in the list")
	}

	// `view` because the session is a screen session, `unattended` because
	// there is nobody to ask.
	grant := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"grant", found[1], "view", "unattended", "--yes")
	grant.Env = append(os.Environ(), "ESPER_HOME="+hostHome)
	if out, err := grant.CombinedOutput(); err != nil {
		return fmt.Errorf("%s", strings.TrimSpace(string(out)))
	}
	return nil
}

// The first fingerprint in `esper-engine devices` output.
var fingerprintLine = regexp.MustCompile(`(?m)^\s+([0-9a-f]{16})\b`)

// pairTwo pairs two homes over loopback, the way a person would.
func pairTwo(ctx context.Context, enginePath, hostHome, viewerHome string) error {
	listener := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"pair", "--listen", "127.0.0.1:0")
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

	// The listener prints the command to run at the other end, which carries
	// both the port it took and the code it generated. Read rather than
	// chosen: picking a port here would race whatever else is on this machine,
	// and inventing a code is not how pairing works.
	// **The port from the offer, and loopback for the host part.** What the
	// listener prints is the command to run on the *other machine*, so the
	// host in it is a placeholder — `<this-machine>` — rather than an address
	// anything can dial. The port is real and is the only part needed here.
	invite := regexp.MustCompile(`pair \S*:(\d+) --code ([A-Z0-9-]+)`)
	line, lines := waitForMatch(said, invite, ctx.Done())
	if line == "" {
		return fmt.Errorf("no code was offered: %s", strings.Join(lastFew(lines), "; "))
	}
	found := invite.FindStringSubmatch(line)
	if len(found) != 3 {
		return fmt.Errorf("the pairing offer could not be read: %q", line)
	}

	joiner := exec.CommandContext(ctx, enginePath, // #nosec G204 -- the path is ours
		"pair", "127.0.0.1:"+found[1], "--code", found[2])
	joiner.Env = append(os.Environ(), "ESPER_HOME="+viewerHome)
	if out, err := joiner.CombinedOutput(); err != nil {
		return fmt.Errorf("%s", strings.TrimSpace(string(out)))
	}
	return nil
}

// waitForMatch reads lines until one matches, and returns it with everything
// read so far.
//
// Everything read is returned even on success, because the lines before a
// match are what a person needs when the match was the wrong one.
func waitForMatch(from interface{ Read([]byte) (int, error) }, what *regexp.Regexp, done <-chan struct{}) (string, []string) {
	var seen []string
	found := make(chan string, 1)
	go func() {
		scanner := bufio.NewScanner(from)
		for scanner.Scan() {
			line := scanner.Text()
			seen = append(seen, line)
			if what.MatchString(line) {
				found <- line
				return
			}
		}
		found <- ""
	}()
	select {
	case line := <-found:
		return line, seen
	case <-done:
		return "", seen
	}
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
