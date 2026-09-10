import { useEffect, useState } from "react";
import { onStreamTrouble } from "@/api/client";
import { triedFor } from "@/api/types";
import { useDevices, useStatus } from "@/api/hooks";
import { Lamp, lampFor } from "@/components/lamp";
import { Popover } from "@/components/popover";
import { cn } from "@/lib/cn";

/**
 * The engine is not running, and what it said about that.
 *
 * # Why this is its own component
 *
 * Because it is the state nothing renders. The gallery draws the real strip
 * against a machine whose engine is running, so the words, contrast and
 * accessibility gates sweep three lamps and never this — which is the one
 * thing on the strip somebody reads when they are stuck.
 *
 * Behind a popover rather than in the strip itself: the strip is the least
 * attention-expensive place in the window, and a paragraph in it would be the
 * most expensive thing on the screen.
 */
export function EngineTrouble({ why }: { why: string }) {
  return (
    <Popover
      label="Why the engine is not running"
      side="top"
      align="start"
      trigger={
        <button
          type="button"
          data-engine-trouble=""
          className={cn(
            "inline-flex cursor-pointer items-center gap-1.5 rounded-sm px-1 text-warn",
            "transition-colors duration-[var(--dur-fast)] hover:text-text data-[state=open]:text-text",
          )}
        >
          engine not running
        </button>
      }
    >
      <div className="flex flex-col gap-2">
        <p className="text-sm text-text">The engine did not answer.</p>
        <p className="text-xs leading-relaxed text-text-2">
          Nothing can be captured, encoded or sent until it does. What it said is below.
        </p>
        <p className="break-words font-mono text-xs leading-relaxed text-text-3">{why}</p>
      </div>
    </Popover>
  );
}

/**
 * The live stream is down, and the screens are showing what they last asked
 * for.
 *
 * # Why this is on the strip at all
 *
 * Because a window with no stream looks exactly like a quiet machine. Every
 * screen here also asks outright, so a failed stream costs nothing but
 * *liveness* — and a person watching an Activity screen that had silently
 * stopped updating would read it as nothing having happened, which is the one
 * reading an audit screen must never invite.
 */
export function StreamTrouble({ why }: { why: string }) {
  return (
    <Popover
      label="Why this window is not updating live"
      side="top"
      align="start"
      trigger={
        <button
          type="button"
          data-stream-trouble=""
          className={cn(
            "inline-flex cursor-pointer items-center gap-1.5 rounded-sm px-1 text-warn",
            "transition-colors duration-[var(--dur-fast)] hover:text-text data-[state=open]:text-text",
          )}
        >
          not updating live
        </button>
      }
    >
      <div className="flex flex-col gap-2">
        <p className="text-sm text-text">This window is not being told when things happen.</p>
        <p className="text-xs leading-relaxed text-text-2">
          What is on screen is what it last asked for, not what is happening now. It keeps trying,
          and every screen still answers when you open it.
        </p>
        <p className="break-words font-mono text-xs leading-relaxed text-text-3">{why}</p>
      </div>
    </Popover>
  );
}

/**
 * Watch the one event stream, so the strip can say when it is down.
 */
function useStreamTrouble(): Error | null {
  const [trouble, setTrouble] = useState<Error | null>(null);
  useEffect(() => onStreamTrouble(setTrouble), []);
  return trouble;
}

/**
 * The strip along the bottom: what this machine can do, in four words.
 *
 * The least attention-expensive place for something always mildly relevant,
 * so it says the fewest true things — whether the engine answered, and what
 * it found. When it did not answer, that is the whole of the message: a strip
 * reporting zeroes would be reporting a machine that captures nothing, which
 * is a different fault from one that is not running.
 */
