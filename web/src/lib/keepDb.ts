// IndexedDB cache for Keep: every full note plus the sync cursor and the label registry, so the page
// opens instantly and works offline until the next sync. Falls back to memory when IndexedDB is
// missing or throws (private windows, blocked site data). Nothing here is the source of truth.

import type { KeepNote, LabelInfo } from "../api/keep";

export interface KeepCache {
  notes: KeepNote[];
  cursor: string;
  labels: LabelInfo[];
}

export interface KeepDb {
  load(): Promise<KeepCache>;
  putNotes(notes: KeepNote[]): Promise<void>;
  removeNotes(ids: string[]): Promise<void>;
  setCursor(cursor: string): Promise<void>;
  setLabels(labels: LabelInfo[]): Promise<void>;
  clear(): Promise<void>;
}

const DB_NAME = "secretary-keep";

export function createKeepDb(name = DB_NAME): KeepDb {
  const mem = { notes: new Map<string, KeepNote>(), meta: new Map<string, unknown>() };
  let broken = false;
  let opening: Promise<IDBDatabase | null> | undefined;

  function open(): Promise<IDBDatabase | null> {
    if (broken || typeof indexedDB === "undefined") return Promise.resolve(null);
    return (opening ??= new Promise((resolve) => {
      try {
        const req = indexedDB.open(name, 1);
        req.onupgradeneeded = () => {
          req.result.createObjectStore("notes", { keyPath: "id" });
          req.result.createObjectStore("meta");
        };
        req.onsuccess = () => {
          req.result.onversionchange = () => {
            req.result.close();
            opening = undefined;
          };
          resolve(req.result);
        };
        req.onerror = req.onblocked = () => resolve(null);
      } catch {
        resolve(null);
      }
    }));
  }

  async function tx<T>(stores: string[], mode: IDBTransactionMode, fn: (t: IDBTransaction) => IDBRequest<T> | void): Promise<T | undefined> {
    try {
      const d = await open();
      if (!d) return undefined;
      return await new Promise<T | undefined>((resolve, reject) => {
        const t = d.transaction(stores, mode);
        const r = fn(t);
        t.oncomplete = () => resolve(r ? r.result : undefined);
        t.onerror = t.onabort = () => reject(t.error);
      });
    } catch {
      broken = true;
      return undefined;
    }
  }

  return {
    async load() {
      try {
        const d = await open();
        if (d) {
          return await new Promise<KeepCache>((resolve, reject) => {
            const t = d.transaction(["notes", "meta"], "readonly");
            const all = t.objectStore("notes").getAll() as IDBRequest<KeepNote[]>;
            const cur = t.objectStore("meta").get("cursor");
            const lab = t.objectStore("meta").get("labels");
            t.oncomplete = () =>
              resolve({
                notes: all.result ?? [],
                cursor: typeof cur.result === "string" ? cur.result : "0",
                labels: Array.isArray(lab.result) ? (lab.result as LabelInfo[]) : [],
              });
            t.onerror = t.onabort = () => reject(t.error);
          });
        }
      } catch {
        broken = true;
      }
      const cursor = mem.meta.get("cursor");
      return { notes: [...mem.notes.values()], cursor: typeof cursor === "string" ? cursor : "0", labels: (mem.meta.get("labels") as LabelInfo[] | undefined) ?? [] };
    },
    async putNotes(notes) {
      for (const n of notes) mem.notes.set(n.id, n);
      await tx(["notes"], "readwrite", (t) => {
        for (const n of notes) t.objectStore("notes").put(n);
      });
    },
    async removeNotes(ids) {
      for (const id of ids) mem.notes.delete(id);
      await tx(["notes"], "readwrite", (t) => {
        for (const id of ids) t.objectStore("notes").delete(id);
      });
    },
    async setCursor(cursor) {
      mem.meta.set("cursor", cursor);
      await tx(["meta"], "readwrite", (t) => {
        t.objectStore("meta").put(cursor, "cursor");
      });
    },
    async setLabels(labels) {
      mem.meta.set("labels", labels);
      await tx(["meta"], "readwrite", (t) => {
        t.objectStore("meta").put(labels, "labels");
      });
    },
    async clear() {
      mem.notes.clear();
      mem.meta.clear();
      await tx(["notes", "meta"], "readwrite", (t) => {
        t.objectStore("notes").clear();
        t.objectStore("meta").clear();
      });
    },
  };
}

export const keepDb: KeepDb = createKeepDb();
