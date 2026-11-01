import type { VoiceDeviceSummary, VoiceEvent, VoiceStatus, VoiceTurn } from "../api/voice";

// Fixtures for the Voice device page: a device status (/v1/voice/devices/{id}/status), its turns and events.

export const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();

export function turn(over: Partial<VoiceTurn> = {}): VoiceTurn {
  return {
    at: ago(3),
    ok: true,
    transcript_chars: 31,
    speech_ms: 1900,
    spans: { stt: 380, lucina_first: 760, tts_first: 1010, first_audio: 1060, total: 3400 },
    device_first_audio_ms: 1130,
    up_bytes: 61_440,
    down_bytes: 188_416,
    ...over,
  };
}

export function status(over: Partial<VoiceStatus> = {}): VoiceStatus {
  return {
    id: "dab8",
    name: "Kitchen speaker",
    online: true,
    connected_since: ago(42),
    last_seen: ago(0.1),
    specs: { chip: "esp32", rev: "v3.1", cores: 2, cpu_mhz: 240, flash_bytes: 4_194_304, psram_bytes: 0, fw: "0.4.1", commit: "1f3ea08", mac: "da:b8", heap_total: 320_000, ina: null, temp_sensor: false },
    live: { at: ago(0.5), uptime_ms: 11_100_000, heap_free: 186_368, heap_min_free: 144_384, rssi: -61, up_bytes: 2_400_000, down_bytes: 7_900_000, tx_air_ms: 41_000, cpu_mhz: 240 },
    network: { today_up_bytes: 812_000, today_down_bytes: 2_650_000, total_up_bytes: 2_400_000, total_down_bytes: 7_900_000 },
    power: { est_mw: 618, measured: false, energy_mwh_today: 2315.4 },
    turns: [
      turn(),
      turn({ at: ago(21), spans: { stt: 350, lucina_first: 2900, tts_first: 3400, first_audio: 3460, total: 7200 }, device_first_audio_ms: 3540 }),
      turn({ at: ago(34), ok: false, err: "lucina_unavailable", spans: { stt: 690, total: 8100 }, device_first_audio_ms: undefined, down_bytes: 2048 }),
    ],
    ...over,
  };
}

export function device(over: Partial<VoiceDeviceSummary> = {}): VoiceDeviceSummary {
  return { id: "dab8", name: "Kitchen speaker", online: true, last_seen: ago(0.1), fw: "0.4.1", rssi: -61, est_mw: 618, ...over };
}

export function event(k: string, minutesAgo: number, body: Record<string, unknown> = {}): VoiceEvent {
  return { at: ago(minutesAgo), k, ...body };
}

/** One event of every kind, newest first. */
export function events(): VoiceEvent[] {
  return [
    event("stats", 1, { uptime_ms: 11_100_000, heap_free: 186_368, heap_min_free: 144_384, rssi: -62, up_bytes: 1, down_bytes: 2, tx_air_ms: 3, cpu_mhz: 240 }),
    event("turn", 3, { ok: true, mode: "link", speech_ms: 1900, first_audio_ms: 1130, total_ms: 3400, up_bytes: 61_440, down_bytes: 188_416 }),
    event("turn", 34, { ok: false, err: "lucina_unavailable", mode: "http", speech_ms: 2400, total_ms: 8100, up_bytes: 1, down_bytes: 2 }),
    event("error", 44, { what: "i2s_underrun", detail: "player starved for 120 ms" }),
    event("link", 60, { up: true, ms: 912 }),
    event("wifi", 61, { up: false, reason: "beacon_timeout" }),
    event("boot", 179, { reset: "poweron", seq: 7 }),
  ];
}
