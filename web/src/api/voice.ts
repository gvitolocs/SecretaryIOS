import { request } from "./client";

// The Voice device page (/voice): the LucinaVoice ESP32 speaker's telemetry, from the device API in
// docs/lucinavoice-protocol.md section 5 (secretary-server stores what the device sends plus its own
// per-turn spans):
//   GET /v1/voice/devices                      -> [{id, name, online, last_seen, fw, rssi?, est_mw?}]
//   GET /v1/voice/devices/{id}/status          -> the specs, the last stats, today's traffic, power, the last 20 turns
//   GET /v1/voice/devices/{id}/events?kind=&limit=<=500[&before=<next>]
//                                              -> newest first, {events:[{at, k, ...}], next?}
// Every field the device may leave out is optional here: an old firmware or a failed turn omits them.

export const VOICE_KINDS = ["boot", "wifi", "link", "turn", "stats", "error"] as const;
export type VoiceKind = (typeof VOICE_KINDS)[number];

export interface VoiceDeviceSummary {
  id: string;
  name: string;
  online: boolean;
  last_seen?: string | null;
  fw?: string | null;
  rssi?: number | null;
  est_mw?: number | null;
}

export interface VoiceSpecs {
  chip?: string;
  rev?: string;
  cores?: number;
  cpu_mhz?: number;
  flash_bytes?: number;
  psram_bytes?: number;
  fw?: string;
  commit?: string;
  /** The last two bytes of the MAC address, "da:b8". */
  mac?: string;
  heap_total?: number;
  ina?: "ina219" | "ina226" | null;
  temp_sensor?: boolean;
}

/** The device's last `stats` event (totals since boot). */
export interface VoiceLive {
  at?: string;
  uptime_ms?: number;
  heap_free?: number;
  heap_min_free?: number;
  rssi?: number | null;
  up_bytes?: number;
  down_bytes?: number;
  tx_air_ms?: number;
  cpu_mhz?: number;
  temp_c?: number | null;
}

export interface VoiceNetwork {
  today_up_bytes: number;
  today_down_bytes: number;
  total_up_bytes: number;
  total_down_bytes: number;
}

export interface VoicePower {
  est_mw?: number | null;
  /** True when an INA219/INA226 measured it; false: the duty-cycle estimate. */
  measured?: boolean;
  energy_mwh_today?: number | null;
}

/** Milliseconds from the end of speech (one clock); a missing one did not happen (a failed turn). */
export interface VoiceSpans {
  stt?: number | null;
  lucina_first?: number | null;
  tts_first?: number | null;
  first_audio?: number | null;
  total?: number | null;
  /** Lucina's `route` event (the voice fast path); absent without it. */
  route?: number | null;
  /** The filler phrase went out; absent when none was played. */
  ack?: number | null;
}

export interface VoiceTurn {
  at: string;
  ok: boolean;
  err?: string | null;
  transcript_chars?: number;
  speech_ms?: number;
  spans?: VoiceSpans;
  device_first_audio_ms?: number | null;
  up_bytes?: number;
  down_bytes?: number;
  /** How Lucina routed the turn (protocol section 6); absent for a Lucina without the fast path. */
  intent?: string;
  confidence?: number | null;
  jev_ms?: number | null;
  /** Answered without the planner LLM. */
  fast?: boolean;
  source?: string;
}

export interface VoiceStatus {
  id: string;
  name: string;
  online: boolean;
  connected_since?: string | null;
  last_seen?: string | null;
  specs: VoiceSpecs;
  live: VoiceLive;
  network: VoiceNetwork;
  power: VoicePower;
  turns: VoiceTurn[];
}

/** One event: `at` (when, server clock), `k` (the kind) and the kind's own fields. */
export interface VoiceEvent {
  at: string;
  k: string;
  [field: string]: unknown;
}

export interface VoiceEventsPage {
  events: VoiceEvent[];
  /** A cursor for the next (older) page; absent on the last page. */
  next?: string | null;
}

const dev = (id: string) => `/v1/voice/devices/${encodeURIComponent(id)}`;

export const voiceApi = {
  async devices(): Promise<VoiceDeviceSummary[]> {
    const list = await request<VoiceDeviceSummary[] | null>("GET", "/v1/voice/devices");
    return Array.isArray(list) ? list : [];
  },

  async status(id: string): Promise<VoiceStatus> {
    const s = await request<VoiceStatus>("GET", `${dev(id)}/status`);
    return { ...s, specs: s.specs ?? {}, live: s.live ?? {}, power: s.power ?? {}, turns: s.turns ?? [], network: s.network ?? { today_up_bytes: 0, today_down_bytes: 0, total_up_bytes: 0, total_down_bytes: 0 } };
  },

  /** `kind` filters on the server; `before` is the previous page's `next`. */
  async events(id: string, opts: { kind?: VoiceKind | null; limit?: number; before?: string | null } = {}): Promise<VoiceEventsPage> {
    const q = new URLSearchParams();
    if (opts.kind) q.set("kind", opts.kind);
    q.set("limit", String(opts.limit ?? 100));
    if (opts.before) q.set("before", opts.before);
    const page = await request<VoiceEventsPage>("GET", `${dev(id)}/events?${q}`);
    return { events: page?.events ?? [], next: page?.next ?? null };
  },
};
