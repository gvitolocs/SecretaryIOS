// lucina-media playback: audio and video need a short-lived token, so the
// browser asks for a play URL on first use instead of linking `/original`.
//
// The server answers POST /v1/media/items/{id}/play with a tokenised
// `/v1/media/s/{token}/{id}/file` URL (the exact field has varied between
// versions; accept the common shapes).

export interface MediaItem {
  id: string;
  kind?: string;
  thumbhash?: string;
  width?: number;
  height?: number;
  durationMs?: number;
  urls?: { original?: string; grid?: string; preview?: string; file?: string };
}

const CSRF = { "X-Secretary-CSRF": "1" };

/** Same-origin paths only; anything else (absolute remote URLs, `//host`) is dropped. */
export function sameOriginPath(value: unknown): string | undefined {
  if (typeof value !== "string" || !value.startsWith("/") || value.startsWith("//")) return undefined;
  return value;
}

function finite(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : undefined;
}

export function parseMediaItem(raw: unknown, id: string): MediaItem {
  const body = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const item: MediaItem = { id };
  if (typeof body.kind === "string" && body.kind) item.kind = body.kind;
  if (typeof body.thumbhash === "string" && body.thumbhash) item.thumbhash = body.thumbhash.slice(0, 400);
  for (const key of ["width", "height", "durationMs"] as const) {
    const value = finite(body[key]);
    if (value !== undefined) item[key] = value;
  }
  if (body.urls && typeof body.urls === "object") {
    const urls: NonNullable<MediaItem["urls"]> = {};
    for (const key of ["original", "grid", "preview", "file"] as const) {
      const value = sameOriginPath((body.urls as Record<string, unknown>)[key]);
      if (value) urls[key] = value;
    }
    item.urls = urls;
  }
  return item;
}

const items = new Map<string, Promise<MediaItem | null>>();

/**
 * GET /v1/media/items/{id} (ThumbHash, dimensions and rendition URLs), once per
 * item per tab. Resolves null on failure; a failed lookup is retried next time.
 */
export function getMediaItem(id: string): Promise<MediaItem | null> {
  const cached = items.get(id);
  if (cached) return cached;
  const pending = fetch(`/v1/media/items/${encodeURIComponent(id)}`, {
    method: "GET",
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  })
    .then(async (res) => (res.ok ? parseMediaItem(await res.json().catch(() => null), id) : null))
    .catch(() => null)
    .then((item) => {
      if (!item) items.delete(id);
      return item;
    });
  items.set(id, pending);
  return pending;
}

/** Test helper: forget cached media items. */
export function clearMediaItems(): void {
  items.clear();
}

function playUrlFrom(raw: unknown, id: string): string | null {
  if (!raw || typeof raw !== "object") return null;
  const body = raw as Record<string, unknown>;
  for (const key of ["url", "playUrl", "fileUrl", "src"] as const) {
    const value = body[key];
    if (typeof value === "string" && value) return value;
  }
  const urls = body.urls;
  if (urls && typeof urls === "object") {
    for (const key of ["file", "play", "original"] as const) {
      const value = (urls as Record<string, unknown>)[key];
      if (typeof value === "string" && value) return value;
    }
  }
  const token = body.token;
  if (typeof token === "string" && token) return `/v1/media/s/${encodeURIComponent(token)}/${encodeURIComponent(id)}/file`;
  return null;
}

/** Ask the server for a playable (tokenised) audio/video URL. */
export async function playMedia(id: string): Promise<string> {
  const res = await fetch(`/v1/media/items/${encodeURIComponent(id)}/play`, {
    method: "POST",
    credentials: "same-origin",
    headers: CSRF,
    cache: "no-store",
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(body || `Play failed (${res.status})`);
  }
  const body: unknown = await res.json().catch(() => null);
  const url = playUrlFrom(body, id);
  if (!url) throw new Error("No playable URL");
  // Same-origin tokenised URLs only; never follow a remote URL.
  const resolved = new URL(url, window.location.origin);
  if (resolved.origin !== window.location.origin) throw new Error("No playable URL");
  return resolved.pathname + resolved.search;
}
