import { Demo, Section, Sub, Swatch, TokenName, TokenTable, useTokens } from "./ui";
import { cn } from "@/lib/cn";

const colourTokens = [
  "--color-surface-0", "--color-surface-1", "--color-surface-2", "--color-video",
  "--color-text", "--color-text-2", "--color-text-3", "--color-text-disabled",
  "--color-accent", "--color-accent-fg", "--color-border", "--color-border-strong",
  "--color-focus", "--color-selection", "--color-signal", "--color-signal-fg",
  "--color-ok", "--color-ok-fg", "--color-ok-soft",
  "--color-warn", "--color-warn-fg", "--color-warn-soft",
  "--color-danger", "--color-danger-fg", "--color-danger-soft",
  "--color-info", "--color-info-fg", "--color-info-soft",
] as const;

const sizingRows = [
  { token: "--row-h", note: "A table or list row" },
  { token: "--control-h", note: "Buttons, inputs, selects" },
  { token: "--topbar-h", note: "The session chrome and the app topbar" },
  { token: "--tap-min", note: "The smallest hit area, whatever is drawn" },
  { token: "--icon-sm" }, { token: "--icon-md" }, { token: "--icon-lg" }, { token: "--icon-xl" },
  { token: "--focus-width", note: "3px in high contrast" },
  { token: "--focus-offset" },
];

const layerRows = [
  { token: "--z-base" }, { token: "--z-sticky" }, { token: "--z-dropdown" }, { token: "--z-overlay" },
  { token: "--z-modal" }, { token: "--z-popover", note: "Above the modal: a select inside a dialog is the common case" },
  { token: "--z-toast" }, { token: "--z-session-chrome", note: "Nothing covers a session" }, { token: "--z-session-hud" },
];

const breakpointRows = [
  { token: "--breakpoint-sm" }, { token: "--breakpoint-md" }, { token: "--breakpoint-lg" }, { token: "--breakpoint-xl" },
];

const radiusRows = [{ token: "--radius-sm" }, { token: "--radius-md" }, { token: "--radius-lg" }, { token: "--radius-full" }];

const shadowTokens = ["--shadow-1", "--shadow-2", "--shadow-3"] as const;

const allTokens = [
  ...colourTokens,
  ...sizingRows.map((r) => r.token),
  ...layerRows.map((r) => r.token),
  ...breakpointRows.map((r) => r.token),
  ...radiusRows.map((r) => r.token),
  ...shadowTokens,
  "--measure",
  "--measure-short",
];

type Tone = "ok" | "warn" | "danger" | "info";
const toneClasses: Record<Tone, { fill: string; soft: string }> = {
  ok: { fill: "bg-ok text-ok-fg", soft: "bg-ok-soft text-ok" },
  warn: { fill: "bg-warn text-warn-fg", soft: "bg-warn-soft text-warn" },
  danger: { fill: "bg-danger text-danger-fg", soft: "bg-danger-soft text-danger" },
  info: { fill: "bg-info text-info-fg", soft: "bg-info-soft text-info" },
};

const typeScale = [
  { cls: "text-xs", token: "--text-xs", use: "Metadata, badges, hints" },
  { cls: "text-sm", token: "--text-sm", use: "Controls, tables, most UI" },
  { cls: "text-base", token: "--text-base", use: "Body" },
  { cls: "text-lg", token: "--text-lg", use: "Card and dialog titles" },
  { cls: "text-xl", token: "--text-xl", use: "Section titles" },
  { cls: "text-2xl", token: "--text-2xl", use: "Page titles" },
  { cls: "text-3xl", token: "--text-3xl", use: "The one heading on a view" },
];

const spaces = [1, 2, 3, 4, 6, 8, 12, 16] as const;
const spaceClass: Record<(typeof spaces)[number], string> = {
  1: "w-1", 2: "w-2", 3: "w-3", 4: "w-4", 6: "w-6", 8: "w-8", 12: "w-12", 16: "w-16",
};

