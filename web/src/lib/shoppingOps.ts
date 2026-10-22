// Shopping v2 items and ops (docs/shopping-v2.md). `applyOps` is the client's
// copy of the server semantics, used for optimistic UI and for rebasing the
// outbox: op-id dedup, one active row per (list, item object), `add` revives a
// recent row, and a removed row never comes back.
//
// Every row points to an item object (a catalog item or a custom object, with
// names in it/en/da). A row added offline whose text matched nothing locally
// has no object id until the server answers; it is matched by its names.

import { fold } from "./shoppingCatalog";

export type ItemStatus = "needed" | "recent" | "removed";

export const LIST_THEMES = ["home", "grocery", "work", "holiday", "garden", "hardware", "baby", "other"] as const;
export type ListTheme = (typeof LIST_THEMES)[number];

export const THEME_ICON: Record<ListTheme, string> = {
  home: "🏠",
  grocery: "🛒",
  work: "💼",
  holiday: "🏖️",
  garden: "🌱",
  hardware: "🔧",
  baby: "🍼",
  other: "📝",
};

export const DEFAULT_LIST_ID = "default";

export type ObjectKind = "catalog" | "custom";

export interface ShopItem {
  id: string;
  /** The item object's id (`item.id`): a catalog id or `u_<ULID>`; null until the server named it. */
  objectId: string | null;
  kind: ObjectKind | null;
  /** The object's names by language (it, en, da). */
  names: Partial<Record<string, string>>;
  catalogId: string | null;
  /** The server's name (in the language it was asked for); the views show `displayName`. */
  name: string;
  /** The text as typed. */
  typedName: string;
  needsTranslation: boolean;
  spec: string;
  quantity: number | null;
  unit: string | null;
  section: string | null;
  icon: string | null;
  status: ItemStatus;
  position: number;
  updatedAt: string;
  updatedBy: string;
}

export interface ShopList {
  id: string;
  name: string;
  theme: ListTheme;
  needed: number;
  recent: number;
  updatedAt: string | null;
}

export interface ListState {
  list: { id: string; name: string; theme: ListTheme };
  revision: number;
  items: ShopItem[];
  /** Custom item objects seen on the rows (not cached). */
  objects?: CustomObject[];
}

interface OpBase {
  opId: string;
}
export type AddOp = OpBase & {
  type: "add";
  /** The item object to add (catalog id or custom id). Outboxes from before item
   * objects used it for a client row id (a UUID); see `addRowId`. */
  itemId?: string;
  /** Client-chosen id for the optimistic row (local only; the server picks its own). */
  rowId?: string;
  /** Typed text, resolved by the server in any language. */
  text?: string;
  /** Legacy: the catalog id. */
  catalogId?: string;
  /** Legacy: the typed text. */
  name?: string;
  spec?: string;
  quantity?: number | null;
  unit?: string | null;
  section?: string | null;
};
export type EditItemOp = OpBase & {
  type: "edit_item";
  /** The custom object's id. */
  itemId: string;
  names?: Partial<Record<string, string>>;
  section?: string;
  /** One emoji; "" clears it. */
  icon?: string;
};
export type Op =
  | AddOp
  | EditItemOp
  | (OpBase & { type: "need" | "purchase" | "remove"; itemId: string })
  | (OpBase & { type: "set_spec"; itemId: string; spec: string })
  | (OpBase & { type: "set_quantity"; itemId: string; quantity: number | null; unit?: string | null })
  | (OpBase & { type: "set_section"; itemId: string; section: string | null })
  | (OpBase & { type: "rename"; itemId: string; name: string })
  | (OpBase & { type: "move"; itemId: string; toListId: string })
  | (OpBase & { type: "clear_recent" })
  | (OpBase & { type: "reorder"; itemId: string; position: number });

/** An op without its id, as the UI produces it. */
type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;
export type OpInput = DistributiveOmit<Op, "opId">;

export const OP_TYPES = new Set<Op["type"]>([
  "add",
  "need",
  "purchase",
  "remove",
  "set_spec",
  "set_quantity",
  "set_section",
  "rename",
  "move",
  "clear_recent",
  "reorder",
  "edit_item",
]);

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** The optimistic row id of an add (old outboxes kept it in `itemId`). */
export function addRowId(op: AddOp): string | undefined {
  return op.rowId ?? (op.itemId && UUID_RE.test(op.itemId) ? op.itemId : undefined);
}

