// Spreadsheets (views/Sheet.tsx): the workbook view lucina-media serves for an .xlsx item
// (docs/xlsx.md, `GET /v1/media/items/{id}/sheet`). Pure logic only: parsing the JSON
// defensively (every field may be missing), A1 references, the grid model of one sheet
// (hidden rows and columns, sizes, merges), cell styles as CSS, and the edit list a save sends.

import { sameOriginPath } from "../api/media";

// ---------------------------------------------------------------- types

export interface SheetStyle {
  fmt: string;
  kind: string;
  bold: boolean;
  italic: boolean;
  underline: boolean;
  size: number | null;
  color: string | null;
  fill: string | null;
  align: string | null;
  valign: string | null;
  wrap: boolean;
  /** Top, right, bottom, left. */
  border: [boolean, boolean, boolean, boolean];
}

export type CellValue = string | number | boolean | null;

export interface SheetCell {
  ref: string;
  r: number;
  c: number;
  /** Index into `styles`. */
  s: number;
  /** s(tring), n(umber), b(ool), e(rror), z (empty). */
  t: string;
  v: CellValue;
  display: string;
  /** The formula, without `=`; null for a value cell. */
  f: string | null;
  editable: boolean;
  input: boolean;
}

export interface SheetCol {
  min: number;
  max: number;
  width: number | null;
  hidden: boolean;
}

export interface SheetRow {
  r: number;
  height: number | null;
  hidden: boolean;
}

export interface SheetValidation {
  type: string;
  sqref: string;
  /** The choices of a list validation, resolved by the server. */
  list: string[] | null;
  strict: boolean;
}

export type SheetState = "visible" | "hidden" | "veryHidden";

export interface Sheet {
  name: string;
  index: number;
  state: SheetState;
  protected: boolean;
  maxRow: number;
  maxCol: number;
  frozen: { rows: number; cols: number } | null;
  cols: SheetCol[];
  rows: SheetRow[];
  merges: string[];
  validations: SheetValidation[];
  cells: SheetCell[];
  defaultColWidth: number | null;
  defaultRowHeight: number | null;
  /** Cells the server left out (too many). */
  truncated: number;
}

export type FieldKind = "date" | "text" | "number" | "bool";

export interface TsField {
  key: string;
  label: string;
  sheet: string;
  cell: string;
  kind: FieldKind;
  value: string;
  editable: boolean;
}

export interface TsDay {
  row: number;
  date: string;
  weekday: string;
  holiday: string | null;
  workday: boolean;
  employed: boolean;
  code: string;
  actual: string;
  start: string;
  end: string;
  remarks: string;
  norm: string;
  worked: string;
  diff: string;
}

export interface TsColumns {
  code: string;
  actual: string;
  start: string;
  end: string;
  remarks: string;
}

export interface TsMonth {
  sheet: string;
  month: number;
  year: number;
  weeklyHours: TsField | null;
  dayOff: TsField | null;
  columns: TsColumns;
  codes: string[];
  days: TsDay[];
  norm: string;
  worked: string;
  diff: string;
  registeredDays: number;
}

export interface Timesheet {
  kind: string;
  masterSheet: string;
  master: TsField[];
  months: TsMonth[];
  balanceCarried: string;
  diff: string;
  total: string;
}

export type SheetLocation = { kind: "drive"; folder: string; path: string } | { kind: "library" };

export interface Workbook {
  itemId: string;
  name: string;
  hash: string;
  version: number | null;
  writable: boolean;
  location: SheetLocation;
  downloadUrl: string | null;
  fullCalcOnLoad: boolean;
  sheets: Sheet[];
  styles: SheetStyle[];
  timesheet: Timesheet | null;
  /** After a save: the cells it changed. */
  changed: { sheet: string; cell: string; before: string; after: string }[];
}

export interface SheetVersion {
  version: number;
  hash: string;
  replacedAt: string | null;
  bytes: number | null;
  downloadUrl: string | null;
}

export interface SheetVersions {
  current: { version: number | null; hash: string; bytes: number | null };
  versions: SheetVersion[];
}

