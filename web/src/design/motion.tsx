import { Plus, X } from "lucide-react";
import { motion } from "motion/react";
import { useState } from "react";
import { Button } from "@/components/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogTrigger } from "@/components/dialog";
import { AnimatePresence, Arrive, layoutTiming } from "@/components/motion";
import { MotionToggle, Segmented } from "@/components/theme-toggle";
import { useToast } from "@/components/toast";
import { arrive, duration as generated, ease } from "@/motion";
import { Presence, SessionChrome, SessionRoot, useSessionChrome, VideoSurface } from "@/session/chrome";
import { Demo, Section, Sub, TokenName, TokenTable, useTokens } from "./ui";

const durationRows = [
  { token: "--dur-instant", note: "Nothing" },
  { token: "--dur-fast", note: "Hover, press, focus. Also the longest motion inside a session" },
  { token: "--dur-base", note: "State changes; arrive" },
  { token: "--dur-slow", note: "Entrances; overlay" },
  { token: "--dur-graph", note: "A chart redrawing" },
  { token: "--dur-video", note: "The video surface. Always 0" },
  { token: "--session-chrome-hide", note: "Idle time before the session controls hide. Not a motion, so reduced motion leaves it alone" },
  { token: "--arrive-rise", note: "How far a new item rises" },
  { token: "--arrive-leave", note: "How far a leaving item lifts" },
  { token: "--overlay-from", note: "The scale an overlay opens from" },
];
const easingRows = [
  { token: "--ease-out", note: "Entrances: a fast start, then a gentle settle" },
  { token: "--ease", note: "Everything else" },
  { token: "--ease-spring", note: "Sparingly" },
];
const tokens = [...durationRows, ...easingRows].map((r) => r.token);

const generatedByToken: Record<string, number> = {
  "--dur-instant": generated.instant,
  "--dur-fast": generated.fast,
  "--dur-base": generated.base,
  "--dur-slow": generated.slow,
  "--dur-graph": generated.graph,
  "--dur-video": generated.video,
};

function ChromeReplay() {
  const { show, hide } = useSessionChrome();
  // Two controls rather than one toggle: each direction is replayable on
  // demand, and a label that does not move is a label a measurement can find.
  return (
    <div className="absolute bottom-3 left-3 flex items-center gap-2">
      <Button size="sm" onClick={hide}>Hide chrome</Button>
      <Button size="sm" onClick={show}>Show chrome</Button>
      <span className="text-xs text-text-3">or bring the pointer to the top edge</span>
    </div>
  );
}

const roster = [
  { id: "ws", name: "workstation", seen: 30 },
  { id: "bb", name: "build-box", seen: 18_000 },
  { id: "ml", name: "mum's laptop", seen: 90 },
  { id: "om", name: "office-mini", seen: 172_800 },
];

/**
 * The layout rule, demonstrated: sort the list or drop a row and the rows
 * that move are animated by Motion measuring them before and after, not by a
 * keyframe guessing where they will land.
 */
function LayoutReplay() {
  const [by, setBy] = useState<"name" | "seen">("name");
  const [gone, setGone] = useState<string[]>([]);
  const shown = roster
    .filter((d) => !gone.includes(d.id))
    .slice()
    .sort((a, b) => (by === "name" ? a.name.localeCompare(b.name) : a.seen - b.seen));
  return (
    <Demo className="flex-col items-stretch gap-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <span className="text-sm font-medium">layout</span>
        <div className="flex gap-2">
          <Segmented
            label="Sort devices by"
            value={by}
            onChange={setBy}
            options={[{ id: "name", label: "By name" }, { id: "seen", label: "By last seen" }]}
          />
          <Button size="sm" onClick={() => setGone([])} disabled={gone.length === 0}>
            Restore
          </Button>
        </div>
      </div>
      <ul className="flex flex-col gap-1">
        <AnimatePresence initial={false}>
          {shown.map((d) => (
            <motion.li
              key={d.id}
              layout
              initial={{ opacity: 0, y: arrive.rise }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -arrive.leave }}
              transition={{ layout: layoutTiming(), duration: layoutTiming().duration, ease: [...ease.out] }}
              className="flex h-8 items-center justify-between rounded-sm bg-surface-1 px-2 text-sm"
            >
              <span>{d.name}</span>
              <span className="flex items-center gap-3">
                <span className="font-mono text-xs tabular-nums text-text-3">{d.seen}s ago</span>
                <button
                  type="button"
                  aria-label={`Forget ${d.name}`}
                  data-tap-min=""
                  className="text-text-3 hover:text-text"
                  onClick={() => setGone((g) => [...g, d.id])}
                >
                  <X aria-hidden size={14} />
                </button>
              </span>
            </motion.li>
          ))}
        </AnimatePresence>
      </ul>
      <p className="max-w-[var(--measure)] text-xs text-text-3">
        Nothing here uses a keyframe. Rows are measured before and after the change and animated on the
        compositor. The lint rejects any <code className="font-mono">@keyframes</code> that animates a layout
        property.
      </p>
    </Demo>
  );
}