/** The object id an add names: `itemId` (when not a row UUID), else the legacy `catalogId`. */
export function addObjectId(op: AddOp): string | undefined {
  return (op.itemId && !UUID_RE.test(op.itemId) ? op.itemId : undefined) ?? op.catalogId;
}

/** The typed text of an add. */
export function addText(op: AddOp): string {
  return (op.text ?? op.name ?? "").trim();
}

/** The identity of a row's object: its object id, else its catalog id. */
export function rowObjectId(item: Pick<ShopItem, "objectId" | "catalogId">): string | null {
  return item.objectId ?? item.catalogId;
}

/** The name to show: the object's name in `lang`, else the server's name. */
export function displayName(item: Pick<ShopItem, "names" | "name">, lang: string): string {
  return item.names[lang] ?? item.name;
}

export interface CatalogHint {
  name: string;
  names?: Partial<Record<string, string>>;
  kind?: ObjectKind;
  section: string | null;
  icon: string;
}

export interface ApplyContext {
  listId: string;
  now: string;
  by: string;
  newId: () => string;
  /** Op ids already applied (mutated: applied ids are added). */
  seen?: Set<string>;
  /** The object behind an id (catalog items and known custom objects). */
  catalog?: (objectId: string) => CatalogHint | undefined;
  /** Typed text to a known object id (any language), when it matches one. */
  resolve?: (text: string) => string | undefined;
  /** The UI language (a custom rename edits this language's name). */
  lang?: string;
}

export interface ApplyResult {
  items: ShopItem[];
  applied: string[];
  duplicates: string[];
}

const MAX_TEXT = 200;

function clip(text: string): string {
  return text.trim().slice(0, MAX_TEXT);
}

function validQuantity(q: unknown): number | null {
  return typeof q === "number" && Number.isFinite(q) && q > 0 ? q : null;
}

export function isActive(item: ShopItem): boolean {
  return item.status !== "removed";
}

type RowKeyFields = Pick<ShopItem, "objectId" | "catalogId" | "name"> & Partial<Pick<ShopItem, "names" | "typedName">>;

/**
 * One active row per (list, item object): a row matches when the object ids
 * agree; a row or text with no object yet matches by folded name in any
 * language ("latte" finds the row named "Milk" once its names are known).
 */
export function sameRow(item: RowKeyFields, objectId: string | null | undefined, name: string): boolean {
  const own = rowObjectId(item);
  if (objectId && own === objectId) return true;
  if (objectId && own) return false;
  const q = fold(name);
  if (!q) return false;
  const names = [item.name, item.typedName ?? "", ...Object.values(item.names ?? {})];
  return names.some((n) => typeof n === "string" && n !== "" && fold(n) === q);
}

function nextPosition(items: ShopItem[]): number {
  let max = 0;
  for (const i of items) if (i.status === "needed" && i.position > max) max = i.position;
  return max + 1;
}

