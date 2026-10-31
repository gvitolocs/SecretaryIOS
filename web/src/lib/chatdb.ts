import type { TimelineItem } from "../api/timeline";

export function conversationKey(item: TimelineItem): string {
  if (item.meta?.via === "mcp") return `agent:${(item.meta.caller ?? "").toLowerCase().replace(/[^a-z0-9_-]/g, "") || "mcp"}`;
  if (item.channel === "web" || item.channel === "ios") return item.meta?.device ? `device:${item.meta.device}` : `legacy:${item.channel}`;
  return ["telegram", "discord", "alexa"].includes(item.channel) ? item.channel : "lucina";
}
export function isMine(item: TimelineItem, device?: string | null): boolean {
  const key = conversationKey(item);
  return key === "legacy:web" || key === "lucina" || (!!device && key === `device:${device}`);
}
export type Scope = "mine" | `conversation:${string}`;
const memory = new Map<string, TimelineItem>();
const states = new Map<string, unknown>();
let broken = false;
let opening: Promise<IDBDatabase | null> | undefined;
function db(): Promise<IDBDatabase | null> {
  if (broken) return Promise.resolve(null);
  return opening ??= new Promise(resolve => {
    try {
      const request = indexedDB.open("secretary-chat", 1);
      request.onupgradeneeded = () => {
        const d = request.result;
        const items = d.createObjectStore("items", { keyPath: "id" });
        items.createIndex("conversation", ["key", "order"]);
        items.createIndex("order", "order");
        d.createObjectStore("state");
      };
      request.onsuccess = () => { request.result.onversionchange = () => { request.result.close(); opening = undefined; }; resolve(request.result); };
      request.onerror = request.onblocked = () => resolve(null);
    } catch { resolve(null); }
  });
}
async function run<T>(store: string, mode: IDBTransactionMode, action: (s: IDBObjectStore) => IDBRequest<T>): Promise<T | undefined> {
  try {
    const d = await db();
    if (!d) return;
    return await new Promise<T>((resolve, reject) => {
      const tx = d.transaction(store, mode);
      const r = action(tx.objectStore(store));
      tx.oncomplete = () => resolve(r.result);
      tx.onerror = tx.onabort = () => reject(tx.error);
    });
  } catch { broken = true; return; }
}
const order = (id: string) => /^\d+$/.test(id) ? id.padStart(40, "0") : id;
const newest = (a: TimelineItem, b: TimelineItem) => order(b.id).localeCompare(order(a.id));
export async function putItems(items: TimelineItem[]): Promise<void> {
  for (const item of items) memory.set(item.id, item);
  const removed = [...memory.values()].sort(newest).slice(5000);
  for (const item of removed) memory.delete(item.id);
  try {
    const d = await db();
    if (!d) return;
    await new Promise<void>((resolve, reject) => {
      const tx = d.transaction("items", "readwrite"), store = tx.objectStore("items");
      for (const item of items) store.put({ id: item.id, key: conversationKey(item), order: order(item.id), item });
      const count = store.count();
      count.onsuccess = () => {
        let remaining = count.result - 5000;
        if (remaining <= 0) return;
        const cursor = store.index("order").openCursor();
        cursor.onsuccess = () => { if (cursor.result && remaining-- > 0) { cursor.result.delete(); cursor.result.continue(); } };
      };
      tx.oncomplete = () => resolve();
      tx.onerror = tx.onabort = () => reject(tx.error);
    });
  } catch { broken = true; /* Memory retains the received items when storage is unavailable. */ }
}
export async function readItems(scope: Scope, device?: string | null, before?: string | null, limit = 50): Promise<TimelineItem[]> {
  const matches = (i: TimelineItem) => (scope === "mine" ? isMine(i, device) : conversationKey(i) === scope.slice(13)) && (!before || order(i.id) < order(before));
  try {
    const d = await db();
    if (d) return await new Promise<TimelineItem[]>((resolve, reject) => {
      const tx = d.transaction("items", "readonly"), store = tx.objectStore("items");
      const conversation = scope !== "mine";
      const range = conversation
        ? IDBKeyRange.bound([scope.slice(13), ""], [scope.slice(13), before ? order(before) : "\uffff"], false, !!before)
        : before ? IDBKeyRange.upperBound(order(before), true) : undefined;
      const cursor = store.index(conversation ? "conversation" : "order").openCursor(range, "prev");
      const result: TimelineItem[] = [];
      cursor.onsuccess = () => {
        const row = cursor.result;
        if (!row || result.length >= limit) return;
        if (matches(row.value.item)) { result.push(row.value.item); memory.set(row.value.item.id, row.value.item); }
        if (result.length < limit) row.continue();
      };
      tx.oncomplete = () => resolve(result);
      tx.onerror = tx.onabort = () => reject(tx.error);
    });
  } catch { broken = true; /* Fall back to the in-memory mirror. */ }
  return [...memory.values()].filter(matches).sort(newest).slice(0, limit);
}
export async function resetScope(scope: Scope, device?: string | null): Promise<void> {
  const rows = await readItems(scope, device, null, 5000);
  for (const i of rows) memory.delete(i.id);
  try {
    const d = await db();
    if (!d) return;
    await new Promise<void>((resolve, reject) => {
      const tx = d.transaction("items", "readwrite");
      for (const i of rows) tx.objectStore("items").delete(i.id);
      tx.oncomplete = () => resolve(); tx.onerror = tx.onabort = () => reject(tx.error);
    });
  } catch { broken = true; /* Memory has already been reset. */ }
}
export async function getState<T>(key: string): Promise<T | undefined> {
  const value = await run<T>("state", "readonly", s => s.get(key));
  return value ?? states.get(key) as T | undefined;
}
export async function setState(key: string, value: unknown): Promise<void> {
  states.set(key, value);
  await run("state", "readwrite", s => s.put(value, key));
}
export async function clearChatDB(): Promise<void> {
  memory.clear(); states.clear();
  try {
    const d = await db();
    if (!d) return;
    await new Promise<void>((resolve, reject) => {
      const tx = d.transaction(["items", "state"], "readwrite");
      tx.objectStore("items").clear(); tx.objectStore("state").clear();
      tx.oncomplete = () => resolve(); tx.onerror = tx.onabort = () => reject(tx.error);
    });
  } catch { broken = true; }
}
