import { createEffect, createSignal, on, type Accessor } from "solid-js";
import { pixivApi } from "../api/pixiv";

// Pixiv images come as signed links, /v1/pixiv/s/<expiry>/<token>/<sig> (the token is the i.pximg.net URL in hex).
// A page opened from its cached copy (lib/pageCache, 7 days) can hold links that have expired since: the proxy then
// answers 403 and the grid shows broken images (owner, 2026-11-01: "pixiv always open like this"). Links are signed
// for 30 days now; an older or failing one is re-signed here, in one request for all the images that ask at once.

/** Unix seconds a signed link expires at; null for anything else (real expiries are 10-digit timestamps). */
export function signedExpiry(url: string): number | null {
  const m = /^\/v1\/pixiv\/s\/(\d{10,})\/([0-9a-f]+)\/[0-9a-f]+$/.exec(url);
  return m ? Number(m[1]) : null;
}

/** The link has expired, or will within `marginSecs`. */
export function isExpired(url: string, nowSecs = Date.now() / 1000, marginSecs = 120): boolean {
  const exp = signedExpiry(url);
  return exp !== null && exp - marginSecs <= nowSecs;
}

function tokenOf(url: string): string | null {
  return /^\/v1\/pixiv\/s\/\d+\/([0-9a-f]+)\//.exec(url)?.[1] ?? null;
}

const BATCH_MS = 25;
let queued = new Map<string, ((url: string) => void)[]>();
let timer: ReturnType<typeof setTimeout> | undefined;

async function flush(): Promise<void> {
  const batch = queued;
  queued = new Map();
  timer = undefined;
  const tokens = [...batch.keys()];
  let urls: Record<string, string> = {};
  for (let i = 0; i < tokens.length; i += 300) {
    try {
      Object.assign(urls, await pixivApi.resign(tokens.slice(i, i + 300)));
    } catch {
      // Offline or refused: the callers keep the link they had.
    }
  }
  for (const [token, waiting] of batch) for (const done of waiting) done(urls[token] ?? "");
}

/** A link that loads: `url` itself while it is valid (unless `force`), else a freshly signed one; `url` on failure. */
export function freshUrl(url: string, force = false): Promise<string> {
  const token = tokenOf(url);
  if (!token || (!force && !isExpired(url))) return Promise.resolve(url);
  return new Promise((resolve) => {
    const waiting = queued.get(token) ?? [];
    waiting.push((fresh) => resolve(fresh || url));
    queued.set(token, waiting);
    timer ??= setTimeout(() => void flush(), BATCH_MS);
  });
}

/**
 * The `src` to show for a signed link: an expired one is re-signed before it is shown (nothing shows meanwhile, so no
 * broken-image icon), and an image that fails to load is re-signed once (`onError`).
 */
export function createFreshSrc(raw: Accessor<string>): { src: Accessor<string>; onError: () => void } {
  const [src, setSrc] = createSignal(isExpired(raw()) ? "" : raw());
  let retried = false;
  createEffect(
    on(raw, (url) => {
      retried = false;
      if (!url || !isExpired(url)) return setSrc(url);
      setSrc("");
      void freshUrl(url).then((fresh) => raw() === url && setSrc(fresh));
    }),
  );
  return {
    src,
    onError() {
      const url = raw();
      if (retried || !url) return;
      retried = true;
      void freshUrl(url, true).then((fresh) => raw() === url && fresh !== src() && setSrc(fresh));
    },
  };
}
