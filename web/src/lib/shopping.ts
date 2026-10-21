// Shopping items come from the server loosely typed. Everything the UI needs
// is derived here, and the raw object is kept so nothing is lost.

export interface ShoppingItem {
  /** Server id as a string, or a synthetic `local-N` id when the item has none. */
  id: string;
  /** False when the server gave no id, so the item cannot be toggled. */
  hasId: boolean;
  name: string;
  quantity: string | null;
  checked: boolean;
  /** Which field the item uses to record bought/unbought. */
  toggleField: "checked" | "status";
  status: string | null;
  raw: Record<string, unknown>;
}

const DONE_STATUSES = new Set(["purchased", "done", "bought", "checked", "complete", "completed", "closed"]);

// [open, done] pairs, so a toggle sends a value from the same vocabulary.
// The first pair is the server's own vocabulary (docs/server-api.md).
const STATUS_PAIRS: Array<[string, string]> = [
  ["needed", "purchased"],
  ["open", "bought"],
  ["to_purchase", "purchased"],
  ["pending", "done"],
  ["todo", "done"],
  ["unchecked", "checked"],
  ["active", "completed"],
];

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function nonEmptyString(v: unknown): string | null {
  return typeof v === "string" && v.trim() ? v.trim() : null;
}

function idOf(raw: Record<string, unknown>): string | null {
  for (const key of ["id", "uuid", "itemId", "_id"]) {
    const v = raw[key];
    if (typeof v === "string" && v) return v;
    if (typeof v === "number" && Number.isFinite(v)) return String(v);
  }
  return null;
}

function nameOf(raw: Record<string, unknown>): string {
  const product = isRecord(raw.product) ? raw.product : null;
  return (
    nonEmptyString(raw.name) ??
    nonEmptyString(raw.rawName) ??
    nonEmptyString(raw.product) ??
    nonEmptyString(product?.name) ??
    nonEmptyString(raw.title) ??
    nonEmptyString(raw.itemId) ??
    "Unnamed item"
  );
}

function quantityOf(raw: Record<string, unknown>): string | null {
  let q: string | null = null;
  const v = raw.quantity ?? raw.qty ?? raw.amount;
  if (typeof v === "number" && Number.isFinite(v)) q = String(v);
  else q = nonEmptyString(v);
  const unit = nonEmptyString(raw.unit);
  if (q && unit) q = `${q} ${unit}`;
  return q ?? nonEmptyString(raw.specification);
}

export function isDoneStatus(status: string): boolean {
  return DONE_STATUSES.has(status.toLowerCase());
}

/** The status value to send when moving `current` to checked/unchecked. */
export function nextStatus(current: string | null, checked: boolean): string {
  const lower = (current ?? "").toLowerCase();
  let next = checked ? "purchased" : "needed";
  for (const [open, done] of STATUS_PAIRS) {
    if (lower === open || lower === done) {
      next = checked ? done : open;
      break;
    }
  }
  const upper = current !== null && current !== "" && current === current.toUpperCase();
  return upper ? next.toUpperCase() : next;
}

export function normalizeItem(rawValue: unknown, index: number): ShoppingItem | null {
  if (!isRecord(rawValue)) return null;
  const raw = rawValue;
  const id = idOf(raw);
  const status = nonEmptyString(raw.status);
  let checked = false;
  let toggleField: ShoppingItem["toggleField"] = "checked";
  if (typeof raw.checked === "boolean") {
    checked = raw.checked;
  } else if (status) {
    checked = isDoneStatus(status);
    toggleField = "status";
  } else if (typeof raw.bought === "boolean") {
    checked = raw.bought;
  } else if (typeof raw.done === "boolean") {
    checked = raw.done;
  }
  return {
    id: id ?? `local-${index}`,
    hasId: id !== null,
    name: nameOf(raw),
    quantity: quantityOf(raw),
    checked,
    toggleField,
    status,
    raw,
  };
}

/** Accepts a bare array (the contract) or `{items: [...]}` defensively. */
export function normalizeList(data: unknown): ShoppingItem[] {
  const list = Array.isArray(data) ? data : isRecord(data) && Array.isArray(data.items) ? data.items : [];
  const out: ShoppingItem[] = [];
  list.forEach((raw, i) => {
    const item = normalizeItem(raw, i);
    if (item) out.push(item);
  });
  return out;
}

export function togglePayload(item: Pick<ShoppingItem, "toggleField" | "status">, checked: boolean): Record<string, unknown> {
  return item.toggleField === "status" ? { status: nextStatus(item.status, checked) } : { checked };
}
