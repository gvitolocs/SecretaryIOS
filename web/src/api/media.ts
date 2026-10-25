// The media library and player (docs/media-player.md): /v1/media on lucina-media (nezopt),
// through p1's Caddy. `POST /v1/media/items/{id}/play` answers tokenised /v1/media/s/... URLs
// that <video>, hls.js and <track> load with no cookie or header.

import { request } from "./client";

export type MediaKind = "video" | "audio" | "photo";

export interface MediaItem {
  id: string;
  kind: MediaKind;
  title: string;
  mime?: string;
  album?: string;
  size?: number;
  duration_ms?: number | null;
  width?: number | null;
  height?: number | null;
  poster_url?: string;
  thumb_url?: string;
}

export interface MediaPage {
  items: MediaItem[];
  next_cursor: string | null;
}

export interface SubtitleTrack {
  id: string;
  lang: string | null;
  label: string;
  forced: boolean;
  /** `image`: DVD/Blu-ray pictures read by OCR on the server. */
  kind: "text" | "image";
  format: string;
  source: "file" | "embedded";
  url: string;
}

export interface ProgressEntry {
  id: string;
  position_ms: number;
  duration_ms: number;
  finished: boolean;
  updated_at: number;
  device?: string | null;
}

export interface PlayInfo {
  id: string;
  kind: MediaKind;
  title: string;
  mode: "direct" | "remux" | "transcode";
  url: string;
  file_url: string;
  hls_url: string | null;
  poster_url: string | null;
  duration_ms: number | null;
  subtitles: SubtitleTrack[];
  bitmap_subtitles: number;
  expires_at: number;
  progress: ProgressEntry | null;
  resume_ms: number;
}

/** The browser as the server's playback planner knows it. */
export function clientProfile(ua = navigator.userAgent): "web_safari" | "web_chrome" | "web_firefox" {
  if (/Firefox\//.test(ua)) return "web_firefox";
  if (/Safari\//.test(ua) && !/(Chrome|Chromium|CriOS|Edg)\//.test(ua)) return "web_safari";
  return "web_chrome";
}

export const mediaApi = {
  async items(kind: MediaKind, cursor?: string): Promise<MediaPage> {
    const q = new URLSearchParams({ kind, limit: "200" });
    if (cursor) q.set("cursor", cursor);
    return request<MediaPage>("GET", `/v1/media/items?${q}`);
  },

  /** Every page of a kind (the library is a few thousand items at most). */
  async allItems(kind: MediaKind): Promise<MediaItem[]> {
    const all: MediaItem[] = [];
    let cursor: string | undefined;
    do {
      const page = await this.items(kind, cursor);
      all.push(...page.items);
      cursor = page.next_cursor ?? undefined;
    } while (cursor && all.length < 2000);
    return all;
  },

  play(id: string): Promise<PlayInfo> {
    return request<PlayInfo>("POST", `/v1/media/items/${encodeURIComponent(id)}/play`, {
      body: { client: clientProfile(), device: "web" },
    });
  },

  async progress(since = 0): Promise<ProgressEntry[]> {
    const page = await request<{ entries: ProgressEntry[] }>("GET", `/v1/media/progress?since=${since}`);
    return page.entries;
  },

  push(entries: ProgressEntry[]): Promise<{ acks: { id: string; updated_at: number }[]; entries: ProgressEntry[] }> {
    return request("POST", "/v1/media/progress", { body: { entries } });
  },
};

export type SubtitleFetch = { state: "ready"; text: string; partial: boolean } | { state: "preparing" };

/** A subtitle track's WebVTT. The server answers 202 while it converts and marks a file
 *  still being read by OCR with `X-Subtitles-Partial`. */
export async function fetchSubtitles(url: string): Promise<SubtitleFetch> {
  const res = await fetch(url, { cache: "no-store" });
  if (res.status === 202) return { state: "preparing" };
  if (!res.ok) throw new Error(`subtitles: HTTP ${res.status}`);
  return { state: "ready", text: await res.text(), partial: res.headers.has("X-Subtitles-Partial") };
}

export function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "--:--";
  const s = Math.floor(seconds);
  const hh = Math.floor(s / 3600);
  const mm = Math.floor(s / 60) % 60;
  const ss = String(s % 60).padStart(2, "0");
  return hh > 0 ? `${hh}:${String(mm).padStart(2, "0")}:${ss}` : `${mm}:${ss}`;
}
