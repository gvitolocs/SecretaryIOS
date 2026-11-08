import { describe, expect, it } from "vitest";
import {
  buildEdits,
  buildGrid,
  cellRef,
  codesFor,
  colName,
  colNumber,
  dayCell,
  defaultMonth,
  inSqref,
  listAt,
  locationDir,
  parseRange,
  parseRef,
  parseSheet,
  parseTimesheet,
  parseWorkbook,
  rowSlots,
  rowWindow,
  sameText,
  styleCss,
  parseStyle,
  type TsMonth,
} from "./sheet";

const SHEET = {
  index: 0,
  name: "November",
  state: "visible",
  protected: true,
  max_row: 6,
  max_col: 6,
  cols: [{ min: 2, max: 3, width: 10, hidden: true }, { min: 4, max: 4, width: 20 }],
  rows: [{ r: 3, height: 30 }, { r: 4, hidden: true }],
  merges: ["E2:F3"],
  validations: [{ sqref: "A1:A3 D5", type: "list", list: ["N", "R"] }, { sqref: "D1", type: "time" }],
  cells: [
    { ref: "A1", r: 1, c: 1, s: 0, t: "s", v: "x", display: "x" },
    { ref: "E2", r: 2, c: 5, s: 0, t: "s", v: "m", display: "m", input: true, editable: true },
    "junk",
    { ref: "D5" },
  ],
};

describe("references", () => {
  it("converts columns and refs", () => {
    expect(colName(1)).toBe("A");
    expect(colName(28)).toBe("AB");
    expect(colNumber("AS")).toBe(45);
    expect(colNumber("A1")).toBe(0);
    expect(cellRef(10, 10)).toBe("J10");
    expect(parseRef("$J$10")).toEqual({ r: 10, c: 10 });
    expect(parseRef("10J")).toBeNull();
    expect(parseRange("F38:F9")).toEqual({ r0: 9, c0: 6, r1: 38, c1: 6 });
    expect(inSqref("A1 I9:K38", 20, 10)).toBe(true);
    expect(inSqref("I9:K38", 20, 12)).toBe(false);
  });
});

describe("parsing", () => {
  it("tolerates missing and wrong fields", () => {
    const wb = parseWorkbook({ sheets: [SHEET, { nope: 1 }], styles: [{ fill: "red", color: "#123456", border: [true] }], timesheet: null, download_url: "https://evil.example/x" });
    expect(wb.sheets).toHaveLength(1);
    expect(wb.sheets[0]!.cells.map((c) => c.ref)).toEqual(["A1", "E2", "D5"]);
    expect(wb.styles[0]!.fill).toBeNull();
    expect(wb.styles[0]!.color).toBe("#123456");
    expect(wb.downloadUrl).toBeNull();
    expect(wb.writable).toBe(true);
    expect(parseWorkbook(null).sheets).toEqual([]);
  });

  it("reads the timesheet", () => {
    const ts = parseTimesheet({
      kind: "au-worktime",
      master_sheet: "Masterdata",
      master: [{ key: "name", label: "Name", sheet: "Masterdata", cell: "B6", kind: "text", value: "", editable: true }, { key: "bad", sheet: "M", cell: "not a cell" }],
      months: [
        { sheet: "December", month: 12, year: 2026, columns: { code: "F", actual: "I", start: "J", end: "K", remarks: "S" }, codes: ["N"], days: [{ row: 9, date: "2026-12-01" }, { nope: 1 }] },
        { sheet: "January", month: 1, columns: { code: "f" } },
        { sheet: "Bad", month: 13, columns: {} },
      ],
      total: "-01:00",
    })!;
    expect(ts.master.map((f) => f.key)).toEqual(["name"]);
    expect(ts.months.map((m) => m.sheet)).toEqual(["January", "December"]);
    expect(ts.months[1]!.days).toHaveLength(1);
    expect(ts.months[0]!.columns.code).toBe("F");
    expect(dayCell(ts.months[1]!, "start", 10)).toBe("J10");
    expect(dayCell(ts.months[0]!, "start", 10)).toBeNull();
    expect(defaultMonth(ts, new Date("2026-12-15"))).toBe(12);
    expect(defaultMonth(ts, new Date("2026-06-15"))).toBe(1);
  });
});