/** Returns true when the op changed something. */
function applyOne(items: ShopItem[], op: Op, ctx: ApplyContext): boolean {
  const touch = (item: ShopItem) => {
    item.updatedAt = ctx.now;
    item.updatedBy = ctx.by;
  };
  const active = (id: string) => items.find((i) => i.id === id && isActive(i));

  switch (op.type) {
    case "add": {
      const text = clip(addText(op));
      const objectId = addObjectId(op) ?? (text ? ctx.resolve?.(text) : undefined);
      const hint = objectId ? ctx.catalog?.(objectId) : undefined;
      const name = hint?.name ?? text;
      if (!name && !objectId) return false;
      const rowId = addRowId(op);
      const existing =
        (rowId ? active(rowId) : undefined) ?? items.find((i) => isActive(i) && sameRow(i, objectId, text || name));
      if (existing) {
        if (existing.status === "recent") {
          existing.position = nextPosition(items);
          existing.status = "needed";
        }
        if (op.spec !== undefined) existing.spec = clip(op.spec);
        if (op.quantity !== undefined) existing.quantity = validQuantity(op.quantity);
        if (op.unit !== undefined) existing.unit = op.unit ? clip(op.unit) : null;
        if (op.section !== undefined) existing.section = op.section;
        touch(existing);
        return true;
      }
      // A removed row keeps its id forever, so a reused id gets a fresh one.
      const id = rowId && !items.some((i) => i.id === rowId) ? rowId : ctx.newId();
      const kind = hint?.kind ?? (objectId && !objectId.startsWith("u_") ? "catalog" : objectId ? "custom" : null);
      items.push({
        id,
        objectId: objectId ?? null,
        kind,
        names: hint?.names ? { ...hint.names } : {},
        catalogId: kind === "catalog" ? (objectId ?? null) : null,
        name: name || objectId || "",
        typedName: text || name,
        needsTranslation: false,
        spec: op.spec ? clip(op.spec) : "",
        quantity: validQuantity(op.quantity),
        unit: op.unit ? clip(op.unit) : null,
        section: op.section ?? hint?.section ?? null,
        icon: hint?.icon ?? null,
        status: "needed",
        position: nextPosition(items),
        updatedAt: ctx.now,
        updatedBy: ctx.by,
      });
      return true;
    }
    case "edit_item": {
      let changed = false;
      for (const item of items) {
        if (!isActive(item) || rowObjectId(item) !== op.itemId) continue;
        if (op.names) {
          const names: Partial<Record<string, string>> = { ...item.names };
          for (const [lang, value] of Object.entries(op.names)) if (value?.trim()) names[lang] = clip(value);
          item.names = names;
          if (ctx.lang && names[ctx.lang]) item.name = names[ctx.lang]!;
          item.needsTranslation = false;
        }
        if (op.section !== undefined) item.section = op.section;
        if (op.icon !== undefined) item.icon = op.icon || null;
        touch(item);
        changed = true;
      }
      return changed;
    }
    case "need": {
      const item = active(op.itemId);
      if (!item || item.status === "needed") return false;
      item.position = nextPosition(items);
      item.status = "needed";
      touch(item);
      return true;
    }
    case "purchase": {
      const item = active(op.itemId);
      if (!item || item.status === "recent") return false;
      item.status = "recent";
      touch(item);
      return true;
    }
    case "remove":
    case "move": {
      const item = active(op.itemId);
      if (!item) return false;
      if (op.type === "move" && op.toListId === ctx.listId) return false;
      item.status = "removed";
      touch(item);
      return true;
    }
    case "set_spec": {
      const item = active(op.itemId);
      if (!item) return false;
      item.spec = clip(op.spec);
      touch(item);
      return true;
    }
    case "set_quantity": {
      const item = active(op.itemId);
      if (!item) return false;
      item.quantity = validQuantity(op.quantity);
      if (op.unit !== undefined) item.unit = op.unit ? clip(op.unit) : null;
      touch(item);
      return true;
    }
    case "set_section": {
      const item = active(op.itemId);
      if (!item) return false;
      item.section = op.section;
      touch(item);
      return true;
    }
    case "rename": {
      const item = active(op.itemId);
      const name = clip(op.name);
      if (!item || !name) return false;
      const target = ctx.resolve?.(name);
      if (target && target !== rowObjectId(item)) {
        // Another object's name: the row now shows that object.
        const hint = ctx.catalog?.(target);
        item.objectId = target;
        item.kind = hint?.kind ?? null;
        item.catalogId = hint?.kind === "catalog" ? target : null;
        item.names = hint?.names ? { ...hint.names } : {};
        item.name = hint?.name ?? name;
        item.typedName = name;
        if (hint) item.icon = hint.icon;
      } else if (item.kind === "custom" && ctx.lang) {
        item.names = { ...item.names, [ctx.lang]: name };
        item.name = name;
      } else if (!target) {
        // A catalog row renamed to new text becomes a new custom object (named by the server).
        item.objectId = null;
        item.kind = null;
        item.catalogId = null;
        item.names = {};
        item.name = name;
        item.typedName = name;
      }
      touch(item);
      return true;
    }
    case "reorder": {
      const item = active(op.itemId);
      if (!item || !Number.isFinite(op.position)) return false;
      item.position = op.position;
      touch(item);
      return true;
    }
    case "clear_recent": {
      let changed = false;
      for (const item of items) {
        if (item.status !== "recent") continue;
        item.status = "removed";
        touch(item);
        changed = true;
      }
      return changed;
    }
  }
}

