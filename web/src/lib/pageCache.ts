// The plugin pages' cache (Instagram, Pixiv, Notebooks): what a page showed last, so a visit renders at once while a
// background fetch checks for changes (stale-while-revalidate). IndexedDB keeps it across visits and app starts, a
// memory mirror makes reads synchronous for the first frame (Back, a remount), and memory alone serves when IndexedDB
// is missing or throws (private windows, blocked site data). Owner-only data: sign-out and the plugins' Disconnect
// clear it. Nothing here is the source of truth.
//
// Each entry lives twice: its value in "entries", and its key, scope, size and times in "meta", so eviction and a
// scope's clear read only the small rows. The total is capped (least recently used goes first) and an entry older
// than `maxAgeMs` is dropped when read.

export type PageScope = "instagram" | "pixiv" | "notebooks";

const SCOPES: PageScope[] = ["instagram", "pixiv", "notebooks"];

interface Meta {
  key: string;
  scope: PageScope;
  /** The value's JSON length in UTF-16 code units, times two. */
  bytes: number;
  savedAt: number;
  usedAt: number;
}

interface Options {
  name?: string;
  maxBytes?: number;
  maxAgeMs?: number;
  now?: () => number;
}

export function createPageCache({ name = "secretary-pages", maxBytes = 50 * 1024 * 1024, maxAgeMs = 7 * 24 * 3600 * 1000, now = Date.now }: Options = {}) {
  const values = new Map<string, unknown>();
  // The entries' meta: this session's at once, every stored one merged in the first time IndexedDB is asked.
  const metas = new Map<string, Meta>();
  let metasLoaded = false;
  // Route UI state and scroll offsets belong to this session, never to IndexedDB.
  const snapshots = new Map<string, unknown>();
  // A clear bumps its scopes' revision: answers and writes started before it are dropped.
  const revisions = new Map<PageScope, number>();
  let broken = false;
  let opening: Promise<IDBDatabase | null> | undefined;
  let pending = Promise.resolve();

  function open(): Promise<IDBDatabase | null> {
    if (broken || typeof indexedDB === "undefined") return Promise.resolve(null);
    return (opening ??= new Promise((resolve) => {
      try {
        const req = indexedDB.open(name, 1);
        req.onupgradeneeded = () => {
          req.result.createObjectStore("entries", { keyPath: "key" });
          req.result.createObjectStore("meta", { keyPath: "key" });
        };
        req.onsuccess = () => {
          req.result.onversionchange = () => {
            req.result.close();
            opening = undefined;
          };
          resolve(req.result);
        };
        req.onerror = req.onblocked = () => {
          broken = true;
          resolve(null);
        };
      } catch {
        broken = true;
        resolve(null);
      }
    }));
  }

  async function tx<T>(stores: string[], mode: IDBTransactionMode, run: (t: IDBTransaction) => IDBRequest<T> | void): Promise<T | undefined> {
    try {
      const db = await open();
      if (!db) return undefined;
      return await new Promise<T | undefined>((resolve, reject) => {
        const t = db.transaction(stores, mode);
        const req = run(t);
        t.oncomplete = () => resolve(req ? req.result : undefined);
        t.onerror = t.onabort = () => reject(t.error);
      });
    } catch {
      broken = true;
      return undefined;
    }
  }

  /** IndexedDB work runs one step at a time, in order. */
  function enqueue<T>(run: () => Promise<T>): Promise<T> {
    const result = pending.then(run);
    pending = result.then(
      () => {},
      () => {},
    );
    return result;
  }

  async function loadMetas(): Promise<Map<string, Meta>> {
    if (!metasLoaded) {
      metasLoaded = true;
      const stored = (await tx<Meta[]>(["meta"], "readonly", (t) => t.objectStore("meta").getAll())) ?? [];
      // This session's writes are newer than what was stored.
      for (const m of stored) if (!metas.has(m.key)) metas.set(m.key, m);
    }
    return metas;
  }

  async function remove(keys: string[]): Promise<void> {
    if (!keys.length) return;
    for (const key of keys) {
      values.delete(key);
      metas.delete(key);
    }
    await tx(["entries", "meta"], "readwrite", (t) => {
      for (const key of keys) {
        t.objectStore("entries").delete(key);
        t.objectStore("meta").delete(key);
      }
    });
  }

  const expired = (meta: Meta) => now() - meta.savedAt > maxAgeMs;
  const revision = (scope: PageScope) => revisions.get(scope) ?? 0;

  /** This session's copy, synchronously; undefined when it was never read or written here, or has expired. */
  function peek<T>(key: string): { value: T; savedAt: number } | undefined {
    const meta = metas.get(key);
    if (!meta || !values.has(key)) return undefined;
    if (expired(meta)) {
      void enqueue(() => remove([key]));
      return undefined;
    }
    return { value: values.get(key) as T, savedAt: meta.savedAt };
  }

  return {
    peek,
    revision,
    snapshots,
    /** The stored copy (and it is now the most recently used), or undefined when missing or expired. */
    get<T>(key: string): Promise<{ value: T; savedAt: number } | undefined> {
      const asked = new Map(revisions);
      return enqueue(async () => {
        const all = await loadMetas();
        const meta = all.get(key);
        if (!meta || (asked.get(meta.scope) ?? 0) !== revision(meta.scope)) return undefined;
        if (expired(meta)) {
          await remove([key]);
          return undefined;
        }
        if (!values.has(key)) {
          const stored = await tx<{ key: string; value: unknown }>(["entries"], "readonly", (t) => t.objectStore("entries").get(key));
          if (!stored) {
            await remove([key]);
            return undefined;
          }
          values.set(key, stored.value);
        }
        meta.usedAt = now();
        await tx(["meta"], "readwrite", (t) => t.objectStore("meta").put(meta));
        return { value: values.get(key) as T, savedAt: meta.savedAt };
      });
    },
    /** Stores `value` (visible to `peek` at once), then evicts the least recently used past `maxBytes`. */
    put(key: string, scope: PageScope, value: unknown): Promise<void> {
      const at = now();
      const meta: Meta = { key, scope, bytes: JSON.stringify(value).length * 2, savedAt: at, usedAt: at };
      const asked = revision(scope);
      values.set(key, value);
      metas.set(key, meta);
      return enqueue(async () => {
        if (asked !== revision(scope)) return;
        const all = await loadMetas();
        all.set(key, meta);
        await tx(["entries", "meta"], "readwrite", (t) => {
          t.objectStore("entries").put({ key, value });
          t.objectStore("meta").put(meta);
        });
        let bytes = 0;
        for (const m of all.values()) bytes += m.bytes;
        const evict: string[] = [];
        for (const m of [...all.values()].sort((a, b) => a.usedAt - b.usedAt)) {
          if (bytes <= maxBytes) break;
          bytes -= m.bytes;
          evict.push(m.key);
        }
        await remove(evict);
      });
    },
    /** Forgets one scope (a plugin's Disconnect) or everything (sign-out), with the route snapshots. */
    clear(scope?: PageScope): Promise<void> {
      const scopes = scope ? [scope] : SCOPES;
      for (const s of scopes) revisions.set(s, revision(s) + 1);
      for (const [key, meta] of metas) {
        if (scopes.includes(meta.scope)) {
          values.delete(key);
          metas.delete(key);
        }
      }
      for (const key of snapshots.keys()) if (!scope || key.startsWith(`${scope}:`)) snapshots.delete(key);
      return enqueue(async () => {
        if (!scope) {
          values.clear();
          metas.clear();
          metasLoaded = true;
          await tx(["entries", "meta"], "readwrite", (t) => {
            t.objectStore("entries").clear();
            t.objectStore("meta").clear();
          });
          return;
        }
        const stored = (await tx<Meta[]>(["meta"], "readonly", (t) => t.objectStore("meta").getAll())) ?? [];
        await remove(stored.filter((m) => m.scope === scope).map((m) => m.key));
      });
    },
  };
}

export const pageCache = createPageCache();

/** Reuses unchanged objects, so Solid's keyed For keeps their DOM nodes. */
export function stableItems<T extends { id: string }>(old: T[], fresh: T[]): T[] {
  const byId = new Map(old.map((item) => [item.id, item]));
  return fresh.map((item) => {
    const previous = byId.get(item.id);
    return previous && JSON.stringify(previous) === JSON.stringify(item) ? previous : item;
  });
}

/** A refreshed first page over a cached list: the fresh items first, in the server's order, then the rest. */
export function mergeFirstPage<T extends { id: string }>(old: T[], fresh: T[]): T[] {
  const ids = new Set(fresh.map((item) => item.id));
  return [...stableItems(old, fresh), ...old.filter((item) => !ids.has(item.id))].slice(0, 300);
}
