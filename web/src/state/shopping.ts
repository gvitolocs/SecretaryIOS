// Shopping v2 state: lists, the current list's tiles, the catalog, and an
// offline outbox of ops.
//
// The current list's tiles are always `confirmed server state + pending ops`,
// computed with the same semantics as the server (lib/shoppingOps.ts), so an
// edit shows at once and survives reloads and refetches until the server has
// acknowledged it. Ops keep their opId across retries, which makes replay
// safe; after a 409 they are rebased on a fresh state with new opIds.

import { batch, createEffect, createMemo, createRoot, createSignal, on } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { toApiError, type ApiError } from "../api/client";
import { shoppingApi } from "../api/shopping";
import { locale, setLocale } from "../i18n";
import { uuid } from "../lib/ids";
import { readString, writeString } from "../lib/storage";
import {
  buildIndex,
  catalogName,
  DEFAULT_ICON,
  EMPTY_CATALOG,
  exactMatch,
  LANGS,
  OTHER_SECTION,
  resolveByName,
  type Catalog,
  type CatalogItem,
  type Lang,
} from "../lib/shoppingCatalog";
import { translate } from "../lib/shoppingI18n";
import {
  addObjectId,
  addRowId,
  addText as addOpText,
  applyOps,
  DEFAULT_LIST_ID,
  displayName,
  isActive,
  opTarget,
  rebaseOps,
  rowObjectId,
  sameRow,
  type ApplyContext,
  type CustomObject,
  type ListState,
  type ListTheme,
  type Op,
  type OpInput,
  type ShopItem,
  type ShopList,
} from "../lib/shoppingOps";
import {
  clearShoppingStorage,
  loadCache,
  loadOutbox,
  saveCache,
  saveOutbox,
  type OutboxEntry,
} from "../lib/shoppingOutbox";
import { markLucinaOnline } from "./lucina";

export const UNDO_MS = 5_000;
const GROUPED_KEY = "secretary.shopping.grouped";
const MAX_CONFLICT_RETRIES = 3;
const RETRY_MIN_MS = 2_000;
const RETRY_MAX_MS = 30_000;

export interface UndoEntry {
  id: number;
  message: string;
  /** Ops that revert the action; null when the action can't be undone. */
  inverse: OpInput[] | null;
  listId: string;
}

interface ShoppingState {
  lang: Lang;
  lists: ShopList[];
  currentListId: string;
  /** Visible rows (needed + recent) of the current list, optimistic. */
  items: ShopItem[];
  revision: number;
  /** True once the current list has any state (server or cached). */
  loaded: boolean;
  loadError: string | null;
  actionError: string | null;
  /** False after a sync failed for lack of network; true after any success. */
  online: boolean;
  pending: number;
  catalog: Catalog;
  /** The owner's custom item objects (searchable next to the catalog). */
  customObjects: CustomObject[];
  grouped: boolean;
  undo: UndoEntry | null;
}

/** The app-wide language (src/i18n: navigator.languages, or the owner's override). */
function initialLang(): Lang {
  const l = locale();
  return (LANGS as readonly string[]).includes(l) ? (l as Lang) : "en";
}

function initialState(): ShoppingState {
  return {
    lang: initialLang(),
    lists: [],
    currentListId: DEFAULT_LIST_ID,
    items: [],
    revision: 0,
    loaded: false,
    loadError: null,
    actionError: null,
    online: true,
    pending: 0,
    catalog: EMPTY_CATALOG,
    customObjects: [],
    grouped: readString("local", GROUPED_KEY) !== "0",
    undo: null,
  };
}

const [shop, setShop] = createStore<ShoppingState>(initialState());
const [syncing, setSyncing] = createSignal(false);
const [refreshing, setRefreshing] = createSignal(false);
const [creatingList, setCreatingList] = createSignal(false);
const [savingListId, setSavingListId] = createSignal<string | null>(null);
const [deletingListId, setDeletingListId] = createSignal<string | null>(null);
export { shop, syncing, refreshing, creatingList, savingListId, deletingListId };

/** A custom object as a search entry next to the catalog items. */
function customAsCatalogItem(o: CustomObject, lang: Lang): CatalogItem {
  const name = o.names[lang] ?? o.names.it ?? o.names.en ?? o.names.da ?? o.id;
  return {
    id: o.id,
    name,
    names: { ...o.names },
    aliases: [...o.aliases],
    section: o.section,
    icon: o.icon ?? DEFAULT_ICON,
    custom: true,
  };
}