// ---------------------------------------------------------------- parsing helpers

type Obj = Record<string, unknown>;

function obj(v: unknown): Obj | null {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : null;
}

function arr(v: unknown): unknown[] {
  return Array.isArray(v) ? v : [];
}

function str(v: unknown, max = 4096): string | null {
  return typeof v === "string" ? v.slice(0, max) : null;
}

function int(v: unknown, min = 0): number | null {
  return typeof v === "number" && Number.isFinite(v) && v >= min ? Math.floor(v) : null;
}

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

const COLOR = /^#[0-9a-f]{3,8}$/i;

function color(v: unknown): string | null {
  return typeof v === "string" && COLOR.test(v) ? v : null;
}

export function parseStyle(raw: unknown): SheetStyle {
  const o = obj(raw) ?? {};
  const b = arr(o.border);
  return {
    fmt: str(o.fmt, 256) ?? "General",
    kind: str(o.kind, 32) ?? "general",
    bold: o.bold === true,
    italic: o.italic === true,
    underline: o.underline === true,
    size: num(o.size),
    color: color(o.color),
    fill: color(o.fill),
    align: str(o.align, 32),
    valign: str(o.valign, 32),
    wrap: o.wrap === true,
    border: [b[0] === true, b[1] === true, b[2] === true, b[3] === true],
  };
}

function parseCell(raw: unknown): SheetCell | null {
  const o = obj(raw);
  if (!o) return null;
  let r = int(o.r, 1);
  let c = int(o.c, 1);
  const ref = str(o.ref, 16)?.toUpperCase() ?? null;
  if ((r === null || c === null) && ref) {
    const p = parseRef(ref);
    if (p) ({ r, c } = p);
  }
  if (r === null || c === null) return null;
  const v = o.v;
  return {
    ref: ref ?? cellRef(r, c),
    r,
    c,
    s: int(o.s) ?? 0,
    t: str(o.t, 4) ?? "z",
    v: typeof v === "string" || typeof v === "number" || typeof v === "boolean" ? v : null,
    display: str(o.display, 32_768) ?? (v === null || v === undefined ? "" : String(v)),
    f: typeof o.f === "string" ? o.f : null,
    editable: o.editable === true,
    input: o.input === true,
  };
}

const STATES: SheetState[] = ["visible", "hidden", "veryHidden"];

export function parseSheet(raw: unknown, i = 0): Sheet | null {
  const o = obj(raw);
  const name = o && str(o.name, 256);
  if (!o || !name) return null;
  const cells = arr(o.cells)
    .map(parseCell)
    .filter((x): x is SheetCell => x !== null);
  const cols: SheetCol[] = [];
  for (const x of arr(o.cols)) {
    const c = obj(x);
    const min = c && int(c.min, 1);
    if (!c || min === null) continue;
    cols.push({ min, max: Math.max(min, int(c.max, 1) ?? min), width: num(c.width), hidden: c.hidden === true });
  }
  const rows: SheetRow[] = [];
  for (const x of arr(o.rows)) {
    const rr = obj(x);
    const r = rr && int(rr.r, 1);
    if (!rr || r === null) continue;
    rows.push({ r, height: num(rr.height), hidden: rr.hidden === true });
  }
  const validations: SheetValidation[] = [];
  for (const x of arr(o.validations)) {
    const v = obj(x);
    const sqref = v && str(v.sqref, 4096);
    if (!v || !sqref) continue;
    const list = Array.isArray(v.list) ? v.list.filter((s): s is string => typeof s === "string") : null;
    validations.push({ type: str(v.type, 32) ?? "", sqref, list, strict: v.strict === true });
  }
  const f = obj(o.frozen);
  const frozen = f ? { rows: int(f.rows) ?? 0, cols: int(f.cols) ?? 0 } : null;
  const maxRow = Math.max(int(o.max_row) ?? 0, ...cells.map((c) => c.r), 0);
  const maxCol = Math.max(int(o.max_col) ?? 0, ...cells.map((c) => c.c), 0);
  return {
    name,
    index: int(o.index) ?? i,
    state: STATES.find((s) => s === o.state) ?? "visible",
    protected: o.protected === true,
    maxRow,
    maxCol,
    frozen: frozen && (frozen.rows > 0 || frozen.cols > 0) ? frozen : null,
    cols,
    rows,
    merges: arr(o.merges).filter((m): m is string => typeof m === "string" && m.length < 32),
    validations,
    cells,
    defaultColWidth: num(o.default_col_width),
    defaultRowHeight: num(o.default_row_height),
    truncated: int(o.truncated) ?? 0,
  };
}

