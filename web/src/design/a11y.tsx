import { Section, Sub } from "./ui";

const baseline = [
  { rule: "Keyboard reachable, in reading order", how: "Every control is a button, an input or a Radix primitive with its own keyboard model. Radio groups take arrow keys; a menubar is one tab stop and arrows move within it. Measured: pnpm check:keyboard tabs through the real shell and asserts the bands come in the order they are drawn, because the way that breaks is never deliberate — a portal rendered at the end of the document, an absolutely-positioned control that kept its DOM position — and none of it is visible on screen." },
  { rule: "Focus comes back", how: "An overlay takes focus, holds it against Tab, and returns it to the control that opened it. Radix returns focus to the *trigger*, which is nothing when there is no trigger — and half the overlays here are opened by state: the palette from ⌘K, the sheet from ?, a confirmation from a row menu whose row is about to vanish. Closing left focus on <body> and the next Tab started from the top of the document. One implementation, useReturnFocus, and a check that found it broken on two of four." },
  { rule: "Every control has a name", how: "Measured, and by the browser rather than by a regex: an accessible name comes from a specified algorithm over labels, aria-labelledby, title and content, and reimplementing it would be reimplementing it wrongly. pnpm check:a11y reads Chromium's own accessibility tree for every control on this page — 299 of them — and fails on one without a name. It catches the way this actually breaks: an icon button somebody added in a hurry, which looks completely fine." },
  { rule: "Visible focus", how: "One treatment, :focus-visible, --focus-width ring at --focus-offset, never removed. 3px in high contrast. Tab through this page to see it on everything." },
  { rule: "AA contrast, AAA in high contrast", how: "Measured twice, because the two answer different questions. scripts/check-contrast.mjs reads the token files — 159 pairings across three themes — and proves the palette is sound. pnpm check:ink reads the rendered page and proves the components used it: it takes the colour of every piece of text and the colour of the ground it is actually painted on, composited, and computes the ratio the specification defines. A muted foreground on a muted fill is two good tokens making unreadable text, and only the second one can see it. Required rows fail; disabled text, anything translucent and anything over video are watched and reported." },
  { rule: "Nothing runs off the side, nothing is cut off", how: "A layout fault is only a fault at a size, and nothing was looking at sizes: the window had grown a horizontal scrollbar and every other gate here passed. pnpm check:overflow renders all seven routes at 380, 900, 1400 and 2560 in three themes and asserts the page does not scroll sideways, that no element hiding its overflow has cut its own text off without an ellipsis, and that no line of running text is set past 108 characters. It found a 644px scrollbar on this page at phone width — two flex rows whose automatic minimum size is their content, which max-width could never bound — and an alarm band that stopped shrinking at 560px." },
  { rule: "Reduced motion", how: "The OS setting and the override above both set every duration to 0 and cut third-party keyframes to 0.01ms. Applied before first paint by the inline script in index.html, and measured in a real browser rather than asserted — see pnpm check:frames." },
  { rule: "No colour-only meaning", how: "Tier is icon and word. Reachability is dot and word. A toned HUD value carries a word. Callouts carry an icon and a title." },
  { rule: "--tap-min respected", how: "A control small in *both* directions carries data-tap-min and gets a 44px hit area from ::after — which is what a hit area is for: a control with no long axis to grab. A 176×28 link and a 28px row are short by density, on purpose, and demanding 44px of vertical from every row would be demanding a different product; check:a11y counts those separately rather than hiding them. One exemption, and it prints its own reason: a title bar is 32px because the OS draws one at 32px, and an ::after that big would swallow clicks meant for the screen below." },
  { rule: "Every icon has a text label", how: "Beside it, or as a Tooltip that is also the aria-label. lucide icons are aria-hidden when a label is beside them." },
];

const content = [
  "Sentence case, everywhere. Titles too.",
  "Buttons name the action: Connect, Forget, Allow, Deny. Never OK, never Yes.",
  "Errors say what to do. The engine's own detail goes underneath, smaller.",
  "No exclamation marks. No \"Oops\". No \"Something went wrong\" without a next step.",
  "Numbers are tabular and carry their unit. Times are relative and update in place, with the absolute time as the title.",
  "A device is called what its owner called it, and by its fingerprint where it has been forgotten.",
];