/**
 * Search index in the UI language: the catalog plus the owner's custom
 * objects (rebuilt only when the catalog, the objects or the language change).
 */
export const catalogIndex = createRoot(() =>
  createMemo(() =>
    buildIndex(
      { ...shop.catalog, items: [...shop.catalog.items, ...shop.customObjects.map((o) => customAsCatalogItem(o, shop.lang))] },
      shop.lang,
    ),
  ),
);

export function t(key: Parameters<typeof translate>[1], vars?: Record<string, string | number>): string {
  return translate(shop.lang, key, vars);
}

// ---------- Non-reactive sync state ----------

/** Confirmed server state per list. */
const server = new Map<string, ListState>();
let outbox: OutboxEntry[] = [];
let booted = false;
let flushing = false;
let retryTimer: ReturnType<typeof setTimeout> | null = null;
let retryDelay = RETRY_MIN_MS;
let undoTimer: ReturnType<typeof setTimeout> | null = null;
let undoSeq = 0;
let generation = 0;
const fetchSeq = new Map<string, number>();
let lastFetchAt = 0;
let catalogLang: Lang | null = null;

function nowIso(): string {
  return new Date().toISOString();
}

function catalogHint(objectId: string) {
  const item = catalogIndex().byId.get(objectId);
  return item
    ? {
        name: catalogName(item, shop.lang),
        names: item.names,
        kind: item.custom ? ("custom" as const) : ("catalog" as const),
        section: item.section,
        icon: item.icon,
      }
    : undefined;
}

function ctxFor(listId: string): ApplyContext {
  return {
    listId,
    now: nowIso(),
    by: "web",
    newId: uuid,
    catalog: catalogHint,
    resolve: (text) => exactMatch(catalogIndex(), text)?.id,
    lang: shop.lang,
  };
}

/** Remember custom objects seen on rows or listed by the server (newest version wins). */
function mergeObjects(objects: readonly CustomObject[] | undefined): void {
  if (!objects?.length) return;
  const byId = new Map(shop.customObjects.map((o) => [o.id, o]));
  let changed = false;
  for (const o of objects) {
    const known = byId.get(o.id);
    if (known && JSON.stringify(known) === JSON.stringify(o)) continue;
    byId.set(o.id, o);
    changed = true;
  }
  if (changed) setShop("customObjects", [...byId.values()]);
}

function pendingFor(listId: string): Op[] {
  return outbox.filter((e) => e.listId === listId).map((e) => e.op);
}

function sortItems(items: ShopItem[]): ShopItem[] {
  return [...items].sort((a, b) => a.position - b.position || a.name.localeCompare(b.name));
}

/** Recompute the visible rows of the current list from server state + outbox. */
function refreshView(): void {
  const listId = shop.currentListId;
  const base = server.get(listId);
  const lang = shop.lang;
  const items = applyOps(base?.items ?? [], pendingFor(listId), ctxFor(listId))
    .items.filter(isActive)
    // Every view shows the object's name in the UI language.
    .map((i) => ({ ...i, name: displayName(i, lang) }));
  batch(() => {
    setShop("items", reconcile(sortItems(items), { key: "id" }));
    setShop("revision", base?.revision ?? 0);
    setShop("pending", outbox.length);
    if (base) setShop("loaded", true);
  });
}

function persist(): void {
  saveOutbox(outbox);
  saveCache({
    currentListId: shop.currentListId,
    lists: [...shop.lists],
    states: Object.fromEntries(server),
  });
}

function errorText(e: unknown): string {
  const err = toApiError(e);
  if (err.lucinaOffline) return t("errorLucina");
  if (err.isNetwork) return t("errorOffline");
  return err.message || t("errorGeneric");
}

/** Errors after which the same request may succeed later (keep the ops queued). */
function isTransient(err: ApiError): boolean {
  return err.isNetwork || err.status === 429 || err.status === 502 || err.status === 503 || err.status === 504 || err.status >= 500;
}

function scheduleRetry(): void {
  if (retryTimer !== null) return;
  retryTimer = setTimeout(() => {
    retryTimer = null;
    void flushOutbox();
  }, retryDelay);
  retryDelay = Math.min(RETRY_MAX_MS, retryDelay * 2);
}