const KINDS: FieldKind[] = ["date", "text", "number", "bool"];

function parseField(raw: unknown): TsField | null {
  const o = obj(raw);
  const sheet = o && str(o.sheet, 256);
  const cell = o && str(o.cell, 16);
  if (!o || !sheet || !cell || !parseRef(cell)) return null;
  const key = str(o.key, 64) ?? cell;
  return {
    key,
    label: str(o.label, 512) ?? key,
    sheet,
    cell: cell.toUpperCase(),
    kind: KINDS.find((k) => k === o.kind) ?? "text",
    value: str(o.value, 4096) ?? "",
    editable: o.editable === true,
  };
}

function parseDay(raw: unknown): TsDay | null {
  const o = obj(raw);
  const row = o && int(o.row, 1);
  if (!o || row === null) return null;
  const s = (k: string) => str(o[k], 4096) ?? "";
  return {
    row,
    date: s("date"),
    weekday: s("weekday"),
    holiday: str(o.holiday, 256) || null,
    workday: o.workday !== false,
    employed: o.employed !== false,
    code: s("code"),
    actual: s("actual"),
    start: s("start"),
    end: s("end"),
    remarks: s("remarks"),
    norm: s("norm"),
    worked: s("worked"),
    diff: s("diff"),
  };
}

const COLUMN = /^[A-Z]{1,3}$/;

function parseMonth(raw: unknown): TsMonth | null {
  const o = obj(raw);
  const sheet = o && str(o.sheet, 256);
  const month = o && int(o.month, 1);
  const c = o && obj(o.columns);
  if (!o || !sheet || month === null || month > 12 || !c) return null;
  const col = (k: string) => {
    const v = str(c[k], 8)?.toUpperCase() ?? "";
    return COLUMN.test(v) ? v : "";
  };
  const columns = { code: col("code"), actual: col("actual"), start: col("start"), end: col("end"), remarks: col("remarks") };
  return {
    sheet,
    month,
    year: int(o.year) ?? 0,
    weeklyHours: parseField(o.weekly_hours),
    dayOff: parseField(o.day_off),
    columns,
    codes: arr(o.codes).filter((x): x is string => typeof x === "string"),
    days: arr(o.days)
      .map(parseDay)
      .filter((x): x is TsDay => x !== null),
    norm: str(o.norm, 32) ?? "",
    worked: str(o.worked, 32) ?? "",
    diff: str(o.diff, 32) ?? "",
    registeredDays: int(o.registered_days) ?? 0,
  };
}

export function parseTimesheet(raw: unknown): Timesheet | null {
  const o = obj(raw);
  if (!o) return null;
  const months = arr(o.months)
    .map(parseMonth)
    .filter((x): x is TsMonth => x !== null)
    .sort((a, b) => a.month - b.month);
  return {
    kind: str(o.kind, 64) ?? "",
    masterSheet: str(o.master_sheet, 256) ?? "",
    master: arr(o.master)
      .map(parseField)
      .filter((x): x is TsField => x !== null),
    months,
    balanceCarried: str(o.balance_carried, 32) ?? "",
    diff: str(o.diff, 32) ?? "",
    total: str(o.total, 32) ?? "",
  };
}

function parseLocation(raw: unknown): SheetLocation {
  const o = obj(raw);
  const folder = o && str(o.folder, 512);
  if (o?.kind === "drive" && folder) return { kind: "drive", folder, path: str(o.path, 4096) ?? "" };
  return { kind: "library" };
}

