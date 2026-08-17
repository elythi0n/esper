/**
 * Tell the host when a screen has actually painted.
 *
 * # Why this exists at all
 *
 * "Cold start to a visible Devices screen under a second" is a promise about
 * something only the page can see. The process is up long before the window
 * is mapped, and the window is mapped before anything is on it — so a
 * measurement taken outside is measuring the wrong end of the thing somebody
 * feels.
 *
 * Once per document, after the browser has had a frame to draw in, and
 * nowhere near a network: the host prints it on stderr and forgets it.
 */
let told = false;

export function painted(screen: string): void {
  if (told) return;
  told = true;
  /*
    What the *system* asked for, alongside the timing.

    Not the in-app override, which is a preference; the media query, which is
    the desktop's own setting. It is here because this is the only place a
    shipped window reports on itself from the inside, and "reduced motion is
    honoured in the built app with the OS setting on" is otherwise a claim
    nobody can check without a person watching a screen.
  */
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const durations = getComputedStyle(document.documentElement);
  const base = durations.getPropertyValue("--dur-base").trim();
  /*
    And which theme the first frame is in.

    "The window opens in the right theme" is a claim about the *first* frame,
    and nothing outside the webview can see one — a screenshot taken later
    shows a window that may well have corrected itself. The element carries
    the answer because the inline script resolved it before the bundle ran.

    This is also what caught a bug nothing else would have: on a desktop
    asking for high contrast, in a home that had never been touched, the
    window reported `theme dark` — because merely opening Settings had
    written the default as though somebody had chosen it.
  */
  const theme = document.documentElement.dataset.theme || "dark";
  // Two frames: the first is scheduled before this render is composited, the
  // second runs after it. Reporting from the first would report the moment
  // the work was *queued*, which is always flattering and never true.
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      void fetch("/shell/painted", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ screen, reduced, base, theme }),
      }).catch(() => {
        // A browser tab has no host to tell. Nothing depends on this.
      });
    });
  });
}