function storeServerState(listId: string, state: ListState): void {
  const known = server.get(listId);
  // A GET that raced a newer ops response must not roll the list back.
  if (known && state.revision < known.revision) return;
  mergeObjects(state.objects);
  const { objects: _objects, ...rest } = state;
  server.set(listId, { ...rest, items: state.items.filter(isActive) });
}

/**
 * After an ops response: rows the client created with its own itemId may come
 * back under a server id. Point queued ops at the server's row (matched by key).
 */
function remapIds(listId: string, sent: Op[], items: ShopItem[]): void {
  const ids = new Set(items.map((i) => i.id));
  const remap = new Map<string, string>();
  for (const op of sent) {
    if (op.type !== "add") continue;
    const rowId = addRowId(op);
    if (!rowId || ids.has(rowId)) continue;
    const match = items.find((i) => isActive(i) && sameRow(i, addObjectId(op), addOpText(op)));
    if (match) remap.set(rowId, match.id);
  }
  if (remap.size === 0) return;
  outbox = outbox.map((e) => {
    if (e.listId !== listId) return e;
    const target = opTarget(e.op);
    const to = target ? remap.get(target) : undefined;
    if (!to) return e;
    // An add's row id lives in rowId (old outboxes: itemId); other ops name the row in itemId.
    if (e.op.type === "add") return { ...e, op: { ...e.op, rowId: to, ...(e.op.rowId ? {} : { itemId: undefined }) } as Op };
    return { ...e, op: { ...e.op, itemId: to } as Op };
  });
}

/** Send queued ops, one list at a time, until the outbox is empty or the network fails. */
export async function flushOutbox(): Promise<void> {
  if (flushing) return;
  flushing = true;
  const gen = generation;
  setSyncing(true);
  const conflicts = new Map<string, number>();
  try {
    while (outbox.length > 0 && gen === generation) {
      const listId = outbox[0]!.listId;
      const sent = pendingFor(listId);
      try {
        let base = server.get(listId);
        if (!base) {
          base = await shoppingApi.listState(listId, shop.lang);
          if (gen !== generation) return;
          storeServerState(listId, base);
        }
        const res = await shoppingApi.postOps(listId, base.revision, sent, shop.lang);
        if (gen !== generation) return;
        markLucinaOnline();
        const done = new Set(sent.map((o) => o.opId));
        outbox = outbox.filter((e) => !done.has(e.op.opId));
        if (res.items) {
          remapIds(listId, sent, res.items);
          storeServerState(listId, { list: base.list, revision: res.revision, items: res.items, objects: res.objects });
        } else {
          storeServerState(listId, await shoppingApi.listState(listId, shop.lang));
        }
        retryDelay = RETRY_MIN_MS;
        setShop("online", true);
      } catch (e) {
        if (gen !== generation) return;
        const err = toApiError(e);
        if (err.status === 409) {
          const n = (conflicts.get(listId) ?? 0) + 1;
          conflicts.set(listId, n);
          const fresh = await shoppingApi.listState(listId, shop.lang);
          if (gen !== generation) return;
          mergeObjects(fresh.objects);
          server.set(listId, { list: fresh.list, revision: fresh.revision, items: fresh.items.filter(isActive) });
          if (n > MAX_CONFLICT_RETRIES) {
            // Give up on this batch rather than loop: the fresh state wins.
            outbox = outbox.filter((e) => e.listId !== listId);
            setShop("actionError", t("errorGeneric"));
          } else {
            const rebased = rebaseOps(fresh.items, pendingFor(listId), ctxFor(listId));
            const others = outbox.filter((e) => e.listId !== listId);
            outbox = [...others, ...rebased.map((op) => ({ listId, op, queuedAt: Date.now() }))];
          }
        } else if (isTransient(err)) {
          setShop("online", !err.isNetwork);
          scheduleRetry();
          break;
        } else {
          // Rejected for good (400/404/422): drop these ops and show the server's view.
          const drop = new Set(sent.map((o) => o.opId));
          outbox = outbox.filter((e) => !drop.has(e.op.opId));
          setShop("actionError", errorText(err));
          if (err.status === 404) server.delete(listId);
          else void loadList(listId);
        }
      } finally {
        if (gen === generation) {
          persist();
          refreshView();
        }
      }
    }
  } catch (e) {
    // A refetch inside the conflict path failed: keep everything queued.
    if (gen === generation) {
      setShop("online", !toApiError(e).isNetwork);
      scheduleRetry();
    }
  } finally {
    if (gen === generation) {
      flushing = false;
      setSyncing(false);
    }
  }
}