/** `GET /v1/media/items/{id}/sheet` (and the answers of save and restore). */
export function parseWorkbook(raw: unknown): Workbook {
  const o = obj(raw) ?? {};
  const changed: Workbook["changed"] = [];
  for (const x of arr(o.changed)) {
    const c = obj(x);
    if (!c || typeof c.sheet !== "string" || typeof c.cell !== "string") continue;
    changed.push({ sheet: c.sheet, cell: c.cell, before: str(c.before) ?? "", after: str(c.after) ?? "" });
  }
  return {
    itemId: str(o.item_id, 128) ?? "",
    name: str(o.name, 1024) ?? "",
    hash: str(o.hash, 128) ?? "",
    version: int(o.version),
    writable: o.writable !== false,
    location: parseLocation(o.location),
    downloadUrl: sameOriginPath(o.download_url) ?? null,
    fullCalcOnLoad: o.full_calc_on_load === true,
    sheets: arr(o.sheets)
      .map(parseSheet)
      .filter((x): x is Sheet => x !== null),
    styles: arr(o.styles).map(parseStyle),
    timesheet: parseTimesheet(o.timesheet),
    changed,
  };
}

/** `GET /v1/media/items/{id}/sheet/versions`. */
export function parseVersions(raw: unknown): SheetVersions {
  const o = obj(raw) ?? {};
  const cur = obj(o.current) ?? {};
  const versions: SheetVersion[] = [];
  for (const x of arr(o.versions)) {
    const v = obj(x);
    const version = v && int(v.version);
    if (!v || version === null) continue;
    versions.push({
      version,
      hash: str(v.hash, 128) ?? "",
      replacedAt: str(v.replaced_at, 64),
      bytes: int(v.bytes),
      downloadUrl: sameOriginPath(v.download_url) ?? null,
    });
  }
  return { current: { version: int(cur.version), hash: str(cur.hash, 128) ?? "", bytes: int(cur.bytes) }, versions };
}

/** Sheets shown as tabs: hidden and very hidden ones are left out. */
export function visibleSheets(wb: Workbook): Sheet[] {
  return wb.sheets.filter((s) => s.state === "visible");
}

// ---------------------------------------------------------------- references

/** 1 → "A", 28 → "AB". */
export function colName(n: number): string {
  let s = "";
  let x = Math.floor(n);
  while (x > 0) {
    const m = (x - 1) % 26;
    s = String.fromCharCode(65 + m) + s;
    x = Math.floor((x - 1) / 26);
  }
  return s;
}

/** "AB" → 28; 0 when not a column. */
export function colNumber(name: string): number {
  const s = name.toUpperCase();
  if (!/^[A-Z]{1,3}$/.test(s)) return 0;
  let n = 0;
  for (const ch of s) n = n * 26 + (ch.charCodeAt(0) - 64);
  return n;
}

export function cellRef(r: number, c: number): string {
  return `${colName(c)}${r}`;
}

/** "J10" (or "$J$10") → {r: 10, c: 10}. */
export function parseRef(ref: string): { r: number; c: number } | null {
  const m = /^\$?([A-Za-z]{1,3})\$?(\d{1,7})$/.exec(ref.trim());
  if (!m) return null;
  const c = colNumber(m[1]!);
  const r = Number(m[2]);
  return c > 0 && r > 0 ? { r, c } : null;
}

export interface Range {
  r0: number;
  c0: number;
  r1: number;
  c1: number;
}

/** "S39:T39" or a single "B2". */
export function parseRange(s: string): Range | null {
  const [a, b] = s.split(":");
  const p = a ? parseRef(a) : null;
  const q = b ? parseRef(b) : p;
  if (!p || !q) return null;
  return { r0: Math.min(p.r, q.r), c0: Math.min(p.c, q.c), r1: Math.max(p.r, q.r), c1: Math.max(p.c, q.c) };
}

