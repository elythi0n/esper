import { Section, Sub } from "./ui";

const baseline = [
  { rule: "Keyboard reachable, in reading order", how: "Every control is a button, an input or a Radix primitive with its own keyboard model. Radio groups use arrow keys, and a menubar is one tab stop. pnpm check:keyboard tabs through the real shell and checks that focus follows the order on screen." },
  { rule: "Focus comes back", how: "An overlay takes focus, keeps Tab inside it, and returns focus to the control that opened it. Many overlays here have no trigger (the palette opens from ⌘K), so useReturnFocus handles that case for all of them." },
  { rule: "Every control has a name", how: "pnpm check:a11y reads Chromium's accessibility tree for every control on this page (299 of them) and fails on any without a name. It catches the usual mistake: an icon button with no label." },
  { rule: "Visible focus", how: "One treatment, :focus-visible, --focus-width ring at --focus-offset, never removed. 3px in high contrast. Tab through this page to see it on everything." },
  { rule: "AA contrast, AAA in high contrast", how: "Checked twice. scripts/check-contrast.mjs checks the palette in the token files (159 pairings across three themes). pnpm check:ink checks the rendered page: each piece of text against the colour it is actually drawn on. Disabled text, translucent text and text over video are reported, not failed." },
  { rule: "Nothing runs off the side, nothing is cut off", how: "pnpm check:overflow renders all seven routes at 380, 900, 1400 and 2560 pixels wide in three themes. It checks that the page never scrolls sideways, that no text is cut off without an ellipsis, and that no line of text is longer than 108 characters." },
  { rule: "Reduced motion", how: "The OS setting and the override above both set every duration to 0 and cut third-party keyframes to 0.01ms. It applies before first paint, and pnpm check:frames measures it in a real browser." },
  { rule: "No colour-only meaning", how: "Tier is icon and word. Reachability is dot and word. A toned HUD value carries a word. Callouts carry an icon and a title." },
  { rule: "--tap-min respected", how: "A control that is small in both directions carries data-tap-min and gets a 44px hit area. Short but wide controls, like 28px rows, are counted separately by check:a11y. The one exemption is the 32px title bar, which matches the OS." },
  { rule: "Every icon has a text label", how: "Beside it, or as a Tooltip that is also the aria-label. lucide icons are aria-hidden when a label is beside them." },
];

const content = [
  "Sentence case, everywhere. Titles too.",
  "Buttons name the action: Connect, Forget, Allow, Deny. Never OK, never Yes.",
  "Errors say what to do. The engine's own detail goes underneath, smaller.",
  "No exclamation marks. No \"Oops\". No \"Something went wrong\" without a next step.",
  "Numbers are tabular and carry their unit. Times are relative and update in place, with the absolute time as the title.",
  "A device shows the name its owner gave it, or its fingerprint once forgotten.",
];

const gates = [
  { name: "pnpm check:contrast", does: "Every pairing, every theme, from the token files. Fails below AA on required rows, AAA for text in high contrast." },
  { name: "pnpm check:discipline", does: "No raw colour, z-index, breakpoint or duration anywhere in src/ outside the token files." },
  { name: "pnpm check:motion", does: "src/motion.ts matches the CSS it was generated from." },
  { name: "pnpm check:presence", does: "presence.css matches presence.contract.ts: size, ring, tones, contrast against video, no motion, no theme." },
  { name: "pnpm check:inventory", does: "Every Tier 1 component named in the inventory resolves to a real export, so the list cannot describe something that was renamed." },
  { name: "pnpm check:words", does: "Checks the content rules on the rendered page, where text is fully assembled. It flags buttons that name no action (OK, Yes), exclamation marks, failures with no next step, Title Case headings, and numbers that are not tabular. Component names and acronyms are ignored." },
  { name: "pnpm check:keyboard", does: "Two passes. For every overlay: focus moves inside, Tab stays inside, Escape closes it, and focus returns to the control that opened it. Then it checks the shell's tab order matches the order on screen: title bar, alarm, sidebar, topbar, page." },
  { name: "pnpm check:a11y", does: "Reads Chromium's accessibility tree and fails on any control without a name. It also checks the tap minimum and lists each exemption with its reason. It needs a browser, so it runs in pnpm verify." },
  { name: "pnpm check:identicon", does: "Draws four thousand keys, and sixteen thousand keys one character away from them, to check that a changed key always looks different." },
  { name: "pnpm check:overflow", does: "Renders every route at four widths in three themes (84 renders). It checks for sideways scrolling, text cut off without an ellipsis, lines too long to read, and that the topbar's edge appears when content scrolls under it." },
  { name: "pnpm check:ink", does: "Measures the contrast of on-screen text against its actual background, on all seven routes in all three themes. Disabled text, translucent text and text over video are reported, not failed." },
  { name: "pnpm check:viewer", does: "Tests the viewer's video path three ways. It decodes the committed stream with the page's own reader and the browser's VideoDecoder, in both Chromium and WebKitGTK (the shipped window uses WebKit). It runs the real engine live and measures latency (about 1ms at the median). And it sends keys, pointer moves, clicks and scrolls, and checks they decode the way the engine expects." },
  { name: "pnpm check:live", does: "Checks four things the browser actually resolves: the session layer is dark in all three themes, the focus ring is 2px (3px in high contrast), a select opens above a dialog, and only three font faces load." },
  { name: "pnpm check:frames", does: "Runs all five named motions in a real browser. Each animation must land within 12ms of its token, and at 0ms with motion reduced. It reports the worst frame. It needs a build, so it runs in pnpm verify." },
];

export function A11ySection() {
  return (
    <Section
      id="a11y"
      title="Baseline"
      lede="What every screen gets for free, and the scripts that check it."
    >
      <div className="grid gap-8 lg:grid-cols-2">
        <Sub title="Accessibility">
          <dl className="flex flex-col divide-y divide-border">
            {baseline.map((b) => (
              <div key={b.rule} className="grid gap-1 py-3 sm:grid-cols-[10rem_1fr] sm:gap-4">
                <dt className="text-sm font-medium text-text">{b.rule}</dt>
                <dd className="text-sm text-text-2">{b.how}</dd>
              </div>
            ))}
          </dl>
        </Sub>
        <div className="flex flex-col gap-8">
          <Sub title="Content">
            {/*
              **This list quotes the rules**, one of which is "no exclamation
              marks" and another "no Oops" — so a check reading the rendered
              page finds them here and would call stating a rule a violation
              of it. `check-words.mjs` skips this container by the attribute
              rather than by section name, so the exemption is visible where
              somebody reading the markup will see it.
            */}
            <ul data-quotes-rules="" className="flex flex-col gap-2 text-sm text-text-2">
              {content.map((c) => (
                <li key={c} className="flex gap-2"><span aria-hidden className="mt-2 size-1 shrink-0 rounded-full bg-text-3" />{c}</li>
              ))}
            </ul>
          </Sub>
          <Sub title="Checked by" about="pnpm check runs the fast gates; pnpm verify adds the ones that need a browser.">
            <ul className="flex flex-col divide-y divide-border rounded-md border border-border bg-surface-1">
              {gates.map((g) => (
                <li key={g.name} className="flex flex-col gap-0.5 px-4 py-3">
                  <code className="font-mono text-xs text-text">{g.name}</code>
                  <span className="text-sm text-text-2">{g.does}</span>
                </li>
              ))}
            </ul>
          </Sub>
        </div>
      </div>
    </Section>
  );
}