export function Foundations() {
  const v = useTokens(allTokens);
  return (
    <Section
      id="foundations"
      title="Foundations"
      lede="Raw values, named roles, and the components that use them. Change the theme above and every value updates."
    >
      <Sub title="Surfaces" about="Three surfaces, lightest on top. Video sits behind everything.">
        <Demo className="items-stretch">
          <Swatch token="--color-surface-0" className="bg-surface-0" note="The canvas" value={v["--color-surface-0"]} />
          <Swatch token="--color-surface-1" className="bg-surface-1" note="Cards, raised" value={v["--color-surface-1"]} />
          <Swatch token="--color-surface-2" className="bg-surface-2" note="Popovers, controls at rest" value={v["--color-surface-2"]} />
          <Swatch token="--color-video" className="bg-video" note="Behind a letterboxed frame" value={v["--color-video"]} />
        </Demo>
      </Sub>

      <Sub title="Text" about="Three levels of text and one for disabled. Nothing is coloured just to draw the eye.">
        <Demo className="flex-col items-stretch gap-2">
          {(["text", "text-2", "text-3", "text-disabled"] as const).map((t) => (
            <div key={t} className="flex items-baseline justify-between gap-4 border-b border-border pb-2 last:border-0">
              <span className={cn("text-base", `text-${t}`)}>Connect to a machine you already trust, in one click.</span>
              <span className="flex flex-col items-end">
                <TokenName>--color-{t}</TokenName>
                <span className="font-mono text-xs text-text-3">{v[`--color-${t}`]}</span>
              </span>
            </div>
          ))}
          <div className="mt-2 flex items-center gap-3">
            <span className="inline-flex h-[var(--control-h)] items-center rounded-sm bg-accent px-3 text-sm font-medium text-accent-fg">Inverse fill</span>
            <TokenName>--color-accent / --color-accent-fg</TokenName>
          </div>
        </Demo>
      </Sub>

      <Sub title="Borders, focus, selection" about="A faint hairline between areas, and a stronger border around controls.">
        <Demo className="items-stretch">
          <div className="flex min-w-36 flex-1 flex-col gap-2">
            <div className="h-14 rounded-sm border border-border bg-surface-1" />
            <TokenName>--color-border</TokenName>
            <span className="font-mono text-xs text-text-2">{v["--color-border"]}</span>
          </div>
          <div className="flex min-w-36 flex-1 flex-col gap-2">
            <div className="h-14 rounded-sm border border-border-strong bg-surface-0" />
            <TokenName>--color-border-strong</TokenName>
            <span className="font-mono text-xs text-text-2">{v["--color-border-strong"]}</span>
          </div>
          <div className="flex min-w-36 flex-1 flex-col gap-2">
            <div className="h-14 rounded-sm bg-surface-1 outline outline-[length:var(--focus-width)] outline-offset-[var(--focus-offset)] outline-focus" />
            <TokenName>--color-focus</TokenName>
            <span className="font-mono text-xs text-text-2">{v["--color-focus"]} · {v["--focus-width"]}</span>
          </div>
          <div className="flex min-w-36 flex-1 flex-col gap-2">
            <div className="grid h-14 place-items-center rounded-sm bg-surface-1 text-sm">
              <span className="bg-selection px-1">selected text</span>
            </div>
            <TokenName>--color-selection</TokenName>
            <span className="font-mono text-xs text-text-2">{v["--color-selection"]}</span>
          </div>
        </Demo>
      </Sub>

      <Sub title="Tones" about="Each tone comes as a solid fill and as a soft tint.">
        <Demo className="items-stretch">
          {(Object.keys(toneClasses) as Tone[]).map((t) => (
            <div key={t} className="flex min-w-40 flex-1 flex-col gap-2">
              <div className={cn("grid h-10 place-items-center rounded-sm text-sm font-medium", toneClasses[t].fill)}>{t} on fill</div>
              <div className={cn("grid h-10 place-items-center rounded-sm border text-sm [border-color:currentColor] [border-width:var(--soft-border-width)]", toneClasses[t].soft)}>
                {t} on soft
              </div>
              <TokenName>--color-{t} · -fg · -soft</TokenName>
              <span className="truncate font-mono text-xs text-text-3" title={v[`--color-${t}`]}>{v[`--color-${t}`]}</span>
            </div>
          ))}
        </Demo>
      </Sub>

      <Sub title="The signal" about="Amber, used once per view. Relayed sessions wear it.">
        <Demo>
          <span className="inline-flex h-5 items-center rounded-sm bg-signal px-1.5 text-xs font-medium text-signal-fg">Relayed</span>
          <span aria-hidden className="size-2 rounded-full bg-signal" />
          <TokenName>--color-signal / --color-signal-fg</TokenName>
          <span className="font-mono text-xs text-text-3">{v["--color-signal"]}</span>
        </Demo>
      </Sub>

      <Sub title="Type" about="Inter for the interface, JetBrains Mono for anything copied or compared, Space Grotesk for headings. The same faces as esper.st.">
        <Demo className="flex-col items-stretch gap-0 p-0">
          {typeScale.map((t) => (
            <div key={t.cls} className="flex flex-wrap items-baseline justify-between gap-x-6 gap-y-1 border-b border-border px-5 py-3 last:border-0">
              <span className={cn(t.cls, "text-text")}>The screen is being viewed</span>
              <span className="flex items-baseline gap-3">
                <TokenName>{t.token}</TokenName>
                <span className="text-xs text-text-3">{t.use}</span>
              </span>
            </div>
          ))}
          <div className="flex flex-wrap items-baseline justify-between gap-x-6 gap-y-1 px-5 py-4">
            <span className="font-mono text-code text-text tabular-nums">482 917</span>
            <span className="flex items-baseline gap-3">
              <TokenName>--text-code</TokenName>
              <span className="text-xs text-text-3">The pairing code. Read across a room.</span>
            </span>
          </div>
        </Demo>
        <Demo className="items-baseline gap-8">
          <span className="font-sans text-lg">Inter: the interface <TokenName>--font-sans</TokenName></span>
          <span className="font-mono text-lg">JetBrains Mono: 3a9f c1e2 <TokenName>--font-mono</TokenName></span>
          <span className="font-display text-lg font-medium tracking-[-0.02em]">Space Grotesk: the headings <TokenName>--font-display</TokenName></span>
        </Demo>
      </Sub>

      <Sub title="Space" about="Everything sits on a 4px grid.">
        <Demo className="items-end gap-4">
          {spaces.map((s) => (
            <div key={s} className="flex flex-col items-center gap-1">
              <div className={cn("h-6 rounded-[2px] bg-text-3", spaceClass[s])} />
              <span className="font-mono text-xs text-text-3">{s}</span>
            </div>
          ))}
        </Demo>
      </Sub>

      <Sub
        title="Measure"
        about="Running text stops at a readable width. Centred text stops sooner."
      >
        <Demo className="flex-col items-stretch gap-5">
          <div className="flex flex-col gap-1.5">
            <TokenName>--measure ({v["--measure"]})</TokenName>
            <p className="max-w-[var(--measure)] text-sm text-text-2">
              A device shows the name its owner gave it, or its fingerprint once it has been forgotten.
              The name comes from the device itself, so when a device is renamed, this window updates
              on its own.
            </p>
          </div>
          <div className="flex flex-col items-center gap-1.5 text-center">
            <TokenName>--measure-short ({v["--measure-short"]})</TokenName>
            <p className="max-w-[var(--measure-short)] text-sm text-text-2">
              A device shows the name its owner gave it, or its fingerprint once forgotten.
            </p>
          </div>
        </Demo>
      </Sub>

      <div className="grid gap-8 lg:grid-cols-2">
        <Sub title="Radius">
          <Demo className="items-end">
            {radiusRows.map((r) => (
              <div key={r.token} className="flex flex-col items-center gap-1">
                <div className="size-14 border border-border-strong bg-surface-1" style={{ borderRadius: `var(${r.token})` }} />
                <TokenName>{r.token}</TokenName>
                <span className="font-mono text-xs text-text-3">{v[r.token]}</span>
              </div>
            ))}
          </Demo>
        </Sub>
        <Sub title="Elevation" about="Four levels of depth. High contrast turns them into outlines.">
          <Demo className="items-end gap-6 py-8">
            {shadowTokens.map((s) => (
              <div key={s} className="flex flex-col items-center gap-2">
                <div className="size-14 rounded-md bg-surface-1" style={{ boxShadow: `var(${s})` }} />
                <TokenName>{s}</TokenName>
              </div>
            ))}
          </Demo>
        </Sub>
      </div>

      <div className="grid gap-8 lg:grid-cols-3">
        <Sub title="Sizing" about="Compact controls, with a tap area of at least 44px.">
          <TokenTable rows={sizingRows} values={v} />
        </Sub>
        <Sub title="Layers" about="A fixed set of layers, so nothing fights to be on top.">
          <TokenTable rows={layerRows} values={v} />
        </Sub>
        <Sub title="Breakpoints" about="The four widths the layout changes at.">
          <TokenTable rows={breakpointRows} values={v} />
        </Sub>
      </div>
    </Section>
  );
}
