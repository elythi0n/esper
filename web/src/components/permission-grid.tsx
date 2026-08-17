import { Switch } from "./switch";
import { cn } from "@/lib/cn";
import { PERMISSIONS, PERMISSION_LABEL, SENSITIVE, type Permission } from "@/api/permissions";

export type Held = ReadonlySet<Permission>;

export interface PermissionGridProps {
  value: Held;
  onChange: (next: Held) => void;
  disabled?: boolean | undefined;
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
export function PermissionGrid({ value, onChange, disabled = false, className }: PermissionGridProps) {
  return (
    <ul className={cn("flex flex-col divide-y divide-border border-y border-border", className)}>
      {PERMISSIONS.map((p) => {
        const always = p === "view";
        const hostOnly = SENSITIVE.has(p);
        const id = `perm-${p}`;
        return (
          <li key={p} className="flex items-center justify-between gap-4 py-2.5">
            <label htmlFor={id} className="min-w-0 truncate text-sm text-text">
              {PERMISSION_LABEL[p]}
            </label>
            <span className="flex shrink-0 items-center gap-3">
              {hostOnly && <span className="font-mono text-xs uppercase tracking-[0.14em] text-text-3">at the host</span>}
              {always && <span className="font-mono text-xs uppercase tracking-[0.14em] text-text-3">always</span>}
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
  );
}

/** A device that may only watch. What a fresh pairing holds. */
export const JUST_VIEWING: Held = new Set<Permission>(["view"]);

export const heldFrom = (list: readonly Permission[]): Held => new Set(list);