/** Apply ops in order on a copy of `items`. Ops whose id was seen are reported as duplicates. */
export function applyOps(items: readonly ShopItem[], ops: readonly Op[], ctx: ApplyContext): ApplyResult {
  const out = items.map((i) => ({ ...i }));
  const seen = ctx.seen ?? new Set<string>();
  const applied: string[] = [];
  const duplicates: string[] = [];
  for (const op of ops) {
    if (seen.has(op.opId)) {
      duplicates.push(op.opId);
      continue;
    }
    seen.add(op.opId);
    applyOne(out, op, ctx);
    applied.push(op.opId);
  }
  return { items: out, applied, duplicates };
}

/** The item id an op targets, if it targets one. */
export function opTarget(op: Op): string | null {
  if (op.type === "clear_recent" || op.type === "edit_item") return null;
  if (op.type === "add") return addRowId(op) ?? null;
  return op.itemId;
}

function signature(items: ShopItem[]): string {
  return JSON.stringify(items.map(({ updatedAt: _a, updatedBy: _b, ...rest }) => rest));
}

/**
 * Rebase pending ops on a fresh server state after a 409: every op gets a new
 * id, ops on rows that no longer exist (or were removed) are dropped, and so
 * are ops that would change nothing any more.
 */
export function rebaseOps(fresh: readonly ShopItem[], pending: readonly Op[], ctx: Omit<ApplyContext, "seen">): Op[] {
  let items = fresh.map((i) => ({ ...i }));
  const out: Op[] = [];
  for (const op of pending) {
    const target = opTarget(op);
    if (op.type !== "add" && target !== null && !items.some((i) => i.id === target && isActive(i))) continue;
    const next = { ...op, opId: ctx.newId() } as Op;
    const before = signature(items);
    const res = applyOps(items, [next], ctx);
    if (signature(res.items) === before) continue;
    items = res.items;
    out.push(next);
  }
  return out;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim() ? v.trim() : null;
}

export function parseTheme(v: unknown): ListTheme {
  return (LIST_THEMES as readonly string[]).includes(v as string) ? (v as ListTheme) : "other";
}

function parseNames(v: unknown): Partial<Record<string, string>> {
  const out: Partial<Record<string, string>> = {};
  if (!isRecord(v)) return out;
  for (const [lang, name] of Object.entries(v)) {
    const s = str(name);
    if (s) out[lang] = s;
  }
  return out;
}

/** A row from the server (`item` object, `itemId`) or from the local cache (`objectId`, `names`). */
export function parseItem(raw: unknown, index = 0): ShopItem | null {
  if (!isRecord(raw)) return null;
  const id = str(raw.id) ?? (typeof raw.id === "number" ? String(raw.id) : null);
  const object = isRecord(raw.item) ? raw.item : null;
  const names = parseNames(raw.names ?? object?.names);
  const name = str(raw.name) ?? Object.values(names)[0] ?? null;
  if (!id || !name) return null;
  const status: ItemStatus = raw.status === "recent" || raw.status === "removed" ? raw.status : "needed";
  const kindRaw = raw.kind ?? object?.kind;
  const catalogId = str(raw.catalogId);
  const objectId = str(raw.objectId) ?? str(raw.itemId) ?? str(object?.id) ?? catalogId;
  return {
    id,
    objectId,
    kind: kindRaw === "catalog" || kindRaw === "custom" ? kindRaw : catalogId ? "catalog" : null,
    names,
    catalogId,
    name,
    typedName: str(raw.typedName) ?? name,
    needsTranslation: (raw.needsTranslation ?? object?.needsTranslation) === true,
    spec: typeof raw.spec === "string" ? raw.spec : "",
    quantity: validQuantity(raw.quantity),
    unit: str(raw.unit),
    section: str(raw.section),
    icon: str(raw.icon),
    status,
    position: typeof raw.position === "number" && Number.isFinite(raw.position) ? raw.position : index,
    updatedAt: str(raw.updatedAt) ?? "",
    updatedBy: str(raw.updatedBy) ?? "",
  };
}

