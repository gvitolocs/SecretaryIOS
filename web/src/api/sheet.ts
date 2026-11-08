// Spreadsheets on lucina-media (docs/xlsx.md): the workbook view of an .xlsx item, saving cell
// edits back into the file, and its earlier versions. Same-origin through p1, parsed by
// lib/sheet.ts. Errors become a SheetError the view can show next to the cell they name.

import { ApiError, request, toApiError } from "./client";
import { parseVersions, parseWorkbook, type SheetEdit, type SheetVersions, type Workbook } from "../lib/sheet";

export type SheetErrorKind = "conflict" | "invalid" | "read_only" | "unsupported" | "other";

export class SheetError extends Error {
  readonly kind: SheetErrorKind;
  readonly status: number;
  /** The server's code: conflict, invalid_value, locked_cell, formula_cell, unknown_sheet, bad_cell, read_only, … */
  readonly code: string;
  readonly sheet: string | null;
  readonly cell: string | null;
  /** On a conflict: the file's current hash. */
  readonly hash: string | null;

  constructor(o: { kind: SheetErrorKind; status: number; code: string; message: string; sheet?: string | null; cell?: string | null; hash?: string | null }) {
    super(o.message);
    this.name = "SheetError";
    this.kind = o.kind;
    this.status = o.status;
    this.code = o.code;
    this.sheet = o.sheet ?? null;
    this.cell = o.cell ?? null;
    this.hash = o.hash ?? null;
  }
}

function text(v: unknown): string | null {
  return typeof v === "string" && v ? v.slice(0, 2000) : null;
}

/** An ApiError (or anything thrown) as a SheetError, reading the sheet API's `{error, message, sheet, cell, hash}`. */
export function toSheetError(e: unknown): SheetError {
  if (e instanceof SheetError) return e;
  const api: ApiError = toApiError(e);
  const body = api.body && typeof api.body === "object" ? (api.body as Record<string, unknown>) : {};
  const code = text(body.error) ?? api.code;
  const message = text(body.message) ?? api.message;
  const kind: SheetErrorKind =
    api.status === 409 || code === "conflict"
      ? "conflict"
      : api.status === 403 || code === "read_only"
        ? "read_only"
        : api.status === 415
          ? "unsupported"
          : api.status === 400 || api.status === 422
            ? "invalid"
            : "other";
  return new SheetError({
    kind,
    status: api.status,
    code,
    message,
    sheet: text(body.sheet),
    cell: text(body.cell)?.toUpperCase() ?? null,
    hash: text(body.hash),
  });
}

async function call<T>(fn: () => Promise<T>): Promise<T> {
  try {
    return await fn();
  } catch (e) {
    throw toSheetError(e);
  }
}

const base = (id: string) => `/v1/media/items/${encodeURIComponent(id)}/sheet`;

export const sheetApi = {
  get(id: string): Promise<Workbook> {
    return call(async () => parseWorkbook(await request<unknown>("GET", base(id), { timeoutMs: 60_000 })));
  },

  /** Write edits into the file; the answer is the new view (new hash, recomputed timesheet). */
  save(id: string, baseHash: string, edits: SheetEdit[]): Promise<Workbook> {
    return call(async () =>
      parseWorkbook(await request<unknown>("POST", base(id), { body: { base_hash: baseHash, edits }, timeoutMs: 60_000 })),
    );
  },

  versions(id: string): Promise<SheetVersions> {
    return call(async () => parseVersions(await request<unknown>("GET", `${base(id)}/versions`)));
  },

  /** Make an earlier version current (the current bytes become a version too). */
  restore(id: string, version: number, baseHash: string): Promise<Workbook> {
    return call(async () =>
      parseWorkbook(await request<unknown>("POST", `${base(id)}/restore`, { body: { version, base_hash: baseHash }, timeoutMs: 60_000 })),
    );
  },
};
