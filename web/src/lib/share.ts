export interface ShareEntry {
  id: string;
  createdAt: number;
  title: string;
  text: string[];
  url: string[];
  files: File[];
}
export type ShareIntent = "ask" | "library" | "shopping" | "note" | "remind";

function inbox<T>(mode: IDBTransactionMode, action: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open("secretary-share", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("inbox", { keyPath: "id" });
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const db = open.result;
      const tx = db.transaction("inbox", mode);
      const request = action(tx.objectStore("inbox"));
      tx.oncomplete = () => { db.close(); resolve(request.result); };
      tx.onabort = tx.onerror = () => { db.close(); reject(tx.error); };
    };
  });
}
export const listShares = () => inbox("readonly", (s) => s.getAll()) as Promise<ShareEntry[]>;
export const saveShare = (entry: ShareEntry) => inbox("readwrite", (s) => s.put(entry));
export const deleteShare = (id: string) => inbox("readwrite", (s) => s.delete(id));
export function fileShare(files: File[]): ShareEntry {
  return { id: crypto.randomUUID(), createdAt: Date.now(), title: "", text: [], url: [], files };
}

export async function uploadFile(file: File): Promise<File> {
  if (/image\/hei[cf]/i.test(file.type) || /\.hei[cf]$/i.test(file.name)) {
    const { default: convert } = await import("heic2any");
    const result = await convert({ blob: file, toType: "image/jpeg", quality: 0.92 });
    const blob = Array.isArray(result) ? result[0]! : result;
    return new File([blob], file.name.replace(/\.[^.]+$/, "") + ".jpg", { type: "image/jpeg" });
  }
  return file.type ? file : new File([file], file.name, { type: "application/octet-stream" });
}

export async function sendShare(entry: ShareEntry, intent: ShareIntent, note: string): Promise<void> {
  const data = new FormData();
  const fields = { shareId: entry.id, intent, note, lang: navigator.language.slice(0, 2).toLowerCase(), channel: "web", source: "web-share", title: entry.title };
  for (const [key, value] of Object.entries(fields)) data.append(key, value);
  for (const value of entry.text) data.append("text", value);
  for (const value of entry.url) data.append("url", value);
  // Convert sequentially to avoid decoding several large photos in memory together.
  for (const file of entry.files) data.append("file", await uploadFile(file));
  if (!entry.text.some((v) => v.trim()) && !entry.url.some((v) => v.trim()) && !entry.files.length) throw new Error("empty");
  if ([...data.keys()].length > 32 || [...data.values()].reduce((n, v) => n + (typeof v === "string" ? new Blob([v]).size : v.size), 0) > 2 * 1024 ** 3) throw new Error("limits");
  const response = await fetch("/v1/share", { method: "POST", body: data, credentials: "include", headers: { "X-Secretary-CSRF": "1" } });
  if (response.status !== 202) {
    const body = await response.json().catch(() => null);
    throw new Error(typeof body?.error === "string" ? body.error : String(response.status));
  }
}

export function linkHost(value: string): string {
  try { return new URL(value).host || value; } catch { return value; }
}