/** A data validation's `sqref`: ranges separated by spaces. */
export function inSqref(sqref: string, r: number, c: number): boolean {
  return sqref
    .split(/\s+/)
    .map(parseRange)
    .some((g) => !!g && r >= g.r0 && r <= g.r1 && c >= g.c0 && c <= g.c1);
}

/** The choices of the list validation covering a cell, or null. */
export function listAt(sheet: Sheet, r: number, c: number): string[] | null {
  for (const v of sheet.validations) {
    if (v.list && v.list.length > 0 && inSqref(v.sqref, r, c)) return v.list;
  }
  return null;
}

// ---------------------------------------------------------------- the grid model

/** Excel character units → px. */
export function colPx(width: number): number {
  return Math.round(width * 7 + 5);
}

/** Points → px. */
export function rowPx(height: number): number {
  return Math.round((height * 4) / 3);
}

const DEFAULT_COL_WIDTH = 8.43;
const DEFAULT_ROW_HEIGHT = 15;
/** Rows past this are not shown (a note says so); the grid renders only a window of them anyway. */
export const MAX_GRID_ROWS = 50_000;
export const MAX_GRID_COLS = 500;
/** Merges bigger than this many cells are ignored (a damaged file). */
const MAX_MERGE_AREA = 100_000;

export interface GridCol {
  c: number;
  name: string;
  px: number;
}

export interface GridRow {
  r: number;
  px: number;
  /** Top offset in px within the rows (not counting the column header). */
  top: number;
}

export interface GridSlot {
  r: number;
  c: number;
  rowSpan: number;
  colSpan: number;
  /** The cell drawn here: the merge's top-left cell for a merge. */
  cell: SheetCell | undefined;
  /** The address the slot stands for (the merge's top-left). */
  ref: string;
}

export interface GridModel {
  sheet: Sheet;
  cols: GridCol[];
  rows: GridRow[];
  /** Total height of the rows in px. */
  height: number;
  /** More rows or columns than the grid shows. */
  clipped: boolean;
  /** Frozen visible rows and columns (counts into `rows` / `cols`). */
  frozenRows: number;
  frozenCols: number;
  cell(r: number, c: number): SheetCell | undefined;
  merge(r: number, c: number): Range | undefined;
}

export function buildGrid(sheet: Sheet): GridModel {
  const byCell = new Map<number, SheetCell>();
  const key = (r: number, c: number) => r * 20_000 + c;
  for (const cell of sheet.cells) byCell.set(key(cell.r, cell.c), cell);

  const colInfo = (c: number) => sheet.cols.find((x) => c >= x.min && c <= x.max);
  const maxCol = Math.min(sheet.maxCol, MAX_GRID_COLS);
  const cols: GridCol[] = [];
  for (let c = 1; c <= maxCol; c++) {
    const info = colInfo(c);
    if (info?.hidden) continue;
    cols.push({ c, name: colName(c), px: colPx(info?.width ?? sheet.defaultColWidth ?? DEFAULT_COL_WIDTH) });
  }

  const rowInfo = new Map(sheet.rows.map((x) => [x.r, x]));
  const maxRow = Math.min(sheet.maxRow, MAX_GRID_ROWS);
  const rows: GridRow[] = [];
  let top = 0;
  for (let r = 1; r <= maxRow; r++) {
    const info = rowInfo.get(r);
    if (info?.hidden) continue;
    const px = Math.max(rowPx(info?.height ?? sheet.defaultRowHeight ?? DEFAULT_ROW_HEIGHT), 4);
    rows.push({ r, px, top });
    top += px;
  }

  const merges = new Map<number, Range>();
  for (const m of sheet.merges) {
    const g = parseRange(m);
    if (!g || (g.r1 - g.r0 + 1) * (g.c1 - g.c0 + 1) > MAX_MERGE_AREA) continue;
    for (let r = g.r0; r <= g.r1; r++) for (let c = g.c0; c <= g.c1; c++) merges.set(key(r, c), g);
  }

  const frozenRows = sheet.frozen ? rows.filter((x) => x.r <= sheet.frozen!.rows).length : 0;
  const frozenCols = sheet.frozen ? cols.filter((x) => x.c <= sheet.frozen!.cols).length : 0;

  return {
    sheet,
    cols,
    rows,
    height: top,
    clipped: sheet.maxRow > maxRow || sheet.maxCol > maxCol,
    frozenRows,
    frozenCols,
    cell: (r, c) => byCell.get(key(r, c)),
    merge: (r, c) => merges.get(key(r, c)),
  };
}