const gates = [
  { name: "pnpm check:contrast", does: "Every pairing, every theme, from the token files. Fails below AA on required rows, AAA for text in high contrast." },
  { name: "pnpm check:discipline", does: "No raw colour, z-index, breakpoint or duration anywhere in src/ outside the token files." },
  { name: "pnpm check:motion", does: "src/motion.ts matches the CSS it was generated from." },
  { name: "pnpm check:presence", does: "presence.css matches presence.contract.ts — size, ring, tones, contrast against video, no motion, no theme." },
  { name: "pnpm check:inventory", does: "Every Tier 1 component named in the inventory resolves to a real export, so the list cannot describe something that was renamed." },
  { name: "pnpm check:words", does: "The content rules, read off the rendered page rather than grepped from the source — copy is assembled, and what a person reads is the composition. Five of the six are checkable: a control that names no action (never OK, never Yes), an exclamation mark, a sentence that reports a failure and offers nothing, Title Case in a heading, and the tabular figures. That last one had to be turned around: the root sets font-variant-numeric and it inherits, so asking whether a number is tabular can only ever pass — proved by setting Figure in a proportional font and watching all 108 numbers still resolve tabular. What can break is somebody turning it off, so that is what is checked. The sentence-case rule ignores phrase boundaries, identifiers like TierBadge, and acronyms, because a rule that cries wolf on a component name is a rule somebody turns off." },
  { name: "pnpm check:keyboard", does: "Two passes. Every overlay the gate offers: focus moved inside, Tab cannot walk out, Escape closes it, and focus came back to the control that opened it — the last is the one that regresses, is invisible to every other gate, and was broken on two of four the first time this ran. Then the shell's tab order on a real screen, asserting the bands come in the order they are drawn: chrome, alarm, rail, topbar, page. A window whose tab order is not its reading order has to be explored rather than read." },
  { name: "pnpm check:a11y", does: "Reads Chromium's accessibility tree for every control the gate renders and fails on one the browser could not name — plus the tap minimum, with its exemptions counted and their reasons printed. Needs a browser, so it runs in pnpm verify." },
  { name: "pnpm check:identicon", does: "Four thousand keys and sixteen thousand one-character neighbours through the fingerprint drawing, because 'a changed key looks different' is a claim about pairs that no gallery of five can support. It found a solid block one key in eighty on its first run." },
  { name: "pnpm check:overflow", does: "Every route at four widths in three themes — 84 renders — for the faults that only exist at a size: the page scrolling sideways, text cut off by an ancestor with nothing saying so, a line of prose too long to read, and the topbar's scroll boundary actually appearing when content is behind it (a :has(+ …) selector cannot be verified by reading it). Every finding on its first run was real, including a 644px horizontal scrollbar on this page at phone width." },
  { name: "pnpm check:ink", does: "The contrast of the text that is on screen, against the ground it is painted on, on all seven routes in all three themes. The token checker proves the palette; this proves the components used it. Watched rather than failed: disabled text, anything with opacity in the stack — the effective ink then depends on the pixel behind it, and this measures colours — and text over video, which is somebody else's screen and has its own contract." },
  { name: "pnpm check:viewer", does: "The viewer's frame path, three ways, ending in a latency nobody had measured. The stream a window's decoder reads is written in Rust and parsed in TypeScript — two implementations of one format, which is what the shared fixtures exist to catch — so this parses the committed stream with the page's own reader and puts every picture through the browser's own VideoDecoder. It runs in Chromium and in WebKitGTK, because the shipped window is not Chromium: the spike behind this path found that WebKit refuses Annex B outright and takes only length-prefixed AVCC with an avcC description, so a gate that ran in Chromium alone would pass on a stream the product's own window cannot play. And a third leg runs the real engine live — a loopback capture writing to a socket the check serves as a chunked body — because the hop this path adds is only a latency when something is actually sending: read out of a file it is the age of the file, which is 184 seconds for the committed fixture and looks exactly like a number somebody would quote. Live it is 1ms at the median from handed to drawn. And it drives the surface as well as watching it: the page's own hand, on the page's own canvas, with the bytes decoded on the other side exactly the way the engine decodes them — a key as usage 0x04, a pointer move, a button, a scroll, and the release that a surface which stops driving always sends." },
  { name: "pnpm check:live", does: "Four claims that are about what the browser *resolves*, not what the tokens say. The session layer is dark under all three themes — read from the value layer as the session element resolves it, against the same token on the document, because the session root paints its own ground with a class and reading that passed a sabotage which disabled the session's token block entirely. Then the ring at 2px and 3px, a popover above a modal driven by opening a select inside a dialog, and three faces loaded with nothing on screen asking for a fourth." },
  { name: "pnpm check:frames", does: "Drives all five named motions in a real browser and reads back what it was told and what it managed: every animation within 12ms of its token, 0ms with motion reduced, and the worst single frame reported. Needs a build, so it runs in pnpm verify rather than pnpm check." },
];

export function A11ySection() {
  return (
    <Section
      id="a11y"
      title="Baseline"
      lede="What every screen built on this system gets without asking, and what a script checks so a review does not have to."
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
          <Sub title="Checked by" about="pnpm check runs the ones that need no browser; pnpm verify adds the ones that do. A gate people work around is a gate nobody has, so each has a floor under its parser and refuses to pass over an empty table — and each of the last four found something real the first time it ran.">
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
