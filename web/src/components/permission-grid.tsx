import { Lock } from "lucide-react";
import { Switch } from "./switch";
import { cn } from "@/lib/cn";
import { PERMISSIONS, PERMISSION_LABEL, SENSITIVE, type Permission } from "@/api/permissions";
import { readSchedule } from "@/api/schedule";

export type Held = ReadonlySet<Permission>;

export interface PermissionGridProps {
  value: Held;
  onChange: (next: Held) => void;
  disabled?: boolean | undefined;
  /**
   * When `unattended` applies, said beside the switch it bounds.
   *
   * `unattended` is the one permission here that can be bounded to hours, and
   * a row that read the same for "may walk in on weekday afternoons" and "may
   * walk in at three in the morning" **overstates** what most devices holding
   * it may do. Absent means no bound, which is what a bare grant means.
   */
  when?: string | undefined;
  className?: string | undefined;
}

/**
 * What a device may do, one switch per permission.
 *
 * The words are the store's fourteen, not a friendlier five: a grid that
 * offered choices the file cannot hold would be a grid that fails on save.
 *
 * Two kinds of switch cannot move. **Seeing the screen** is always on — a
 * session with nothing granted is not a session. And the **sensitive** five
 * need somebody at the machine: the local API refuses them outright
 * (`ErrHostOnly`), so a switch that offered them would be one that always
 * fails, and it says where they are granted instead.
 */
export function PermissionGrid({ value, onChange, disabled = false, when, className }: PermissionGridProps) {
  const hours = readSchedule(when);
  return (
    <div className={cn("flex flex-col", className)}>
      <ul className="flex flex-col divide-y divide-border">
        {PERMISSIONS.map((p) => {
          const always = p === "view";
          const hostOnly = SENSITIVE.has(p);
          const id = `perm-${p}`;
          return (
            <li key={p} className="flex items-center justify-between gap-4 py-2.5">
              <span className="flex min-w-0 flex-col gap-0.5">
                <label htmlFor={id} className="text-sm leading-snug text-text">
                  {PERMISSION_LABEL[p]}
                </label>
                {/* The bound under the name it bounds, so the row never squeezes either. */}
                {p === "unattended" && value.has(p) && hours && (
                  <span data-bounded="" className="text-xs text-text-3">
                    {hours}
                  </span>
                )}
              </span>
              <span className="flex shrink-0 items-center gap-3">
                {/* Marked with the same lock the note under the list explains. */}
                {hostOnly && (
                  <span className="text-text-3" title={HOST_ONLY} role="img" aria-label={HOST_ONLY}>
                    <Lock aria-hidden size={13} strokeWidth={1.75} />
                  </span>
                )}
                {always && <span className="text-xs text-text-3">Always on</span>}
                <Switch
                  id={id}
                  checked={always ? true : value.has(p)}
                  disabled={disabled || always || hostOnly}
                  onCheckedChange={(on) => {
                    const next = new Set(value);
                    if (on) next.add(p);
                    else next.delete(p);
                    onChange(next);
                  }}
                  aria-label={PERMISSION_LABEL[p]}
                />
              </span>
            </li>
          );
        })}
      </ul>
      <p className="flex items-start gap-2 border-t border-border py-3 text-xs leading-relaxed text-text-3">
        <Lock aria-hidden size={13} strokeWidth={1.75} className="mt-0.5 shrink-0" />
        {HOST_ONLY}
      </p>
    </div>
  );
}

/** Why five switches cannot move here: the local API refuses them by design. */
const HOST_ONLY = "Only someone at this machine can turn these on, in a terminal, with esper grant.";

/** A device that may only watch. What a fresh pairing holds. */
export const JUST_VIEWING: Held = new Set<Permission>(["view"]);

export const heldFrom = (list: readonly Permission[]): Held => new Set(list);
