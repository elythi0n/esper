/**
 * When a device may connect unattended, as a person reads it.
 *
 * # Why a screen has to say this
 *
 * `unattended` is the permission that removes the question: a device holding
 * it walks in without anybody at the host being asked. It can be **bounded** —
 * `esper schedule <device> weekdays 08:00-18:00` — and a device bounded to
 * office hours is a different device from one that may walk in at three in the
 * morning.
 *
 * The engine has stored that bound since Phase 14, the API answers with it and
 * the command line prints it. The screens showed a lamp labelled "Connect
 * unasked" and nothing else, which overstates what most such devices may
 * actually do — and overstating a permission is the one direction a screen
 * about trust must never be wrong in.
 */

/** The days a schedule names, in the order a week is read. */
const DAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] as const;

const READABLE: Readonly<Record<string, string>> = {
  mon: "Mon",
  tue: "Tue",
  wed: "Wed",
  thu: "Thu",
  fri: "Fri",
  sat: "Sat",
  sun: "Sun",
};

/**
 * A schedule as the engine writes it, turned into a phrase.
 *
 * The engine's own vocabulary is `weekdays 08:00-18:00`, `daily 22:00-06:00`,
 * `mon,wed,fri 9-17` and `always`. This does not re-derive any of it: what
 * comes back is what the engine stored, and the only work here is making it
 * read as English and saying plainly when a window crosses midnight — because
 * `22:00-06:00` is eight hours somebody could read as sixteen.
 *
 * An empty or absent schedule is `null`: no bound, which is what `always`
 * means and what the absence of the field means too. The caller says nothing
 * rather than saying "always", because a lamp that is on already says that.
 */
export function readSchedule(when: string | undefined): string | null {
  const said = (when ?? "").trim();
  if (said === "" || said.toLowerCase() === "always") return null;

  const [dayPart, hourPart] = said.split(/\s+/, 2);
  if (!dayPart) return null;
  const days = readDays(dayPart);
  if (!hourPart) return days;

  const [from, to] = hourPart.split("-", 2);
  if (!from || !to) return days;
  const start = readTime(from);
  const end = readTime(to);
  if (start === null || end === null) return `${days} ${hourPart}`;
  // A window whose end is at or before its start runs through midnight. Said
  // rather than left to arithmetic: 22:00–06:00 is eight hours, and somebody
  // reading it as a normal range reads sixteen.
  const overnight = end <= start;
  return `${days} ${from}–${to}${overnight ? " (overnight)" : ""}`;
}

/** `weekdays`, `weekends`, `daily` or a list, as a person says them. */
function readDays(part: string): string {
  const word = part.toLowerCase();
  if (word === "weekdays") return "Weekdays";
  if (word === "weekends") return "Weekends";
  if (word === "daily" || word === "everyday") return "Every day";
  const named = word
    .split(",")
    .map((d) => d.trim())
    .filter((d) => (DAYS as readonly string[]).includes(d))
    .map((d) => READABLE[d] ?? d);
  if (named.length === 0) return part;
  if (named.length === 7) return "Every day";
  return named.join(", ");
}

/** Minutes past midnight, or null when it is not a time at all. */
function readTime(text: string): number | null {
  const bits = text.trim().split(":");
  const hour = Number(bits[0]);
  const minute = bits.length > 1 ? Number(bits[1]) : 0;
  if (!Number.isInteger(hour) || hour < 0 || hour > 24) return null;
  if (!Number.isInteger(minute) || minute < 0 || minute > 59) return null;
  return hour * 60 + minute;
}

/** The `Schedule` the form control takes: days as 0–6 from Monday, and hours. */
export interface Window {
  days: readonly (0 | 1 | 2 | 3 | 4 | 5 | 6)[];
  from: string;
  to: string;
}

const EVERY: Window["days"] = [0, 1, 2, 3, 4, 5, 6];
const WEEKDAYS: Window["days"] = [0, 1, 2, 3, 4];
const WEEKENDS: Window["days"] = [5, 6];

/**
 * A stored schedule as the control's own shape.
 *
 * The engine writes words — `weekdays`, `weekends`, `daily`, a day list — and
 * the control takes day numbers and two times. Absent or `always` becomes
 * every day, all day, which is what a bare grant means and therefore the right
 * thing for a control that is about to be *narrowed*.
 */
export function asWindow(when: string | undefined): Window {
  const said = (when ?? "").trim().toLowerCase();
  if (said === "" || said === "always") return { days: EVERY, from: "00:00", to: "23:59" };

  const [dayWord, hourPart] = said.split(/\s+/, 2);
  const dayPart = dayWord ?? "";
  let days: Window["days"] = EVERY;
  if (dayPart === "weekdays") days = WEEKDAYS;
  else if (dayPart === "weekends") days = WEEKENDS;
  else if (dayPart === "daily" || dayPart === "everyday") days = EVERY;
  else {
    const named = dayPart
      .split(",")
      .map((d) => DAYS.indexOf(d.trim() as (typeof DAYS)[number]))
      .filter((i): i is 0 | 1 | 2 | 3 | 4 | 5 | 6 => i >= 0);
    if (named.length > 0) days = named;
  }

  const [from, to] = (hourPart ?? "").split("-", 2);
  return { days, from: padTime(from) ?? "00:00", to: padTime(to) ?? "23:59" };
}

/**
 * The control's shape as the engine's own words.
 *
 * Written back in the engine's vocabulary rather than a second one: what is
 * sent is a string the command line would have taken, so there is one grammar
 * and the parser that decides is the engine's.
 *
 * Every day, all day is `always` — the word the engine uses for no bound —
 * because a schedule that names every hour of every week is not a schedule,
 * and storing one would make an unbounded grant look bounded.
 */
export function asWhen(window: Window): string {
  const days = [...window.days].sort((a, b) => a - b);
  const allDay = window.from === "00:00" && (window.to === "23:59" || window.to === "24:00");
  if (days.length === 7 && allDay) return "always";
  const named =
    days.length === 7
      ? "daily"
      : same(days, WEEKDAYS)
        ? "weekdays"
        : same(days, WEEKENDS)
          ? "weekends"
          : days.map((d) => DAYS[d]).join(",");
  return `${named} ${window.from}-${window.to}`;
}

function same(a: readonly number[], b: readonly number[]): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** "9" and "9:5" are hours a person typed; the engine writes "09:00". */
function padTime(text: string | undefined): string | null {
  if (!text) return null;
  const bits = text.split(":");
  const hour = Number(bits[0]);
  const minute = bits.length > 1 ? Number(bits[1]) : 0;
  if (!Number.isInteger(hour) || !Number.isInteger(minute)) return null;
  return `${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}`;
}