export function parseItems(raw: unknown): ShopItem[] {
  if (!Array.isArray(raw)) return [];
  const out: ShopItem[] = [];
  raw.forEach((r, i) => {
    const item = parseItem(r, i);
    if (item) out.push(item);
  });
  return out;
}

export function parseList(raw: unknown): ShopList | null {
  if (!isRecord(raw)) return null;
  const id = str(raw.id);
  if (!id) return null;
  const count = (v: unknown) => (typeof v === "number" && Number.isFinite(v) && v >= 0 ? Math.floor(v) : 0);
  return {
    id,
    name: str(raw.name) ?? id,
    theme: parseTheme(raw.theme),
    needed: count(raw.needed),
    recent: count(raw.recent),
    updatedAt: str(raw.updatedAt),
  };
}

export function parseLists(raw: unknown): ShopList[] {
  const arr = Array.isArray(raw) ? raw : isRecord(raw) && Array.isArray(raw.lists) ? raw.lists : [];
  return arr.map(parseList).filter((l): l is ShopList => l !== null);
}

export function parseListState(raw: unknown, fallbackId: string): ListState {
  const r = isRecord(raw) ? raw : {};
  const list = parseList(r.list) ?? { id: fallbackId, name: fallbackId, theme: "other" as ListTheme };
  return {
    list: { id: list.id, name: list.name, theme: list.theme },
    revision: typeof r.revision === "number" && Number.isFinite(r.revision) ? r.revision : 0,
    items: parseItems(r.items),
    objects: objectsOfRows(r.items),
  };
}

export interface OpsResponse {
  revision: number;
  applied: string[];
  duplicates: string[];
  rejected: Array<{ opId: string; error: string }>;
  items: ShopItem[] | null;
  /** Custom item objects seen on the returned rows. */
  objects: CustomObject[];
}

/** A custom item object (`GET /v1/shopping/items`, or a row's `item`). */
export interface CustomObject {
  id: string;
  names: Partial<Record<string, string>>;
  aliases: string[];
  section: string | null;
  icon: string | null;
  needsTranslation: boolean;
}

export function parseCustomObject(raw: unknown): CustomObject | null {
  if (!isRecord(raw) || raw.kind !== "custom") return null;
  const id = str(raw.id);
  const names = parseNames(raw.names);
  if (!id || Object.keys(names).length === 0) return null;
  return {
    id,
    names,
    aliases: Array.isArray(raw.aliases) ? raw.aliases.map(str).filter((a): a is string => a !== null) : [],
    section: str(raw.section),
    icon: str(raw.icon),
    needsTranslation: raw.needsTranslation === true,
  };
}

export function parseCustomObjects(raw: unknown): CustomObject[] {
  const arr = Array.isArray(raw) ? raw : isRecord(raw) && Array.isArray(raw.items) ? raw.items : [];
  return arr.map(parseCustomObject).filter((o): o is CustomObject => o !== null);
}

/** Custom objects carried by raw server rows (`item` with kind custom). */
export function objectsOfRows(raw: unknown): CustomObject[] {
  if (!Array.isArray(raw)) return [];
  return raw.map((r) => (isRecord(r) ? parseCustomObject(r.item) : null)).filter((o): o is CustomObject => o !== null);
}

export function parseOpsResponse(raw: unknown): OpsResponse {
  const r = isRecord(raw) ? raw : {};
  const ids = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
  const rejected = Array.isArray(r.rejected)
    ? r.rejected.filter(isRecord).map((x) => ({ opId: str(x.opId) ?? "", error: str(x.error) ?? "" }))
    : [];
  return {
    revision: typeof r.revision === "number" && Number.isFinite(r.revision) ? r.revision : 0,
    applied: ids(r.applied),
    duplicates: ids(r.duplicates),
    rejected,
    items: Array.isArray(r.items) ? parseItems(r.items) : null,
    objects: objectsOfRows(r.items),
  };
}
