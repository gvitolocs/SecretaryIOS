// Dates the way Keep shows them: "Edited 14:30", "Edited 24 Oct", reminders as "Today, 18:00".

import { locale } from "../i18n";

const startOfDay = (d: Date): number => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
const clock = (d: Date): string => new Intl.DateTimeFormat(locale(), { hour: "2-digit", minute: "2-digit" }).format(d);

export function dayDiff(d: Date, now: Date): number {
  return Math.round((startOfDay(d) - startOfDay(now)) / 86_400_000);
}

/** "14:30" today, "24 Oct" this year, "24 Oct 2025" before. */
export function editedLabel(iso: string, now: Date = new Date()): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  if (dayDiff(d, now) === 0) return clock(d);
  const sameYear = d.getFullYear() === now.getFullYear();
  return new Intl.DateTimeFormat(locale(), sameYear ? { day: "numeric", month: "short" } : { day: "numeric", month: "short", year: "numeric" }).format(d);
}

/** "Today, 18:00" / "Tomorrow, 08:00" / "24 Oct, 09:00" / "24 Oct 2025, 09:00". */
export function reminderLabel(iso: string, now: Date = new Date()): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const diff = dayDiff(d, now);
  let day: string;
  if (diff === 0 || diff === 1) {
    const w = new Intl.RelativeTimeFormat(locale(), { numeric: "auto" }).format(diff, "day");
    day = w.charAt(0).toLocaleUpperCase(locale()) + w.slice(1);
  } else {
    const sameYear = d.getFullYear() === now.getFullYear();
    day = new Intl.DateTimeFormat(locale(), sameYear ? { day: "numeric", month: "short" } : { day: "numeric", month: "short", year: "numeric" }).format(d);
  }
  return `${day}, ${clock(d)}`;
}

export interface ReminderPreset {
  id: "laterToday" | "tomorrow" | "nextWeek";
  at: Date;
}

/** Keep's quick reminder choices: later today (18:00, or in three hours after 15:00), tomorrow 08:00, next Monday 08:00. */
export function reminderPresets(now: Date = new Date()): ReminderPreset[] {
  const at = (days: number, h: number, m = 0) => new Date(now.getFullYear(), now.getMonth(), now.getDate() + days, h, m, 0, 0);
  const out: ReminderPreset[] = [];
  const evening = at(0, 18);
  if (evening.getTime() - now.getTime() > 30 * 60_000) out.push({ id: "laterToday", at: evening });
  else if (now.getHours() < 21) {
    const later = new Date(now.getTime() + 3 * 3_600_000);
    later.setMinutes(0, 0, 0);
    if (later.getDate() === now.getDate()) out.push({ id: "laterToday", at: later });
  }
  out.push({ id: "tomorrow", at: at(1, 8) });
  const toMonday = ((8 - now.getDay()) % 7) || 7;
  out.push({ id: "nextWeek", at: at(toMonday, 8) });
  return out;
}

/** <input type=date> and <input type=time> values for a local Date, and back. */
export const toDateInput = (d: Date): string => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
export const toTimeInput = (d: Date): string => `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
export function fromInputs(date: string, time: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  const t = /^(\d{2}):(\d{2})$/.exec(time);
  if (!m || !t) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), Number(t[1]), Number(t[2]), 0, 0);
  return Number.isNaN(d.getTime()) ? null : d;
}
