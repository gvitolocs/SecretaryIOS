// Units are stored as integers everywhere (grams, seconds, meters, ms).
// These helpers only format for display. Parsing typed weights and every
// computed label belong to the Rust engines (docs/server-api.md).

export type WeightUnit = "kg" | "lb";

const LB_GRAMS = 453.59237;

/** The raw numeric value in the display unit (used for charts). */
export function weightValue(grams: number, unit: WeightUnit): number {
  return unit === "lb" ? grams / LB_GRAMS : grams / 1000;
}

function oneDecimal(value: number): string {
  if (!Number.isFinite(value)) return "0";
  const fixed = value.toFixed(1);
  return fixed.endsWith(".0") ? fixed.slice(0, -2) : fixed;
}

export function formatWeight(grams: number, unit: WeightUnit): string {
  return oneDecimal(weightValue(grams, unit));
}

export function formatDuration(seconds: number): string {
  const secs = Math.max(0, Math.floor(seconds));
  if (secs >= 3600) {
    const hours = Math.floor(secs / 3600);
    const minutes = Math.floor(secs / 60) % 60;
    return `${hours}h ${String(minutes).padStart(2, "0")}m`;
  }
  const minutes = Math.floor(secs / 60);
  return `${minutes}m ${String(secs % 60).padStart(2, "0")}s`;
}

export function formatDistance(meters: number): string {
  const m = Math.max(0, Math.round(meters));
  if (m >= 1000) return `${oneDecimal(m / 1000)} km`;
  return `${m} m`;
}

export function formatDate(ms: number): string {
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(new Date(ms));
}

export function formatDateTime(ms: number): string {
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(ms));
}

/** Countdown text for the rest banner: 90 -> "1:30". Visual formatting only. */
export function formatCountdown(seconds: number): string {
  const secs = Math.max(0, Math.floor(seconds));
  return `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
}
