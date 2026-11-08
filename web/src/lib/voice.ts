import { locale } from "../i18n";
import type { VoiceSpans } from "../api/voice";

// Display helpers for the Voice device page. Pure: the page passes numbers in, text comes out.

const num = (n: number, o?: Intl.NumberFormatOptions) => new Intl.NumberFormat(locale(), o).format(n);

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB"];

/** "812 B", "1.5 KB", "12 MB": 1024-based like the flash and heap sizes, one decimal below 10. */
export function formatBytes(bytes: number): string {
  let n = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
  let i = 0;
  const digits = () => (i === 0 ? 0 : n < 10 ? 1 : 0);
  const rounded = () => Number(n.toFixed(digits()));
  while (i < BYTE_UNITS.length - 1 && (n >= 1024 || (i > 0 && rounded() >= 1024))) {
    n /= 1024;
    i++;
  }
  return `${num(n, { maximumFractionDigits: digits() })} ${BYTE_UNITS[i]}`;
}

/** "420 ms", "1,300 ms", and from 10 s on "12.3 s". */
export function formatMs(ms: number): string {
  const v = Number.isFinite(ms) ? Math.max(0, ms) : 0;
  if (v >= 10_000) return `${num(v / 1000, { maximumFractionDigits: 1 })} s`;
  return `${num(Math.round(v))} ms`;
}

/** "42 s", "12m 05s", "3h 05m", "2d 4h". */
export function formatUptime(ms: number): string {
  const s = Math.max(0, Math.floor((Number.isFinite(ms) ? ms : 0) / 1000));
  const d = Math.floor(s / 86_400);
  const h = Math.floor(s / 3600) % 24;
  const m = Math.floor(s / 60) % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${pad(m)}m`;
  if (m > 0) return `${m}m ${pad(s % 60)}s`;
  return `${s}s`;
}

export type SignalQuality = "excellent" | "good" | "fair" | "weak";

/** A simple label for a Wi-Fi RSSI in dBm. */
export function rssiQuality(rssi: number): SignalQuality {
  if (rssi >= -60) return "excellent";
  if (rssi >= -70) return "good";
  if (rssi >= -80) return "fair";
  return "weak";
}

export type SegmentKey = "stt" | "lucina" | "tts" | "audio" | "rest";

export interface TurnSegment {
  key: SegmentKey;
  /** How long this stage took. */
  ms: number;
  /** Where it starts, from the end of speech. */
  from: number;
}

export interface TurnBar {
  segments: TurnSegment[];
  /** The bar's length: the total, or the last stage if there is no total. */
  end: number;
}

const STAGES: [keyof VoiceSpans, SegmentKey][] = [
  ["stt", "stt"],
  ["lucina_first", "lucina"],
  ["tts_first", "tts"],
  ["first_audio", "audio"],
];

/**
 * The server's spans are all measured from the end of speech (protocol section 2, `done.ms`), so each is the
 * moment a stage finished: STT done, Lucina's first text, the first TTS audio, the first audio sent. A stage's
 * own time is the step from the previous moment; what is left up to `total` is the rest of the reply. A moment
 * that is missing (a failed turn) is skipped, and one that went backwards is clamped, so the bar never overlaps.
 */
export function turnBar(spans: VoiceSpans | undefined): TurnBar {
  const segments: TurnSegment[] = [];
  let at = 0;
  for (const [field, key] of STAGES) {
    const v = spans?.[field];
    if (typeof v !== "number" || !Number.isFinite(v) || v < 0) continue;
    const to = Math.max(v, at);
    segments.push({ key, from: at, ms: to - at });
    at = to;
  }
  const total = spans?.total;
  if (typeof total === "number" && Number.isFinite(total) && total > at) {
    segments.push({ key: "rest", from: at, ms: total - at });
    at = total;
  }
  return { segments, end: at };
}

/** The moment of a span, if the server sent a usable one. */
export function spanMs(v: number | null | undefined): number | null {
  return typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : null;
}

/** The time of an event or turn, "14:03:07", with the day when it is not today. */
export function formatClock(iso: string | number, now = Date.now()): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "";
  const time = new Intl.DateTimeFormat(locale(), { hour: "2-digit", minute: "2-digit", second: "2-digit" }).format(at);
  const same = at.toDateString() === new Date(now).toDateString();
  if (same) return time;
  return `${new Intl.DateTimeFormat(locale(), { day: "numeric", month: "short" }).format(at)}, ${time}`;
}