/**
 * The slots to draw in one row, for rows drawn as a contiguous run `[from, to]` (indexes into
 * `grid.rows`). A merge is drawn once, at its first visible row of the run and its first visible
 * column, spanning the visible rows and columns it covers; the cells it covers are skipped.
 */
export function rowSlots(grid: GridModel, index: number, from: number, to: number): GridSlot[] {
  const row = grid.rows[index];
  if (!row) return [];
  const out: GridSlot[] = [];
  for (let ci = 0; ci < grid.cols.length; ci++) {
    const col = grid.cols[ci]!;
    const m = grid.merge(row.r, col.c);
    if (!m) {
      out.push({ r: row.r, c: col.c, rowSpan: 1, colSpan: 1, cell: grid.cell(row.r, col.c), ref: cellRef(row.r, col.c) });
      continue;
    }
    // The merge's visible rows within the run, and its visible columns.
    let first = -1;
    let last = -1;
    for (let i = from; i <= to; i++) {
      const rr = grid.rows[i];
      if (!rr || rr.r < m.r0) continue;
      if (rr.r > m.r1) break;
      if (first < 0) first = i;
      last = i;
    }
    if (first !== index) continue;
    let firstCol = ci;
    while (firstCol > 0 && grid.cols[firstCol - 1]!.c >= m.c0) firstCol--;
    if (firstCol !== ci) continue;
    let span = 1;
    while (ci + span < grid.cols.length && grid.cols[ci + span]!.c <= m.c1) span++;
    out.push({ r: row.r, c: col.c, rowSpan: last - first + 1, colSpan: span, cell: grid.cell(m.r0, m.c0), ref: cellRef(m.r0, m.c0) });
  }
  return out;
}

/** The index range `[from, to]` of the rows (after `skip` frozen ones) inside a viewport, plus `margin` rows each side. */
export function rowWindow(grid: GridModel, skip: number, scrollTop: number, viewport: number, margin = 12): [number, number] {
  const rows = grid.rows;
  if (rows.length <= skip) return [skip, skip - 1];
  const base = rows[skip]!.top;
  const y0 = base + Math.max(0, scrollTop);
  const y1 = y0 + Math.max(viewport, 1);
  let lo = skip;
  let hi = rows.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (rows[mid]!.top + rows[mid]!.px <= y0) lo = mid + 1;
    else hi = mid;
  }
  let end = lo;
  while (end < rows.length - 1 && rows[end]!.top + rows[end]!.px < y1) end++;
  return [Math.max(skip, lo - margin), Math.min(rows.length - 1, end + margin)];
}

// ---------------------------------------------------------------- styles

/** A cell's look from its style: fill, colour, font, alignment, wrapping and borders. */
export function styleCss(style: SheetStyle | undefined, numeric = false): Record<string, string> {
  const css: Record<string, string> = {};
  if (!style) {
    if (numeric) css["text-align"] = "right";
    return css;
  }
  if (style.fill) {
    css["background-color"] = style.fill;
    // Dark text on the file's own (usually light) fills, also in dark mode.
    css.color = style.color ?? "#1c1c1e";
  } else if (style.color && style.color.toUpperCase() !== "#000000") {
    css.color = style.color;
  }
  if (style.bold) css["font-weight"] = "600";
  if (style.italic) css["font-style"] = "italic";
  if (style.underline) css["text-decoration"] = "underline";
  if (style.size && style.size !== 11) css["font-size"] = `${Math.min(Math.max(style.size, 6), 36) / 11}em`;
  const align = style.align === "centerContinuous" ? "center" : style.align;
  if (align && ["left", "center", "right", "justify"].includes(align)) css["text-align"] = align;
  else if (numeric) css["text-align"] = "right";
  if (style.valign === "center") css["vertical-align"] = "middle";
  else if (style.valign === "top" || style.valign === "bottom") css["vertical-align"] = style.valign;
  css["white-space"] = style.wrap ? "pre-wrap" : "pre";
  const line = "1px solid var(--sheet-border)";
  const [bt, br, bb, bl] = style.border;
  if (bt) css["border-top"] = line;
  if (br) css["border-right"] = line;
  if (bb) css["border-bottom"] = line;
  if (bl) css["border-left"] = line;
  return css;
}

