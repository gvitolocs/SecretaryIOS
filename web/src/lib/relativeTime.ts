import { parseTs } from "../api/timeline";
import { locale } from "../i18n";

/** "3 minutes ago", "yesterday", "in 22 hours": `ts` relative to now, in the app's locale. */
export function relativeTime(ts: number | string): string {
  const seconds = Math.round(((parseTs(ts) ?? Date.now()) - Date.now()) / 1000);
  const abs = Math.abs(seconds);
  const [unit, size]: [Intl.RelativeTimeFormatUnit, number] =
    abs >= 86_400 ? ["day", 86_400] : abs >= 3_600 ? ["hour", 3_600] : abs >= 60 ? ["minute", 60] : ["second", 1];
  return new Intl.RelativeTimeFormat(locale(), { numeric: "auto" }).format(Math.round(seconds / size), unit);
}
