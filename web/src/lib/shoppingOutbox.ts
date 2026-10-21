// Persistence for the shopping outbox and the last known lists, so edits made
// offline survive a reload and are replayed with the same op ids.
// Web Storage may be missing or throw; the storage helpers never throw, and a
// failed write only costs durability, never the in-memory state.

import { readJSON, removeKey, writeJSON } from "./storage";
import { OP_TYPES, parseLists, parseListState, type ListState, type Op, type ShopList } from "./shoppingOps";

export const OUTBOX_KEY = "secretary.shopping.outbox.v1";
export const CACHE_KEY = "secretary.shopping.cache.v1";

export interface OutboxEntry {
  listId: string;
  op: Op;
  queuedAt: number;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isOp(v: unknown): v is Op {
  return isRecord(v) && typeof v.opId === "string" && v.opId !== "" && OP_TYPES.has(v.type as Op["type"]);
}

export function loadOutbox(): OutboxEntry[] {
  const raw = readJSON("local", OUTBOX_KEY);
  if (!Array.isArray(raw)) return [];
  const out: OutboxEntry[] = [];
  const ids = new Set<string>();
  for (const e of raw) {
    if (!isRecord(e) || typeof e.listId !== "string" || !isOp(e.op) || ids.has(e.op.opId)) continue;
    ids.add(e.op.opId);
    out.push({ listId: e.listId, op: e.op, queuedAt: typeof e.queuedAt === "number" ? e.queuedAt : 0 });
  }
  return out;
}

export function saveOutbox(entries: readonly OutboxEntry[]): void {
  if (entries.length === 0) removeKey("local", OUTBOX_KEY);
  else writeJSON("local", OUTBOX_KEY, entries);
}

export interface ShoppingCache {
  currentListId: string | null;
  lists: ShopList[];
  states: Record<string, ListState>;
}

export function loadCache(): ShoppingCache {
  const raw = readJSON("local", CACHE_KEY);
  const empty: ShoppingCache = { currentListId: null, lists: [], states: {} };
  if (!isRecord(raw)) return empty;
  const states: Record<string, ListState> = {};
  if (isRecord(raw.states)) {
    for (const [id, s] of Object.entries(raw.states)) states[id] = parseListState(s, id);
  }
  return {
    currentListId: typeof raw.currentListId === "string" ? raw.currentListId : null,
    lists: parseLists(raw.lists),
    states,
  };
}

export function saveCache(cache: ShoppingCache): void {
  writeJSON("local", CACHE_KEY, cache);
}

export function clearShoppingStorage(): void {
  removeKey("local", OUTBOX_KEY);
  removeKey("local", CACHE_KEY);
}
