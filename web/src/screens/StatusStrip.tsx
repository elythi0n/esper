import { useDevices, useStatus } from "@/api/hooks";
import { Lamp, lampFor } from "@/components/lamp";
import { Popover } from "@/components/popover";
import { cn } from "@/lib/cn";

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

  if (loading) return <span className="text-text-3">…</span>;
  if (error || !data) return <span className="text-warn">engine not running</span>;

  const lamps = [
    {
      label: "capture",
      ok: data.capture.available,
      detail: data.capture.detail,
      what: "How this machine reads its own screen. Without it there is nothing to send.",
    },
    {
      label: "encode",
      ok: data.encoder.available,
      detail: data.encoder.detail,
      what: "What turns those pixels into something a link will carry.",
    },
    {
      label: "input",
      ok: data.input.available,
      detail: data.input.detail,
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
                "inline-flex cursor-pointer items-center gap-1.5 rounded-sm px-1 outline-none",
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
    </>
  );
}