/** Queue ops for a list, show them at once, and start a flush. */
export function enqueue(listId: string, inputs: OpInput[]): Op[] {
  const ops = inputs.map((input) => ({ ...input, opId: uuid() }) as Op);
  outbox = [...outbox, ...ops.map((op) => ({ listId, op, queuedAt: Date.now() }))];
  persist();
  refreshView();
  void flushOutbox();
  return ops;
}

// ---------- Loading ----------

export async function loadList(listId = shop.currentListId): Promise<void> {
  const mine = (fetchSeq.get(listId) ?? 0) + 1;
  fetchSeq.set(listId, mine);
  const gen = generation;
  if (listId === shop.currentListId) setRefreshing(true);
  lastFetchAt = Date.now();
  try {
    const state = await shoppingApi.listState(listId, shop.lang);
    if (gen !== generation || fetchSeq.get(listId) !== mine) return;
    markLucinaOnline();
    storeServerState(listId, state);
    setShop("online", true);
    if (listId === shop.currentListId) setShop("loadError", null);
    persist();
    refreshView();
  } catch (e) {
    if (gen !== generation || fetchSeq.get(listId) !== mine) return;
    if (listId === shop.currentListId) setShop("loadError", errorText(e));
    if (toApiError(e).isNetwork) setShop("online", false);
  } finally {
    if (gen === generation && fetchSeq.get(listId) === mine && listId === shop.currentListId) setRefreshing(false);
  }
}

export async function loadLists(): Promise<void> {
  const gen = generation;
  try {
    const lists = await shoppingApi.lists();
    if (gen !== generation) return;
    setShop("lists", reconcile(lists, { key: "id" }));
    if (lists.length > 0 && !lists.some((l) => l.id === shop.currentListId)) {
      selectList(lists.find((l) => l.id === DEFAULT_LIST_ID)?.id ?? lists[0]!.id);
    }
    persist();
  } catch {
    // The list chooser keeps the cached lists.
  }
}

export async function loadCatalog(lang = shop.lang): Promise<void> {
  if (catalogLang === lang && shop.catalog.items.length > 0) return;
  const gen = generation;
  try {
    const catalog = await shoppingApi.catalog(lang);
    if (gen !== generation || lang !== shop.lang) return;
    catalogLang = lang;
    setShop("catalog", catalog);
    refreshView();
  } catch {
    // Search falls back to free text.
  }
}

/** The owner's custom item objects, for search. */
export async function loadCustomObjects(): Promise<void> {
  const gen = generation;
  try {
    const objects = await shoppingApi.customItems();
    if (gen !== generation) return;
    mergeObjects(objects);
  } catch {
    // Search still has the catalog and the objects seen on lists.
  }
}

/** Start everything (idempotent): cached state, outbox replay, fresh data. */
export function initShopping(): void {
  if (!booted) {
    booted = true;
    const cache = loadCache();
    for (const [id, state] of Object.entries(cache.states)) server.set(id, state);
    outbox = loadOutbox();
    batch(() => {
      if (cache.lists.length) setShop("lists", cache.lists);
      if (cache.currentListId) setShop("currentListId", cache.currentListId);
    });
    refreshView();
  }
  void loadCatalog();
  void loadCustomObjects();
  void loadLists();
  void loadList();
  void flushOutbox();
}

export function refreshShopping(opts: { ifOlderThanMs?: number } = {}): void {
  if (opts.ifOlderThanMs !== undefined && Date.now() - lastFetchAt < opts.ifOlderThanMs) return;
  void loadList();
  void loadLists();
  void flushOutbox();
}

/** Browser came back online: replay at once instead of waiting for the backoff. */
export function onOnline(): void {
  if (retryTimer !== null) clearTimeout(retryTimer);
  retryTimer = null;
  retryDelay = RETRY_MIN_MS;
  void flushOutbox();
  void loadList();
}

// ---------- Settings ----------

/** Language override from the Shopping settings menu: sets the app-wide language. */
export function setLang(lang: Lang): void {
  setLocale(lang);
}

