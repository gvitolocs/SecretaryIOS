import { request } from "./client";

// Voice Learn: the shared custom vocabulary of every speech-to-text path (docs/lucinavoice-protocol.md section 8).
// Lucina owns the entries; secretary-server proxies them:
//   GET    /v1/voice/learn                  -> the entries (an array, or an object holding them)
//   POST   /v1/voice/learn                  -> the new entry (201)
//   PATCH  /v1/voice/learn/{id}             -> the changed entry
//   DELETE /v1/voice/learn/{id}
//   POST   /v1/voice/learn/suggest          {transcript, corrected} -> candidate entries
//   POST   /v1/voice/learn/sample           a ≤ 10 s PCM WAV clip   -> {heard, corrections}; nothing is stored
// Everything from the network is untrusted: the parsers keep what they understand and drop the rest.

export const LEARN_LANGS = ["it", "en", "da"] as const;
export type LearnLang = (typeof LEARN_LANGS)[number];

/** Limits Lucina enforces; the form mirrors them so most mistakes are caught before the request. */
export const MAX_ENTRIES = 500;
export const MAX_FIELD_CHARS = 100;
export const MAX_SAMPLE_SECONDS = 10;

export interface LearnEntry {
  id: string;
  word: string;
  soundsLike: string[];
  meaning?: string;
  lang?: LearnLang;
  source?: "app" | "correction";
  createdAt?: string;
  updatedAt?: string;
}

/** What the form sends (POST) or changes (PATCH). `lang: null` clears it. */
export interface LearnDraft {
  word?: string;
  soundsLike?: string[];
  meaning?: string;
  lang?: LearnLang | null;
}

/** A candidate from a transcript edit: the same fields as an entry, without the id. */
export interface LearnSuggestion {
  word: string;
  soundsLike: string[];
  meaning?: string;
  lang?: LearnLang;
}

export interface SampleResult {
  heard: string;
  corrections: { from: string; to: string }[];
}

const text = (v: unknown, max = MAX_FIELD_CHARS): string | undefined =>
  typeof v === "string" && v.trim() ? v.trim().slice(0, max) : undefined;

const lang = (v: unknown): LearnLang | undefined => (typeof v === "string" && (LEARN_LANGS as readonly string[]).includes(v) ? (v as LearnLang) : undefined);

function words(v: unknown): string[] {
  const list = Array.isArray(v) ? v : typeof v === "string" ? v.split(",") : [];
  const out: string[] = [];
  for (const w of list) {
    const t = text(w);
    if (t && !out.includes(t)) out.push(t);
  }
  return out.slice(0, 20);
}

/** One entry, or null when it has no id or word. */
export function parseEntry(v: unknown): LearnEntry | null {
  if (!v || typeof v !== "object") return null;
  const r = v as Record<string, unknown>;
  const id = typeof r.id === "string" || typeof r.id === "number" ? String(r.id) : "";
  const word = text(r.word);
  if (!id || !word) return null;
  const e: LearnEntry = { id, word, soundsLike: words(r.soundsLike) };
  const meaning = text(r.meaning, 400);
  if (meaning) e.meaning = meaning;
  const l = lang(r.lang);
  if (l) e.lang = l;
  if (r.source === "app" || r.source === "correction") e.source = r.source;
  if (typeof r.createdAt === "string") e.createdAt = r.createdAt;
  if (typeof r.updatedAt === "string") e.updatedAt = r.updatedAt;
  return e;
}

/** The list inside Lucina's answer: a bare array, or an object under `entries`, `items` or `words`. */
function listOf(raw: unknown): unknown[] {
  if (Array.isArray(raw)) return raw;
  if (raw && typeof raw === "object") {
    for (const key of ["entries", "items", "words", "candidates", "suggestions"]) {
      const v = (raw as Record<string, unknown>)[key];
      if (Array.isArray(v)) return v;
    }
  }
  return [];
}

export function parseEntries(raw: unknown): LearnEntry[] {
  return listOf(raw).flatMap((v) => {
    const e = parseEntry(v);
    return e ? [e] : [];
  });
}

/** The entry of a create or update answer: the entry itself or `{entry}`. */
export function parseSaved(raw: unknown): LearnEntry | null {
  const inner = raw && typeof raw === "object" ? (raw as Record<string, unknown>).entry : undefined;
  return parseEntry(inner) ?? parseEntry(raw);
}

export function parseSuggestions(raw: unknown): LearnSuggestion[] {
  const items = listOf(raw);
  // A single suggestion may come as the entry itself.
  const source = items.length || !raw || typeof raw !== "object" || !(raw as Record<string, unknown>).word ? items : [raw];
  const out: LearnSuggestion[] = [];
  for (const v of source) {
    if (!v || typeof v !== "object") continue;
    const r = v as Record<string, unknown>;
    const word = text(r.word);
    if (!word) continue;
    const s: LearnSuggestion = { word, soundsLike: words(r.soundsLike) };
    const meaning = text(r.meaning, 400);
    if (meaning) s.meaning = meaning;
    const l = lang(r.lang);
    if (l) s.lang = l;
    out.push(s);
  }
  return out;
}

export function parseSample(raw: unknown): SampleResult {
  const r = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const corrections: SampleResult["corrections"] = [];
  if (Array.isArray(r.corrections)) {
    for (const c of r.corrections.slice(0, 16)) {
      const o = (c && typeof c === "object" ? c : {}) as Record<string, unknown>;
      const from = text(o.from);
      const to = text(o.to);
      if (from && to) corrections.push({ from, to });
    }
  }
  return { heard: typeof r.heard === "string" ? r.heard.trim() : "", corrections };
}

const base = "/v1/voice/learn";

export const voiceLearnApi = {
  async list(): Promise<LearnEntry[]> {
    return parseEntries(await request<unknown>("GET", base));
  },

  async create(draft: LearnDraft): Promise<LearnEntry | null> {
    return parseSaved(await request<unknown>("POST", base, { body: draft }));
  },

  async update(id: string, draft: LearnDraft): Promise<LearnEntry | null> {
    return parseSaved(await request<unknown>("PATCH", `${base}/${encodeURIComponent(id)}`, { body: draft }));
  },

  async remove(id: string): Promise<void> {
    await request<unknown>("DELETE", `${base}/${encodeURIComponent(id)}`);
  },

  /** Candidate entries from a transcript edit (the "add to Voice Learn" offer). */
  async suggest(transcript: string, corrected: string): Promise<LearnSuggestion[]> {
    return parseSuggestions(await request<unknown>("POST", `${base}/suggest`, { body: { transcript, corrected } }));
  },

  /** What the speech-to-text engine heard in a short clip (16 kHz PCM WAV). Nothing is stored. */
  async sample(wav: Blob, lang?: LearnLang): Promise<SampleResult> {
    const q = lang ? `?lang=${lang}` : "";
    return parseSample(await request<unknown>("POST", `${base}/sample${q}`, { raw: { body: wav, contentType: "audio/wav" }, timeoutMs: 60_000 }));
  },
};

/** "a, b ,c" → ["a", "b", "c"], without empties and duplicates. */
export function splitSoundsLike(input: string): string[] {
  return words(input);
}
