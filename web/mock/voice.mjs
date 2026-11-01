// The LucinaVoice device API for the dev mock (docs/lucinavoice-protocol.md section 5):
//   GET /v1/voice/devices
//   GET /v1/voice/devices/{id}/status
//   GET /v1/voice/devices/{id}/events?kind=&limit=&before=   newest first, {events, next?}
// One online speaker ("Kitchen speaker") with 10 turns (fast, slow, one error, one over HTTP) and
// events of every kind. MOCK_VOICE=offline: the speaker is offline; MOCK_VOICE=empty: no devices.

const ID = "lucinavoice-dab8";
const min = (m) => new Date(Date.now() - m * 60_000).toISOString();
const sec = (s) => new Date(Date.now() - s * 1000).toISOString();

const mode = () => process.env.MOCK_VOICE ?? "";
const online = () => mode() !== "offline";

// Spans are all from the end of speech, so each is the moment a stage finished.
const TURNS = [
  { ago: 3, ok: true, chars: 31, speech: 1900, spans: { stt: 380, lucina_first: 760, tts_first: 1010, first_audio: 1060, total: 3400 }, dev: 1130, up: 61_440, down: 188_416 },
  { ago: 9, ok: true, chars: 52, speech: 3100, spans: { stt: 410, lucina_first: 880, tts_first: 1130, first_audio: 1190, total: 4800 }, dev: 1270, up: 99_328, down: 276_480 },
  { ago: 21, ok: true, chars: 24, speech: 1500, spans: { stt: 350, lucina_first: 2900, tts_first: 3400, first_audio: 3460, total: 7200 }, dev: 3540, up: 48_128, down: 241_664 },
  { ago: 34, ok: false, err: "lucina_unavailable", chars: 40, speech: 2400, spans: { stt: 690, total: 8100 }, up: 76_800, down: 2_048 },
  { ago: 47, ok: true, chars: 18, speech: 1200, spans: { stt: 330, lucina_first: 690, tts_first: 940, first_audio: 990, total: 2600 }, dev: 1050, up: 38_912, down: 121_856 },
  { ago: 62, ok: true, chars: 88, speech: 5200, spans: { stt: 940, lucina_first: 4100, tts_first: 5200, first_audio: 5290, total: 12_800 }, dev: 5380, up: 163_840, down: 593_920 },
  { ago: 80, ok: true, chars: 29, speech: 1800, spans: { stt: 400, lucina_first: 820, tts_first: 1080, first_audio: 1130, total: 3900 }, dev: 1210, up: 57_344, down: 207_872, mode: "http" },
  { ago: 101, ok: true, chars: 36, speech: 2200, spans: { stt: 370, lucina_first: 790, tts_first: 1040, first_audio: 1090, total: 4100 }, dev: 1160, up: 70_656, down: 223_232 },
  { ago: 128, ok: true, chars: 21, speech: 1400, spans: { stt: 360, lucina_first: 710, tts_first: 960, first_audio: 1010, total: 2900 }, dev: 1080, up: 43_008, down: 146_432 },
  { ago: 156, ok: true, chars: 47, speech: 2900, spans: { stt: 430, lucina_first: 1500, tts_first: 1860, first_audio: 1930, total: 6100 }, dev: 2010, up: 92_160, down: 319_488 },
];

function turns() {
  return TURNS.map((x) => ({
    at: min(x.ago),
    ok: x.ok,
    ...(x.err ? { err: x.err } : {}),
    transcript_chars: x.chars,
    speech_ms: x.speech,
    spans: x.spans,
    ...(x.dev ? { device_first_audio_ms: x.dev } : {}),
    up_bytes: x.up,
    down_bytes: x.down,
  }));
}

