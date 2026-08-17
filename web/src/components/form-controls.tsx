import { useState, type InputHTMLAttributes } from "react";
import { Input } from "./input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./select";
import { cn } from "@/lib/cn";

/*
  The form controls a remote-desktop product needs beyond text: a number with
  its unit, a duration, a schedule. Each is a plain input underneath, so
  everything the Input carries — the strong border, the disabled treatment,
  the tap minimum — is carried here too.
*/

export interface NumberInputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "type" | "value" | "onChange"> {
  value: number | "";
  onChange: (value: number | "") => void;
  /** Shown inside the control, after the number. */
  unit?: string | undefined;
}

export function NumberInput({ value, onChange, unit, className, ...props }: NumberInputProps) {
  return (
    <div className={cn("relative", className)}>
      <Input
        type="number"
        inputMode="decimal"
        value={value}
        onChange={(e) => onChange(e.target.value === "" ? "" : Number(e.target.value))}
        className={cn("tabular-nums", unit && "pr-12")}
        {...props}
      />
      {unit && (
        <span aria-hidden className="pointer-events-none absolute inset-y-0 right-2 inline-flex items-center text-xs text-text-3">
          {unit}
        </span>
      )}
    </div>
  );
}

type DurationUnit = "s" | "min" | "h" | "d";
const unitSeconds: Record<DurationUnit, number> = { s: 1, min: 60, h: 3600, d: 86400 };
const unitWord: Record<DurationUnit, string> = { s: "seconds", min: "minutes", h: "hours", d: "days" };

/** The largest unit that divides the value evenly, so 3600 shows as 1 hour and 90 as 90 seconds. */
function bestUnit(seconds: number): DurationUnit {
  for (const u of ["d", "h", "min"] as const) if (seconds > 0 && seconds % unitSeconds[u] === 0) return u;
  return "s";
}

export interface DurationInputProps {
  /** Always seconds; the unit is a way of writing it. */
  value: number;
  onChange: (seconds: number) => void;
  id?: string | undefined;
  disabled?: boolean | undefined;
  className?: string | undefined;
  "aria-describedby"?: string | undefined;
}

export function DurationInput({ value, onChange, id, disabled, className, ...aria }: DurationInputProps) {
  const [unit, setUnit] = useState<DurationUnit>(() => bestUnit(value));
  const shown = value / unitSeconds[unit];
  return (
    <div className={cn("flex gap-2", className)}>
      <NumberInput
        id={id}
        min={0}
        disabled={disabled}
        value={Number.isInteger(shown) ? shown : Number(shown.toFixed(2))}
        onChange={(v) => onChange(v === "" ? 0 : Math.max(0, v) * unitSeconds[unit])}
        className="w-28"
        {...aria}
      />
      <Select value={unit} onValueChange={(u) => setUnit(u as DurationUnit)} disabled={disabled ?? false}>
        <SelectTrigger aria-label="Unit" className="w-32">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {(Object.keys(unitWord) as DurationUnit[]).map((u) => (
            <SelectItem key={u} value={u}>
              {unitWord[u]}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}

export type Weekday = 0 | 1 | 2 | 3 | 4 | 5 | 6;
export interface Schedule {
  days: readonly Weekday[];
  /** "HH:MM", local time on the host. */
  from: string;
  to: string;
}

const dayLetters = ["M", "T", "W", "T", "F", "S", "S"] as const;
const dayNames = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"] as const;

export interface ScheduleInputProps {
  value: Schedule;
  onChange: (next: Schedule) => void;
  disabled?: boolean | undefined;
  className?: string | undefined;
}

/**
 * When unattended access is allowed: which days, between which times. Days
 * are toggle buttons with the full name as the label, and the letter is what
 * fits; the times are the platform's own time inputs, because a hand-drawn
 * clock is a thing nobody types into well.
 */
export function ScheduleInput({ value, onChange, disabled, className }: ScheduleInputProps) {
  const toggle = (d: Weekday) =>
    onChange({ ...value, days: value.days.includes(d) ? value.days.filter((x) => x !== d) : [...value.days, d].sort() });
  return (
    <div className={cn("flex flex-col gap-3", className)}>
      <div role="group" aria-label="Days" className="flex gap-1">
        {dayLetters.map((letter, i) => {
          const d = i as Weekday;
          const on = value.days.includes(d);
          return (
            <button
              key={dayNames[d]}
              type="button"
              aria-pressed={on}
              aria-label={dayNames[d]}
              disabled={disabled}
              data-tap-min=""
              onClick={() => toggle(d)}
              className={cn(
                "inline-flex size-[var(--control-h)] cursor-pointer items-center justify-center rounded-sm border text-sm font-medium",
                "transition-[background-color,border-color,color] duration-[var(--dur-fast)] ease-[var(--ease)]",
                on ? "border-accent bg-accent text-accent-fg" : "border-border-strong bg-surface-0 text-text-2 hover:text-text",
                "disabled:border-border disabled:bg-surface-1 disabled:text-text-disabled",
              )}
            >
              {letter}
            </button>
          );
        })}
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm text-text-2">
        <span>from</span>
        <div className="w-28">
          <Input type="time" aria-label="From" value={value.from} disabled={disabled} onChange={(e) => onChange({ ...value, from: e.target.value })} className="tabular-nums" />
        </div>
        <span>to</span>
        <div className="w-28">
          <Input type="time" aria-label="To" value={value.to} disabled={disabled} onChange={(e) => onChange({ ...value, to: e.target.value })} className="tabular-nums" />
        </div>
      </div>
    </div>
  );
}
