// The owner's own background image for Lucina's stage (studio B3). It is kept only in this
// browser (IndexedDB), never uploaded. Every call fails soft: no IndexedDB, no image.

const DB = "secretary-studio";
const STORE = "files";
const KEY = "background-image";
/** Largest side kept: big photos are scaled down before they are stored. */
export const MAX_SIDE = 2048;

function open(): Promise<IDBDatabase | null> {
  return new Promise((resolve) => {
    try {
      if (typeof indexedDB === "undefined") return resolve(null);
      const req = indexedDB.open(DB, 1);
      req.onupgradeneeded = () => req.result.createObjectStore(STORE);
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => resolve(null);
      req.onblocked = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
}

async function run<T>(mode: IDBTransactionMode, fn: (s: IDBObjectStore) => IDBRequest<T>): Promise<T | null> {
  const db = await open();
  if (!db) return null;
  return new Promise((resolve) => {
    try {
      const tx = db.transaction(STORE, mode);
      const req = fn(tx.objectStore(STORE));
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => resolve(null);
      tx.oncomplete = () => db.close();
    } catch {
      resolve(null);
    }
  });
}

export async function loadBackgroundImage(): Promise<Blob | null> {
  const v = await run<unknown>("readonly", (s) => s.get(KEY));
  return v instanceof Blob ? v : null;
}

export async function saveBackgroundImage(blob: Blob): Promise<void> {
  await run("readwrite", (s) => s.put(blob, KEY));
}

export async function clearBackgroundImage(): Promise<void> {
  await run("readwrite", (s) => s.delete(KEY));
}

/** Scales a picked file down to MAX_SIDE (JPEG) so it stays small; falls back to the file. */
export async function shrinkImage(file: Blob): Promise<Blob> {
  try {
    if (typeof createImageBitmap !== "function") return file;
    const bmp = await createImageBitmap(file);
    const k = Math.min(1, MAX_SIDE / Math.max(bmp.width, bmp.height));
    if (k >= 1 && file.size < 3_000_000) return file;
    const c = document.createElement("canvas");
    c.width = Math.round(bmp.width * k);
    c.height = Math.round(bmp.height * k);
    c.getContext("2d")?.drawImage(bmp, 0, 0, c.width, c.height);
    return await new Promise((res) => c.toBlob((b) => res(b ?? file), "image/jpeg", 0.88));
  } catch {
    return file;
  }
}