/** Newest first. Events of every kind: boot, wifi, link, turn, stats and error. */
function events() {
  const out = [];
  const add = (ago, k, body) => out.push({ at: typeof ago === "number" ? min(ago) : ago, k, ...body });
  out.push({ at: sec(40), k: "stats", ...stats(10) });
  TURNS.forEach((x) => {
    add(x.ago, "turn", {
      ok: x.ok,
      ...(x.err ? { err: x.err } : {}),
      mode: x.mode ?? "link",
      speech_ms: x.speech,
      ...(x.dev ? { first_audio_ms: x.dev } : {}),
      total_ms: x.spans.total,
      up_bytes: x.up,
      down_bytes: x.down,
      ...(x.ok ? { server_ms: x.spans } : {}),
    });
    if (!x.ok) add(x.ago - 0.1, "error", { what: "turn_failed", detail: x.err });
  });
  // A stats event every 5 minutes for the last three hours.
  for (let m = 5; m <= 180; m += 5) add(m + 0.4, "stats", stats(m));
  add(22.5, "wifi", { up: false, reason: "beacon_timeout" });
  add(22.2, "wifi", { up: true, rssi: -71 });
  add(22.1, "link", { up: true, ms: 912 });
  add(22.4, "link", { up: false, err: "ws_closed" });
  add(44, "error", { what: "i2s_underrun", detail: "player starved for 120 ms" });
  add(178, "link", { up: true, ms: 640 });
  add(178.5, "wifi", { up: true, rssi: -58 });
  add(179, "boot", { reset: "poweron", seq: 7 });
  return out.sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
}

function stats(minutesAgo) {
  const up = Math.max(0, 11_000_000 - minutesAgo * 60_000);
  return {
    uptime_ms: up,
    heap_free: 182_000 + ((minutesAgo * 37) % 9000),
    heap_min_free: 141_000,
    rssi: -61 - (minutesAgo % 4),
    up_bytes: 2_400_000 - minutesAgo * 900,
    down_bytes: 7_900_000 - minutesAgo * 2400,
    tx_air_ms: 41_000,
    cpu_mhz: 240,
    power: { est_mw: 612 + (minutesAgo % 3) * 9, energy_mwh: 4300 - minutesAgo * 2 },
  };
}

function summary() {
  return { id: ID, name: "Kitchen speaker", online: online(), last_seen: online() ? sec(8) : min(95), fw: "0.4.1", rssi: online() ? -61 : null, est_mw: online() ? 618 : null };
}

function status() {
  const s = stats(0.2);
  return {
    id: ID,
    name: "Kitchen speaker",
    online: online(),
    ...(online() ? { connected_since: min(42) } : {}),
    last_seen: online() ? sec(8) : min(95),
    specs: { chip: "esp32", rev: "v3.1", cores: 2, cpu_mhz: 240, flash_bytes: 4_194_304, psram_bytes: 0, fw: "0.4.1", commit: "1f3ea08", mac: "da:b8", heap_total: 320_000, ina: null, temp_sensor: false },
    live: { at: sec(40), uptime_ms: s.uptime_ms, heap_free: s.heap_free, heap_min_free: s.heap_min_free, rssi: s.rssi, up_bytes: s.up_bytes, down_bytes: s.down_bytes, tx_air_ms: s.tx_air_ms, cpu_mhz: s.cpu_mhz },
    network: { today_up_bytes: 812_000, today_down_bytes: 2_650_000, total_up_bytes: s.up_bytes, total_down_bytes: s.down_bytes },
    power: { est_mw: s.power.est_mw, measured: false, energy_mwh_today: 2315.4 },
    turns: turns(),
  };
}

export async function handleVoice(req, res, { path, method, url, send, fail }) {
  if (!path.startsWith("/v1/voice/devices") || method !== "GET") return false;
  if (path === "/v1/voice/devices") {
    send(res, 200, mode() === "empty" ? [] : [summary()]);
    return true;
  }
  const m = /^\/v1\/voice\/devices\/([^/]+)\/(status|events)$/.exec(path);
  if (!m || decodeURIComponent(m[1]) !== ID || mode() === "empty") {
    fail(res, 404, "not_found", "no such voice device");
    return true;
  }
  if (m[2] === "status") {
    send(res, 200, status());
    return true;
  }
  const kind = url.searchParams.get("kind");
  const before = url.searchParams.get("before");
  const limit = Math.max(1, Math.min(Number(url.searchParams.get("limit")) || 100, 500));
  let list = events().filter((e) => !kind || e.k === kind);
  if (before) list = list.filter((e) => Date.parse(e.at) < Date.parse(before));
  const page = list.slice(0, limit);
  send(res, 200, { events: page, ...(list.length > limit ? { next: page[page.length - 1].at } : {}) });
  return true;
}