export function StatusStrip() {
  const { data, error, loading } = useStatus();
  const devices = useDevices();
  const streamTrouble = useStreamTrouble();

  if (loading) return <span className="text-text-3">…</span>;
  if (error || !data) {
    /*
      **And why**, behind the same popover the lamps use.

      "engine not running" is four words with no next step, and the reason is
      now knowable: the control plane keeps reading the engine's own output
      when it dies at startup, so a configuration file with a typo in it
      arrives here naming the file and the line. Throwing that away left
      somebody with a warning and nowhere to go.

      Behind a popover rather than in the strip, because the strip is the
      least attention-expensive place in the window and a paragraph in it
      would be the most expensive thing on the screen.
    */
    const why = error?.message?.trim();
    if (!why) return <span className="text-warn">engine not running</span>;
    return <EngineTrouble why={why} />;
  }

  const lamps = [
    {
      label: "capture",
      ok: data.capture.available,
      detail: data.capture.detail,
      tried: triedFor(data.capture),
      what: "How this machine reads its own screen. Without it there is nothing to send.",
    },
    {
      label: "encode",
      ok: data.encoder.available,
      detail: data.encoder.detail,
      tried: triedFor(data.encoder),
      what: "What turns those pixels into something a link will carry.",
    },
    {
      label: "input",
      ok: data.input.available,
      detail: data.input.detail,
      tried: triedFor(data.input),
      what: "Whether a paired device can type and point here, or only watch.",
    },
  ];
  return (
    <>
      {/*
        **A lamp with a sentence behind it.** These three decide whether a
        session is possible at all, and a coloured dot with one word is not
        enough to act on — the engine's own detail line says *which* backend
        and why, which is the difference between "it does not work" and "it
        needs a portal". A `title` held that and could not hold a sentence, or
        be reached by a keyboard.
      */}
      {lamps.map((l) => (
        <Popover
          key={l.label}
          label={`What ${l.label} means`}
          side="top"
          align="start"
          trigger={
            <button
              type="button"
              className={cn(
                "inline-flex cursor-pointer items-center gap-1.5 rounded-sm px-1",
                "transition-colors duration-[var(--dur-fast)] hover:text-text data-[state=open]:text-text",
              )}
            >
              <Lamp state={lampFor(l.ok)}>{l.label}</Lamp>
            </button>
          }
        >
          <div className="flex flex-col gap-2">
            <p className="text-sm text-text">
              <Lamp state={lampFor(l.ok)} className="gap-2">
                {l.label}
                <span className="ml-2 text-text-3">{l.ok ? "available" : "not available"}</span>
              </Lamp>
            </p>
            <p className="text-xs leading-relaxed text-text-2">{l.what}</p>
            {l.detail && (
              <p className="break-words font-mono text-xs leading-relaxed text-text-3">{l.detail}</p>
            )}
            {l.tried.length > 0 && (
              <ul data-tried={l.label} className="space-y-1 border-t border-line pt-2">
                {l.tried.map((attempt) => (
                  <li key={attempt} className="break-words font-mono text-xs leading-relaxed text-text-3">
                    {attempt}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </Popover>
      ))}
      {/*
        Whose version this is, and the one day it matters.

        Two programs: this window, built at a version, and the engine it is
        talking to, running at one. On an ordinary machine they agree and the
        number is reassurance. They disagree exactly when somebody has updated
        one and not the other — which is the day the readout is worth anything,
        and the day two bare numbers, one here and one in the sidebar, tell
        nobody which is which.
      */}
      {data.engine === __ESPER_VERSION__ ? (
        <span className="font-mono">
          <span className="text-text-3">engine</span> {data.engine}
        </span>
      ) : (
        <Lamp state="warn" title={`This window is ${__ESPER_VERSION__} and the engine it is talking to is ${data.engine}.`}>
          <span className="font-mono">
            <span className="text-text-3">engine</span> {data.engine}
            <span className="text-text-3"> · window</span> {__ESPER_VERSION__}
          </span>
        </Lamp>
      )}
      {devices.data && <span className="font-mono tabular-nums">{devices.data.length} paired</span>}
      {/*
        **Announced, not only shown.** This appears while somebody is looking
        at something else — that is the whole point of it — and a person using
        a screen reader would otherwise learn that the window had stopped
        being told things only by going and asking. The region is always here
        and its contents change, which is what makes the change an
        announcement rather than a repaint.
      */}
      <span role="status" aria-live="polite">
        {streamTrouble && <StreamTrouble why={streamTrouble.message} />}
      </span>
    </>
  );
}
