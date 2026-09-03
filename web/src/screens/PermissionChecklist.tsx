import { Callout } from "@/components/callout";
import { checklist, type Platform } from "./permissions";

/**
 * What this machine still needs somebody to do, on the platform it is on.
 *
 * Shown where a person meets the problem — the first run, and Settings beside
 * the capabilities themselves — and **not shown at all when there is nothing
 * to do**: a checklist of things that are already true is a checklist nobody
 * reads, and it would sit on screen for ever on a machine where everything
 * works.
 *
 * The words are in `permissions.ts` rather than here, because the part that
 * has to be right is the part nobody here can test: a person who owns a Mac
 * can read that table and correct it without touching a screen.
 */
export function PermissionChecklist({
  platform,
  have,
  className,
}: {
  platform: Platform;
  have: { capture: boolean; input: boolean };
  className?: string | undefined;
}) {
  const steps = checklist(platform, have);
  if (steps.length === 0) return null;
  return (
    <Callout
      tone="warn"
      title={steps.length === 1 ? "One thing to do on this machine" : `${steps.length} things to do on this machine`}
      data-checklist=""
      {...(className === undefined ? {} : { className })}
    >
      <ul className="flex flex-col gap-2">
        {steps.map((step) => (
          <li key={`${step.what}:${step.todo}`} className="flex flex-col gap-0.5">
            <span>{step.todo}</span>
            {step.where && <span className="text-text-3">{step.where}</span>}
          </li>
        ))}
      </ul>
    </Callout>
  );
}