// Follow the app language: strings switch at once, the catalog is refetched in that language.
createRoot(() =>
  createEffect(
    on(
      locale,
      () => {
        const lang = initialLang();
        if (shop.lang === lang) return;
        setShop("lang", lang);
        refreshView();
        if (booted) {
          void loadCatalog(lang);
          void loadList();
        }
      },
      { defer: true },
    ),
  ),
);

export function setGrouped(grouped: boolean): void {
  setShop("grouped", grouped);
  writeString("local", GROUPED_KEY, grouped ? "1" : "0");
}

export function dismissShoppingError(): void {
  setShop("actionError", null);
}

// ---------- Item helpers ----------

type RowRef = Pick<ShopItem, "catalogId" | "name"> & Partial<Pick<ShopItem, "objectId">>;

/** The search entry behind a row (catalog item or custom object): its object id, else its name resolved like the server does. */
function rowCatalogItem(item: RowRef): CatalogItem | undefined {
  const id = item.objectId ?? item.catalogId;
  if (id) return catalogIndex().byId.get(id);
  return resolveByName(catalogIndex(), item.name)?.item;
}

/** Key of a row for matching catalog tiles and search results (`itemKey`). */
export function rowKeyId(item: Pick<ShopItem, "objectId" | "catalogId">): string | null {
  return rowObjectId(item);
}

export function itemIcon(item: Pick<ShopItem, "icon"> & RowRef): string | null {
  if (item.icon) return item.icon;
  const cat = rowCatalogItem(item);
  if (cat) return cat.icon;
  return item.catalogId ? DEFAULT_ICON : null;
}

export function itemSection(item: Pick<ShopItem, "section"> & RowRef): string {
  if (item.section) return item.section;
  return rowCatalogItem(item)?.section ?? OTHER_SECTION;
}

function findActive(objectId: string | null, name: string): ShopItem | undefined {
  return shop.items.find((i) => sameRow(i, objectId, name));
}

/** The add op for a known object; legacy fields keep servers from before item objects working. */
function addOpFor(cat: CatalogItem, name: string, rowId: string): OpInput {
  return cat.custom
    ? { type: "add", itemId: cat.id, rowId, name }
    : { type: "add", itemId: cat.id, rowId, catalogId: cat.id, name };
}

// ---------- Undo ----------

function offerUndo(message: string, inverse: OpInput[] | null): void {
  if (undoTimer !== null) clearTimeout(undoTimer);
  const id = ++undoSeq;
  setShop("undo", { id, message, inverse, listId: shop.currentListId });
  undoTimer = setTimeout(() => {
    undoTimer = null;
    if (shop.undo?.id === id) setShop("undo", null);
  }, UNDO_MS);
}

export function undoLast(): void {
  const entry = shop.undo;
  if (undoTimer !== null) clearTimeout(undoTimer);
  undoTimer = null;
  setShop("undo", null);
  if (entry?.inverse?.length) enqueue(entry.listId, entry.inverse);
}

export function dismissUndo(): void {
  if (undoTimer !== null) clearTimeout(undoTimer);
  undoTimer = null;
  setShop("undo", null);
}

// ---------- Actions ----------

/** Tap on a list tile: to-buy -> bought (recent); recent -> back on the list. */
export function tapItem(id: string): void {
  const item = shop.items.find((i) => i.id === id);
  if (!item) return;
  const name = item.name;
  if (item.status === "needed") {
    enqueue(shop.currentListId, [{ type: "purchase", itemId: id }]);
    offerUndo(t("boughtToast", { name }), [{ type: "need", itemId: id }]);
  } else {
    enqueue(shop.currentListId, [{ type: "need", itemId: id }]);
    offerUndo(t("addedToast", { name }), [{ type: "purchase", itemId: id }]);
  }
}

/** Tap on a catalog (or custom object) search result. Returns the row id it ended up on. */
export function addCatalogItem(cat: CatalogItem, spec?: string): string {
  const name = catalogName(cat, shop.lang);
  const existing = findActive(cat.id, name);
  if (existing) {
    if (existing.status === "needed" && spec === undefined) return existing.id;
    const wasRecent = existing.status === "recent";
    enqueue(shop.currentListId, [{ ...addOpFor(cat, name, existing.id), ...(spec !== undefined ? { spec } : {}) } as OpInput]);
    if (wasRecent) offerUndo(t("addedToast", { name }), [{ type: "purchase", itemId: existing.id }]);
    return existing.id;
  }
  const rowId = uuid();
  enqueue(shop.currentListId, [
    { ...addOpFor(cat, name, rowId), section: cat.section, ...(spec ? { spec } : {}) } as OpInput,
  ]);
  offerUndo(t("addedToast", { name }), [{ type: "remove", itemId: rowId }]);
  return rowId;
}

