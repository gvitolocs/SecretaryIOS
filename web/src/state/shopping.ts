import { createSignal } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import { toApiError } from "../api/client";
import { api } from "../api/secretary";
import { uuid } from "../lib/ids";
import { nextStatus, normalizeItem, normalizeList, togglePayload, type ShoppingItem } from "../lib/shopping";
import { markLucinaOnline } from "./lucina";

export interface ShoppingRow extends ShoppingItem {
  /** A toggle or add for this row is in flight. */
  pending: boolean;
  /** Optimistic row that the server has not confirmed yet. */
  temp: boolean;
}

interface ShoppingState {
  items: ShoppingRow[];
  loaded: boolean;
  /** Message for the last failed load. */
  loadError: string | null;
  /** Message for the last failed add/toggle (dismissible). */
  actionError: string | null;
}

const [shop, setShop] = createStore<ShoppingState>({ items: [], loaded: false, loadError: null, actionError: null });
const [refreshing, setRefreshing] = createSignal(false);
export { shop, refreshing };

/** Desired checked state of rows whose PATCH is in flight. Survives refetches. */
const pendingToggles = new Map<string, boolean>();
let fetchSeq = 0;
let lastFetchAt = 0;

function toRow(item: ShoppingItem, temp = false): ShoppingRow {
  return { ...item, pending: temp, temp };
}

function errorText(e: unknown, fallback: string): string {
  const err = toApiError(e);
  if (err.lucinaOffline) return `${fallback} Lucina is offline.`;
  if (err.isNetwork) return `${fallback} Can't reach the server.`;
  return `${fallback} ${err.message}`;
}

export async function refreshShopping(opts: { ifOlderThanMs?: number } = {}): Promise<void> {
  if (opts.ifOlderThanMs !== undefined && Date.now() - lastFetchAt < opts.ifOlderThanMs) return;
  const mine = ++fetchSeq;
  lastFetchAt = Date.now();
  setRefreshing(true);
  try {
    const data = await api.listShopping();
    if (mine !== fetchSeq) return;
    markLucinaOnline();
    const fresh = normalizeList(data).map((item) => {
      const want = pendingToggles.get(item.id);
      if (want === undefined) return toRow(item);
      const status = item.toggleField === "status" ? nextStatus(item.status, want) : item.status;
      return { ...toRow(item), checked: want, status, pending: true };
    });
    const temps = shop.items.filter((i) => i.temp);
    setShop("items", reconcile([...fresh, ...temps], { key: "id" }));
    setShop({ loaded: true, loadError: null });
  } catch (e) {
    if (mine !== fetchSeq) return;
    setShop("loadError", errorText(e, "Couldn't load the list."));
  } finally {
    if (mine === fetchSeq) setRefreshing(false);
  }
}

export async function addItem(rawName: string): Promise<boolean> {
  const name = rawName.trim();
  if (!name) return false;
  const tempId = `temp-${uuid()}`;
  const temp = toRow(
    { id: tempId, hasId: false, name, quantity: null, checked: false, toggleField: "checked", status: null, raw: { rawName: name } },
    true,
  );
  setShop("items", shop.items.length, temp);
  setShop("actionError", null);
  try {
    const created = await api.addShopping(name);
    markLucinaOnline();
    const item = normalizeItem(created, 0);
    setShop(
      "items",
      produce((items) => {
        const idx = items.findIndex((i) => i.id === tempId);
        if (idx < 0) return;
        if (item && item.hasId && !items.some((i) => i.id === item.id)) items[idx] = toRow(item);
        else items.splice(idx, 1);
      }),
    );
    void refreshShopping();
    return true;
  } catch (e) {
    setShop("items", (items) => items.filter((i) => i.id !== tempId));
    setShop("actionError", errorText(e, `Couldn't add "${name}".`));
    return false;
  }
}

export async function toggleItem(id: string): Promise<void> {
  const row = shop.items.find((i) => i.id === id);
  if (!row || row.temp || !row.hasId || pendingToggles.has(id)) return;
  // Snapshot before awaiting: the store row may change underneath us.
  const prevChecked = row.checked;
  const prevStatus = row.status;
  const next = !prevChecked;
  const payload = togglePayload({ toggleField: row.toggleField, status: prevStatus }, next);
  const name = row.name;

  pendingToggles.set(id, next);
  setShop("items", (i) => i.id === id, {
    checked: next,
    pending: true,
    status: typeof payload.status === "string" ? payload.status : prevStatus,
  });
  try {
    await api.patchShopping(id, payload);
    markLucinaOnline();
    setShop("items", (i) => i.id === id, { pending: false });
  } catch (e) {
    setShop("items", (i) => i.id === id, { checked: prevChecked, status: prevStatus, pending: false });
    setShop("actionError", errorText(e, `Couldn't update "${name}".`));
  } finally {
    pendingToggles.delete(id);
  }
}

export function dismissShoppingError(): void {
  setShop("actionError", null);
}

export function resetShopping(): void {
  fetchSeq++;
  lastFetchAt = 0;
  pendingToggles.clear();
  setRefreshing(false);
  setShop({ items: [], loaded: false, loadError: null, actionError: null });
}