export function MotionSection() {
  const v = useTokens(tokens);
  const [items, setItems] = useState([1, 2, 3]);
  const [next, setNext] = useState(4);
  const toast = useToast();

  return (
    <Section
      id="motion"
      title="Motion"
      lede="Five motions and no others. Every one can be replayed below, with motion on or off."
    >
      <Demo className="justify-between">
        <div className="flex flex-col gap-1">
          <span className="text-sm font-medium text-text">Reduced motion</span>
          <span className="text-xs text-text-2">Turn it on in your OS settings or here. Either way, every duration token becomes 0, and any other animation is cut to 0.01ms.</span>
        </div>
        <MotionToggle />
      </Demo>

      <div className="grid gap-8 lg:grid-cols-[3fr_2fr]">
        <Sub title="Durations" about="The durations in use right now. Turn on reduced motion and they drop to zero.">
          <table className="w-full border-collapse text-sm">
            <thead className="text-left text-xs text-text-3">
              <tr className="h-[var(--row-h)]">
                <th className="pr-4 font-medium">Token</th>
                <th className="pr-4 font-medium">Document</th>
                <th className="pr-4 font-medium">motion.ts</th>
                <th className="font-medium">Use</th>
              </tr>
            </thead>
            <tbody className="[&_tr]:border-t [&_tr]:border-border">
              {durationRows.map((r) => {
                const gen = generatedByToken[r.token];
                return (
                  <tr key={r.token} className="h-[var(--row-h)]">
                    <td className="pr-4"><TokenName>{r.token}</TokenName></td>
                    <td className="pr-4 font-mono text-xs tabular-nums text-text">{v[r.token] ?? "—"}</td>
                    <td className="pr-4 font-mono text-xs tabular-nums text-text-2">{gen === undefined ? "" : `${gen}s`}</td>
                    <td className="text-xs text-text-3">{r.note}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </Sub>
        <Sub title="Easing" about="Things arriving use ease-out. Everything else uses ease.">
          <TokenTable rows={easingRows} values={v} />
          <div className="flex flex-col gap-2 pt-2">
            {easingRows.map((r) => (
              <div key={r.token} className="flex items-center gap-3">
                <div className="relative h-6 w-full rounded-sm bg-surface-1">
                  <EaseBall token={r.token} />
                </div>
              </div>
            ))}
            <span className="text-xs text-text-3">Hover a bar to run it once. With motion off, the ball is simply at the end.</span>
          </div>
        </Sub>
      </div>

      <Sub title="Replay" about="Run each motion on demand.">
        <div className="grid gap-4 lg:grid-cols-2">
          <Demo className="flex-col items-stretch gap-3">
            <div className="flex items-center justify-between">
              <span className="text-sm font-medium">arrive</span>
              <Button size="sm" onClick={() => { setItems((a) => [...a, next]); setNext((n) => n + 1); }}>
                <Plus aria-hidden size={14} /> Add a row
              </Button>
            </div>
            <ul className="flex flex-col gap-1">
              <AnimatePresence initial={false}>
                {items.map((i) => (
                  <Arrive key={i} className="flex h-[var(--row-h)] items-center justify-between rounded-sm bg-surface-1 px-2 text-sm">
                    <span>Row {i} rose 6px over --dur-base</span>
                    <button type="button" aria-label={`Remove row ${i}`} data-tap-min="" className="text-text-3 hover:text-text" onClick={() => setItems((a) => a.filter((x) => x !== i))}>
                      <X aria-hidden size={14} />
                    </button>
                  </Arrive>
                ))}
              </AnimatePresence>
            </ul>
          </Demo>

          <Demo className="flex-col items-stretch gap-3">
            <span className="text-sm font-medium">overlay · drawer · toast</span>
            <div className="flex flex-wrap gap-2">
              <Dialog>
                <DialogTrigger asChild><Button>Open a dialog</Button></DialogTrigger>
                <DialogContent title="Forget this device?" description="You'll need to pair it again to connect. Nothing changes on that machine.">
                  <DialogFooter>
                    <DialogClose asChild>
                      <Button>Cancel</Button>
                    </DialogClose>
                    <Button variant="danger">Forget</Button>
                  </DialogFooter>
                </DialogContent>
              </Dialog>
              <Dialog>
                <DialogTrigger asChild><Button>Open a drawer</Button></DialogTrigger>
                <DialogContent drawer title="Session details" description="Everything about this connection, in one place.">
                  <p className="text-sm text-text-2">A drawer enters from its edge over --dur-slow and leaves over --dur-fast.</p>
                </DialogContent>
              </Dialog>
              <Button onClick={() => toast.show({ title: "Copied the pairing code", tone: "ok" })}>Show a toast</Button>
            </div>
            <p className="text-xs text-text-3">The overlay opens from 0.97 scale over --dur-slow. Toasts arrive in a reserved band on --z-toast and never cover a control.</p>
          </Demo>
        </div>

        <Demo className="flex-col items-stretch gap-3">
          <span className="text-sm font-medium">chrome hide / show</span>
          <SessionRoot className="h-56 rounded-md border border-border">
            <VideoSurface>
              <span className="font-mono text-xs text-text-3">1920 × 1080 · the video surface never animates</span>
            </VideoSurface>
            <SessionChrome>
              <span className="text-sm font-medium">workstation</span>
              <span className="text-xs text-text-3">hides after --session-chrome-hide, over --dur-base (--dur-fast inside a session)</span>
            </SessionChrome>
            <Presence />
            <ChromeReplay />
          </SessionRoot>
        </Demo>

        <LayoutReplay />
      </Sub>

      <Sub title="The never-animate list" about="Where nothing moves, on purpose.">
        <ul className="grid gap-2 text-sm text-text-2 sm:grid-cols-2">
          <li className="rounded-sm border border-border p-3"><span className="font-medium text-text">Text reflow.</span> Words move when a layout does. They do not tween.</li>
          <li className="rounded-sm border border-border p-3"><span className="font-medium text-text">Table and list cell content.</span> A number counting up is wrong until it stops.</li>
          <li className="rounded-sm border border-border p-3"><span className="font-medium text-text">Anything per keystroke.</span> Typing has its own rhythm.</li>
          <li className="rounded-sm border border-border p-3"><span className="font-medium text-text">The reading critical path.</span> Skeletons do not shimmer; time-ago updates in place.</li>
          <li className="rounded-sm border border-border p-3"><span className="font-medium text-text">HUD numbers.</span> RTT, bitrate and fps are read while doing something else.</li>
          <li className="rounded-sm border border-border p-3"><span className="font-medium text-text">The video surface, and the presence indicator.</span> Frames swap. The indicator is there or it is not.</li>
        </ul>
      </Sub>
    </Section>
  );
}

/** A ball that runs the bar once on hover, on the easing named. */
function EaseBall({ token }: { token: string }) {
  const [run, setRun] = useState(false);
  return (
    <button
      type="button"
      aria-label={`Run ${token}`}
      onPointerEnter={() => setRun(true)}
      onFocus={() => setRun(true)}
      onPointerLeave={() => setRun(false)}
      onBlur={() => setRun(false)}
      className="absolute inset-0 flex items-center justify-end rounded-sm px-1"
    >
      <span
        aria-hidden
        className="absolute top-1 block size-4 rounded-full bg-text transition-[left] duration-[var(--dur-graph)]"
        style={{ transitionTimingFunction: `var(${token})`, left: run ? "calc(100% - 1.25rem)" : "0.25rem" }}
      />
      <span className="font-mono text-xs text-text-3">{token}</span>
    </button>
  );
}