describe("the grid", () => {
  const sheet = parseSheet(SHEET)!;
  const grid = buildGrid(sheet);

  it("skips hidden rows and columns and sizes the rest", () => {
    expect(grid.cols.map((c) => c.name)).toEqual(["A", "D", "E", "F"]);
    expect(grid.cols[1]!.px).toBe(145);
    expect(grid.rows.map((r) => r.r)).toEqual([1, 2, 3, 5, 6]);
    expect(grid.rows[2]!.px).toBe(40);
  });

  it("draws a merge once, spanning, and skips what it covers", () => {
    const row2 = rowSlots(grid, 1, 0, 4);
    const merged = row2.find((s) => s.ref === "E2")!;
    expect([merged.rowSpan, merged.colSpan]).toEqual([2, 2]);
    expect(merged.cell?.display).toBe("m");
    expect(row2.map((s) => s.ref)).toEqual(["A2", "D2", "E2"]);
    expect(rowSlots(grid, 2, 0, 4).map((s) => s.ref)).toEqual(["A3", "D3"]);
    // A run that starts inside the merge still draws it, at the run's first row.
    expect(rowSlots(grid, 2, 2, 4).map((s) => s.ref)).toEqual(["A3", "D3", "E2"]);
  });

  it("finds validations and windows the rows", () => {
    expect(listAt(sheet, 2, 1)).toEqual(["N", "R"]);
    expect(listAt(sheet, 1, 4)).toBeNull();
    const big = buildGrid(parseSheet({ name: "s", max_row: 1000, max_col: 2, default_row_height: 15 })!);
    const [from, to] = rowWindow(big, 0, 2000, 400, 2);
    expect(from).toBeLessThan(100);
    expect(from).toBeGreaterThan(90);
    expect(to).toBeLessThan(125);
    expect(big.height).toBe(20000);
  });

  it("turns styles into css", () => {
    const css = styleCss(parseStyle({ fill: "#FFFF00", bold: true, align: "center", wrap: true, border: [true, false, false, false] }));
    expect(css["background-color"]).toBe("#FFFF00");
    expect(css.color).toBe("#1c1c1e");
    expect(css["font-weight"]).toBe("600");
    expect(css["text-align"]).toBe("center");
    expect(css["white-space"]).toBe("pre-wrap");
    expect(css["border-top"]).toContain("--sheet-border");
    expect(css["border-left"]).toBeUndefined();
    expect(styleCss(undefined, true)["text-align"]).toBe("right");
  });
});

describe("edits", () => {
  it("sends only what changed", () => {
    expect(sameText("9:00", "09:00")).toBe(true);
    expect(sameText("", "00:00")).toBe(true);
    expect(sameText("00:00", "")).toBe(true);
    expect(sameText("R", "N")).toBe(false);
    expect(
      buildEdits([
        { sheet: "M", cell: "f10", text: " R ", original: "N" },
        { sheet: "M", cell: "J10", text: "9:00", original: "09:00" },
        { sheet: "M", cell: "S10", text: "", original: "old" },
      ]),
    ).toEqual([
      { sheet: "M", cell: "F10", value: { t: "input", v: "R" } },
      { sheet: "M", cell: "S10", value: { t: "empty" } },
    ]);
  });

  it("follows the unsaved day-off answer for the allowed codes", () => {
    const m = { codes: ["N", "R"] } as TsMonth;
    expect(codesFor(m, undefined, "N")).toEqual(["N", "R"]);
    expect(codesFor(m, "yes", "N")).toEqual(["F", "N", "R"]);
    const off = { codes: ["F", "N", "R"] } as TsMonth;
    expect(codesFor(off, "no", "N")).toEqual(["N", "R"]);
    // The cell's own value stays selectable even when the list no longer has it.
    expect(codesFor(off, "no", "F")).toEqual(["N", "R", "F"]);
  });

  it("finds the folder for the back link", () => {
    expect(locationDir({ kind: "drive", folder: "A", path: "x/y/z.xlsx" })).toEqual({ folder: "A", path: "x/y" });
    expect(locationDir({ kind: "library" })).toBeNull();
  });
});