/**
 * Free text from the search bar: a known object (catalog or custom, any
 * language) when it matches exactly, else new text the server turns into a
 * custom object with names in every language.
 */
export function addText(text: string): string | null {
  const name = text.trim().slice(0, 200);
  if (!name) return null;
  const cat = exactMatch(catalogIndex(), name);
  if (cat) return addCatalogItem(cat);
  const existing = findActive(null, name);
  if (existing) {
    if (existing.status === "recent") tapItem(existing.id);
    return existing.id;
  }
  const rowId = uuid();
  enqueue(shop.currentListId, [{ type: "add", rowId, text: name, name }]);
  offerUndo(t("addedToast", { name }), [{ type: "remove", itemId: rowId }]);
  return rowId;
}

export interface ItemPatch {
  name?: string;
  /** A custom object's names (any of it, en, da). */
  names?: Partial<Record<Lang, string>>;
  /** A custom object's icon (one emoji; "" clears it). */
  icon?: string;
  spec?: string;
  quantity?: number | null;
  unit?: string | null;
  section?: string | null;
}

/**
 * Edits from the details sheet; only changed fields become ops. A custom
 * object's names, section and icon are object edits (`edit_item`, shown on
 * every list); a catalog row keeps per-row `rename` and `set_section`.
 */
export function editItem(id: string, patch: ItemPatch): void {
  const item = shop.items.find((i) => i.id === id);
  if (!item) return;
  const ops: OpInput[] = [];
  const custom = item.kind === "custom" && item.objectId ? item.objectId : null;
  if (custom) {
    const names: Partial<Record<string, string>> = {};
    for (const [lang, value] of Object.entries(patch.names ?? {})) {
      const clean = value?.trim();
      if (clean && clean !== item.names[lang]) names[lang] = clean;
    }
    if (patch.name !== undefined && patch.name.trim() && patch.name.trim() !== item.name && !names[shop.lang]) {
      names[shop.lang] = patch.name.trim();
    }
    const edit: OpInput = { type: "edit_item", itemId: custom };
    if (Object.keys(names).length) edit.names = names;
    if (patch.section !== undefined && patch.section !== null && patch.section !== itemSection(item)) edit.section = patch.section;
    if (patch.icon !== undefined && patch.icon.trim() !== (item.icon ?? "")) edit.icon = patch.icon.trim();
    if (edit.names || edit.section !== undefined || edit.icon !== undefined) ops.push(edit);
  } else if (patch.name !== undefined && patch.name.trim() && patch.name.trim() !== item.name) {
    ops.push({ type: "rename", itemId: id, name: patch.name.trim() });
  }
  if (patch.spec !== undefined && patch.spec.trim() !== item.spec) ops.push({ type: "set_spec", itemId: id, spec: patch.spec.trim() });
  const qty = patch.quantity === undefined ? item.quantity : patch.quantity;
  const unit = patch.unit === undefined ? item.unit : patch.unit || null;
  if (qty !== item.quantity || unit !== item.unit) ops.push({ type: "set_quantity", itemId: id, quantity: qty, unit });
  if (!custom && patch.section !== undefined && patch.section !== itemSection(item)) {
    ops.push({ type: "set_section", itemId: id, section: patch.section });
  }
  if (ops.length) enqueue(shop.currentListId, ops);
}

export function deleteItem(id: string): void {
  const item = shop.items.find((i) => i.id === id);
  if (!item) return;
  const snapshot = { ...item };
  const newId = uuid();
  enqueue(shop.currentListId, [{ type: "remove", itemId: id }]);
  // A removed row never comes back, so undo re-adds it as a new row.
  const objectId = rowObjectId(snapshot);
  offerUndo(t("deletedToast", { name: snapshot.name }), [
    {
      type: "add",
      rowId: newId,
      ...(objectId ? { itemId: objectId } : { text: snapshot.typedName || snapshot.name }),
      ...(snapshot.catalogId ? { catalogId: snapshot.catalogId } : {}),
      name: snapshot.name,
      spec: snapshot.spec,
      quantity: snapshot.quantity,
      unit: snapshot.unit,
      section: snapshot.section,
    },
    ...(snapshot.status === "recent" ? [{ type: "purchase" as const, itemId: newId }] : []),
  ]);
}

