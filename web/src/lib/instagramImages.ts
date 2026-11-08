import { createEffect, createSignal, on, onCleanup, type Accessor } from "solid-js";
import { instagramApi, type IgItem } from "../api/instagram";

// Instagram media comes as signed links that live about an hour (/v1/instagram/s/<expiry>/<sig>/<token>, /a/ for saved
// items), while a page opens on its copy of up to 7 days ago (lib/pageCache). Asking for an expired link is a 403 and a
// broken tile: that was every first open of the page (2026-11-01, 116 of them in a day). So an expired link is re-signed
// before it is shown, all the images that ask at once in one request (POST /v1/instagram/resign, which takes only links
// the service itself signed). A picture that fails to load is asked again, with a clear state on the tile meanwhile.

/** Unix seconds a signed link expires at; null for anything else (real expiries are 10-digit timestamps). */
export function signedExpiry(url: string): number | null {
  const m = /^\/v1\/instagram\/[sa]\/(\d{10,})\//.exec(url);
  return m ? Number(m[1]) : null;
}

/** The link has expired, or will within `marginSecs`. */
export function isExpired(url: string, nowSecs = Date.now() / 1000, marginSecs = 120): boolean {
  const exp = signedExpiry(url);
  return exp !== null && exp - marginSecs <= nowSecs;
}

const BATCH_MS = 25;
const BATCH_SIZE = 400;
let queued = new Map<string, ((url: string) => void)[]>();
let timer: ReturnType<typeof setTimeout> | undefined;

async function flush(): Promise<void> {
  const batch = queued;
  queued = new Map();
  timer = undefined;
  const urls = [...batch.keys()];
  const fresh: Record<string, string> = {};
  for (let i = 0; i < urls.length; i += BATCH_SIZE) {
    try {
      Object.assign(fresh, await instagramApi.resign(urls.slice(i, i + BATCH_SIZE)));
    } catch {
      // Offline or refused: the callers keep the link they had.
    }
  }
  for (const [url, waiting] of batch) for (const done of waiting) done(fresh[url] ?? url);
}

/** A link that loads: `url` itself while it is valid (unless `force`), else a freshly signed one; `url` on failure. */
export function freshUrl(url: string, force = false): Promise<string> {
  if (signedExpiry(url) === null || (!force && !isExpired(url))) return Promise.resolve(url);
  return new Promise((resolve) => {
    const waiting = queued.get(url) ?? [];
    waiting.push(resolve);
    queued.set(url, waiting);
    timer ??= setTimeout(() => void flush(), BATCH_MS);
  });
}

/** How long after each failure a picture is asked again, then every last value while it is on screen. */
const RETRY_MS = [0, 3_000, 15_000, 60_000];

export interface FreshSrc {
  src: Accessor<string>;
  /** The picture failed and is being asked again: hide the browser's broken image. */
  retrying: Accessor<boolean>;
  /** Asked more than once and still failing: say so. It keeps being asked. */
  failed: Accessor<boolean>;
  onLoad: () => void;
  onError: () => void;
}

/**
 * The `src` to show for a signed link. An expired one is re-signed before it is shown; one that fails is re-signed and
 * asked again, with `?r=<n>` so the browser does not answer from its failure. `onGiveUp` runs once, when it first counts
 * as failed (the page may ask for a newer link than this copy has).
 */
export function createFreshSrc(raw: Accessor<string>, onGiveUp?: () => void): FreshSrc {
  const [src, setSrc] = createSignal(isExpired(raw()) ? "" : raw());
  const [retrying, setRetrying] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  let attempts = 0;
  let retry: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(retry));

  createEffect(
    on(raw, (url) => {
      clearTimeout(retry);
      attempts = 0;
      setRetrying(false);
      setFailed(false);
      if (!url || !isExpired(url)) return setSrc(url);
      setSrc("");
      void freshUrl(url).then((fresh) => raw() === url && setSrc(fresh));
    }),
  );

  return {
    src,
    retrying,
    failed,
    onLoad() {
      clearTimeout(retry);
      attempts = 0;
      setRetrying(false);
      setFailed(false);
    },
    onError() {
      const url = raw();
      if (!url) return;
      setRetrying(true);
      if (attempts === 2) {
        setFailed(true);
        onGiveUp?.();
      }
      const n = attempts++;
      clearTimeout(retry);
      retry = setTimeout(() => {
        void freshUrl(url, true).then((fresh) => raw() === url && setSrc(`${fresh}?r=${n + 1}`));
      }, RETRY_MS[Math.min(n, RETRY_MS.length - 1)]);
    },
  };
}

function withoutSignatures(item: IgItem): string {
  return JSON.stringify(item).replace(/\/v1\/instagram\/([sa])\/\d+\/[0-9a-f]+\//g, "/$1/");
}

/**
 * A refreshed list over the one on screen: an item that differs only in its signatures keeps its old object while those
 * are good for a while, so its tile is not rebuilt and the picture does not load again.
 */
export function keepSigned(old: IgItem[], fresh: IgItem[]): IgItem[] {
  const byId = new Map(old.map((item) => [item.id, item]));
  return fresh.map((item) => {
    const previous = byId.get(item.id);
    if (!previous || isExpired(previous.url, Date.now() / 1000, 600)) return item;
    return withoutSignatures(previous) === withoutSignatures(item) ? previous : item;
  });
}