// ---------------------------------------------------------------- edits

export interface PendingEdit {
  sheet: string;
  cell: string;
  /** What the owner typed. */
  text: string;
  /** What the cell or field showed before. */
  original: string;
}

export interface SheetEdit {
  sheet: string;
  cell: string;
  value: { t: "input"; v: string } | { t: "empty" };
}

export function editKey(sheet: string, cell: string): string {
  return `${sheet}!${cell.toUpperCase()}`;
}

const TIME = /^(-?)(\d{1,3}):(\d{2})$/;

/** Minutes of an `hh:mm` text, or null. */
export function timeMinutes(s: string): number | null {
  const m = TIME.exec(s.trim());
  if (!m) return null;
  const v = Number(m[2]) * 60 + Number(m[3]);
  return m[1] ? -v : v;
}

/** Whether typed text still says the same as the original ("9:00" = "09:00"; an empty time is 0). */
export function sameText(a: string, b: string): boolean {
  const x = a.trim();
  const y = b.trim();
  if (x === y) return true;
  const ta = x === "" ? 0 : timeMinutes(x);
  const tb = y === "" ? 0 : timeMinutes(y);
  return ta !== null && tb !== null && ta === tb && (x !== "" || y !== "");
}

/** The edits a save sends: only the changed ones, `{t: "empty"}` for an emptied cell. */
export function buildEdits(pending: Iterable<PendingEdit>): SheetEdit[] {
  const out: SheetEdit[] = [];
  for (const p of pending) {
    if (sameText(p.text, p.original)) continue;
    const text = p.text.trim();
    out.push({ sheet: p.sheet, cell: p.cell.toUpperCase(), value: text === "" ? { t: "empty" } : { t: "input", v: text } });
  }
  return out;
}

// ---------------------------------------------------------------- the timesheet form

export type DayColumn = keyof TsColumns;

/** A day field's address: `columns.start + row` → "J10"; null when the template has no such column. */
export function dayCell(month: TsMonth, column: DayColumn, row: number): string | null {
  const col = month.columns[column];
  return col ? `${col}${row}` : null;
}

/**
 * The codes a day may take. The server's list follows the saved weekly-day-off answer (`F` only
 * while it is yes); an answer typed but not saved moves `F` too, and the cell's own value stays
 * selectable.
 */
export function codesFor(month: TsMonth, typedDayOff: string | undefined, current: string): string[] {
  let codes = month.codes;
  if (typedDayOff === "yes" && !codes.includes("F")) codes = ["F", ...codes];
  if (typedDayOff === "no") codes = codes.filter((c) => c !== "F");
  return current && !codes.includes(current) ? [...codes, current] : codes;
}

/** The month to show first: the current one when the workbook has it, else the first. */
export function defaultMonth(ts: Timesheet, now = new Date()): number {
  const m = now.getMonth() + 1;
  return ts.months.some((x) => x.month === m) ? m : (ts.months[0]?.month ?? m);
}

/** The folder a Drive workbook lives in, for the back link. */
export function locationDir(loc: SheetLocation): { folder: string; path: string } | null {
  if (loc.kind !== "drive") return null;
  const parts = loc.path.split("/").filter(Boolean);
  parts.pop();
  return { folder: loc.folder, path: parts.join("/") };
}

/** True for a negative `hh:mm` total. */
export function isNegative(s: string): boolean {
  return s.trim().startsWith("-") && timeMinutes(s) !== 0;
}
