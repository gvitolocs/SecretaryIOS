// Shopping v2 items and ops (docs/shopping-v2.md). `applyOps` is the client's
// copy of the server semantics, used for optimistic UI and for rebasing the
// outbox: op-id dedup, one active row per (list, catalogId-or-folded-name),
// `add` revives a recent row, and a removed row never comes back.

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

export interface ShopItem {
  id: string;
  catalogId: string | null;
  name: string;
  spec: string;
  quantity: number | null;
  unit: string | null;
  section: string | null;
  icon: string | null;
  /** The server says `icon` is the row's own, hand-picked emoji (else it is the catalog's). */
  iconPicked?: boolean;
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
}

interface OpBase {
  opId: string;
}
export type AddOp = OpBase & {
  type: "add";
  /** Client-chosen id for a new row (contract extension; see the report). */
  itemId?: string;
  catalogId?: string;
  name: string;
  spec?: string;
  quantity?: number | null;
  unit?: string | null;
  section?: string | null;
};
export type Op =
  | AddOp
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
]);

export interface CatalogHint {
  name: string;
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
  catalog?: (catalogId: string) => CatalogHint | undefined;
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

/**
 * One active row per (list, catalogId-or-folded-name): a row matches when the
 * catalog ids agree, or when the folded names do ("pane" finds the catalog "Pane").
 */
export function sameRow(item: Pick<ShopItem, "catalogId" | "name">, catalogId: string | null | undefined, name: string): boolean {
  if (catalogId && item.catalogId === catalogId) return true;
  if (catalogId && item.catalogId && item.catalogId !== catalogId) return false;
  return name !== "" && fold(item.name) === fold(name);
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
      const hint = op.catalogId ? ctx.catalog?.(op.catalogId) : undefined;
      const name = clip(op.name || hint?.name || "");
      if (!name && !op.catalogId) return false;
      const existing =
        (op.itemId ? active(op.itemId) : undefined) ?? items.find((i) => isActive(i) && sameRow(i, op.catalogId, name));
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
      const id = op.itemId && !items.some((i) => i.id === op.itemId) ? op.itemId : ctx.newId();
      items.push({
        id,
        catalogId: op.catalogId ?? null,
        name: name || op.catalogId || "",
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
      item.name = name;
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
  if (op.type === "clear_recent") return null;
  if (op.type === "add") return op.itemId ?? null;
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

export function parseItem(raw: unknown, index = 0): ShopItem | null {
  if (!isRecord(raw)) return null;
  const id = str(raw.id) ?? (typeof raw.id === "number" ? String(raw.id) : null);
  const name = str(raw.name);
  if (!id || !name) return null;
  const status: ItemStatus = raw.status === "recent" || raw.status === "removed" ? raw.status : "needed";
  return {
    id,
    catalogId: str(raw.catalogId),
    name,
    spec: typeof raw.spec === "string" ? raw.spec : "",
    quantity: validQuantity(raw.quantity),
    unit: str(raw.unit),
    section: str(raw.section),
    icon: str(raw.icon),
    iconPicked: raw.iconPicked === true,
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
  };
}

export interface OpsResponse {
  revision: number;
  applied: string[];
  duplicates: string[];
  items: ShopItem[] | null;
}

export function parseOpsResponse(raw: unknown): OpsResponse {
  const r = isRecord(raw) ? raw : {};
  const ids = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
  return {
    revision: typeof r.revision === "number" && Number.isFinite(r.revision) ? r.revision : 0,
    applied: ids(r.applied),
    duplicates: ids(r.duplicates),
    items: Array.isArray(r.items) ? parseItems(r.items) : null,
  };
}