export function moveItem(id: string, toListId: string): void {
  const item = shop.items.find((i) => i.id === id);
  if (!item || toListId === shop.currentListId) return;
  enqueue(shop.currentListId, [{ type: "move", itemId: id, toListId }]);
  offerUndo(t("movedToast", { name: item.name }), null);
  // The target list's counts change on the server; refresh them shortly.
  setTimeout(() => void loadLists(), 1_500);
}

/** "Recently used" edit mode: remove the selected rows. */
export function removeItems(ids: readonly string[]): void {
  const present = ids.filter((id) => shop.items.some((i) => i.id === id && i.status === "recent"));
  if (present.length === 0) return;
  enqueue(shop.currentListId, present.map((itemId) => ({ type: "remove" as const, itemId })));
  offerUndo(t("removedToast", { n: present.length }), null);
}

export function clearRecent(): void {
  if (!shop.items.some((i) => i.status === "recent")) return;
  enqueue(shop.currentListId, [{ type: "clear_recent" }]);
  offerUndo(t("clearedToast"), null);
}

// ---------- Lists ----------

export function selectList(id: string): void {
  if (id === shop.currentListId) return;
  dismissUndo();
  batch(() => {
    setShop("currentListId", id);
    setShop("loaded", server.has(id));
    setShop("loadError", null);
  });
  refreshView();
  persist();
  void loadList(id);
}

export async function createList(name: string, theme: ListTheme): Promise<boolean> {
  const clean = name.trim().slice(0, 80);
  if (!clean || creatingList()) return false;
  setCreatingList(true);
  setShop("actionError", null);
  try {
    const list = await shoppingApi.createList(clean, theme);
    setShop("lists", (lists) => [...lists.filter((l) => l.id !== list.id), list]);
    server.set(list.id, { list: { id: list.id, name: list.name, theme: list.theme }, revision: 0, items: [] });
    selectList(list.id);
    return true;
  } catch (e) {
    setShop("actionError", errorText(e));
    return false;
  } finally {
    setCreatingList(false);
  }
}

export async function updateList(id: string, patch: { name?: string; theme?: ListTheme }): Promise<boolean> {
  if (savingListId() !== null) return false;
  setSavingListId(id);
  setShop("actionError", null);
  try {
    const list = await shoppingApi.patchList(id, patch);
    setShop("lists", (l) => l.id === id, { name: list.name, theme: list.theme });
    persist();
    return true;
  } catch (e) {
    setShop("actionError", errorText(e));
    return false;
  } finally {
    setSavingListId(null);
  }
}

export async function removeList(id: string): Promise<boolean> {
  if (id === DEFAULT_LIST_ID || deletingListId() !== null) return false;
  setDeletingListId(id);
  setShop("actionError", null);
  try {
    await shoppingApi.deleteList(id);
    setShop("lists", (lists) => lists.filter((l) => l.id !== id));
    server.delete(id);
    outbox = outbox.filter((e) => e.listId !== id);
    if (shop.currentListId === id) {
      const next = shop.lists.find((l) => l.id === DEFAULT_LIST_ID)?.id ?? shop.lists[0]?.id ?? DEFAULT_LIST_ID;
      selectList(next);
    }
    persist();
    return true;
  } catch (e) {
    setShop("actionError", errorText(e));
    return false;
  } finally {
    setDeletingListId(null);
  }
}

/** Sign-out and tests: forget everything, including the stored outbox and cache. */
export function resetShopping(): void {
  generation++;
  booted = false;
  flushing = false;
  server.clear();
  outbox = [];
  fetchSeq.clear();
  lastFetchAt = 0;
  catalogLang = null;
  retryDelay = RETRY_MIN_MS;
  if (retryTimer !== null) clearTimeout(retryTimer);
  if (undoTimer !== null) clearTimeout(undoTimer);
  retryTimer = null;
  undoTimer = null;
  clearShoppingStorage();
  batch(() => {
    setSyncing(false);
    setRefreshing(false);
    setCreatingList(false);
    setSavingListId(null);
    setDeletingListId(null);
    setShop(reconcile(initialState()));
  });
}

/** Test hook: the queued ops. */
export function outboxSnapshot(): OutboxEntry[] {
  return outbox.map((e) => ({ ...e, op: { ...e.op } as Op }));
}
