import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { type Call, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import SheetView from "./Sheet";

function field(key: string, label: string, sheet: string, cell: string, kind: string, value: string, editable = true) {
  return { key, label, sheet, cell, kind, value, editable };
}

function day(row: number, date: string, weekday: string, over: Record<string, unknown> = {}) {
  return { row, date, weekday, holiday: null, workday: true, employed: true, code: "N", actual: "", start: "", end: "", remarks: "", norm: "07:24", worked: "07:24", diff: "00:00", ...over };
}

function month(over: Record<string, unknown> = {}) {
  return {
    sheet: "November",
    month: 11,
    year: 2026,
    weekly_hours: field("weekly_hours", "Standard work hours", "November", "B1", "number", "37"),
    day_off: field("day_off", "Do you have a weekly day off?", "November", "B2", "bool", "no"),
    columns: { code: "F", actual: "I", start: "J", end: "K", remarks: "S" },
    codes: ["N", "R"],
    days: [day(9, "2026-11-01", "Sunday", { workday: false, norm: "00:00", worked: "00:00" }), day(10, "2026-11-02", "Monday")],
    norm: "155:24",
    worked: "155:24",
    diff: "00:00",
    registered_days: 0,
    ...over,
  };
}

const STYLES = [
  { fmt: "General", kind: "general", border: [false, false, false, false] },
  { fmt: "h:mm", kind: "time", border: [true, true, true, true] },
];

function workbook(over: Record<string, unknown> = {}) {
  return {
    item_id: "itm_1",
    name: "worktime.xlsx",
    hash: "h1",
    version: 1,
    writable: true,
    location: { kind: "drive", folder: "CraftIT", path: "worktime.xlsx" },
    download_url: "/v1/media/items/itm_1/original?download=1",
    full_calc_on_load: false,
    styles: STYLES,
    sheets: [
      {
        index: 0,
        name: "November",
        state: "visible",
        protected: true,
        max_row: 10,
        max_col: 13,
        frozen: null,
        cols: [{ min: 4, max: 5, width: 15, hidden: true }],
        rows: [],
        merges: [],
        validations: [{ sqref: "F9:F38", type: "list", operator: "between", strict: true, list: ["N", "R"] }],
        cells: [
          { ref: "A8", r: 8, c: 1, s: 0, t: "s", v: "Week", display: "Week" },
          { ref: "F10", r: 10, c: 6, s: 0, t: "s", v: "N", display: "N", editable: true, input: true },
          { ref: "J10", r: 10, c: 10, s: 1, t: "n", v: 0, display: "0:00", editable: true, input: true },
          { ref: "M10", r: 10, c: 13, s: 0, t: "s", v: "0:00", display: "0:00", f: "IF(F10=\"N\",\"0:00\",K10-J10)" },
          { ref: "D10", r: 10, c: 4, s: 0, t: "n", v: 2, display: "2" },
        ],
      },
      { index: 1, name: "Hidden", state: "hidden", protected: false, max_row: 1, max_col: 1, cols: [], rows: [], merges: [], validations: [], cells: [] },
    ],
    timesheet: {
      kind: "au-worktime",
      master_sheet: "Masterdata",
      master: [field("au_id", "Enter your AU ID", "Masterdata", "B7", "text", "")],
      months: [month()],
      balance_carried: "00:00",
      diff: "00:00",
      total: "00:00",
    },
    ...over,
  };
}

function open(handler: (call: Call) => { status?: number; body?: unknown }) {
  const calls = stubFetch(handler);
  renderAt("/drive/sheet/itm_1", () => <Route path="/drive/sheet/:id" component={SheetView} />);
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Sheet form", () => {
  it("sends exactly the changed fields, then shows the server's new totals", async () => {
    const calls = open((call) => {
      if (call.method === "POST") {
        return {
          body: workbook({
            hash: "h2",
            version: 2,
            changed: [{ sheet: "November", cell: "F10", before: "N", after: "R" }, { sheet: "November", cell: "J10", before: "", after: "9:00" }],
            timesheet: { ...workbook().timesheet, months: [month({ diff: "-03:54", worked: "151:30", registered_days: 1 })], total: "-03:54", diff: "-03:54" },
          }),
        };
      }
      return { body: workbook() };
    });
    const code = await screen.findByLabelText("Code 2026-11-02");
    fireEvent.change(code, { target: { value: "R" } });
    fireEvent.change(screen.getByLabelText("Start 2026-11-02"), { target: { value: "9:00" } });
    // Typing the same zero back is no edit.
    fireEvent.change(screen.getByLabelText("End 2026-11-02"), { target: { value: "00:00" } });
    expect(screen.getByText("2 changes not saved")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    const post = calls.find((c) => c.method === "POST")!;
    expect(post.url).toBe("/v1/media/items/itm_1/sheet");
    expect(post.body).toEqual({
      base_hash: "h1",
      edits: [
        { sheet: "November", cell: "F10", value: { t: "input", v: "R" } },
        { sheet: "November", cell: "J10", value: { t: "input", v: "9:00" } },
      ],
    });
    expect(await screen.findByText("Saved, 2 cells changed.")).toBeInTheDocument();
    expect(screen.getAllByText("-03:54").length).toBeGreaterThan(0);
    expect(screen.queryByText(/not saved/)).toBeNull();
  });

  it("offers F once the owner says he has a weekly day off", async () => {
    open(() => ({ body: workbook() }));
    const code = (await screen.findByLabelText("Code 2026-11-02")) as HTMLSelectElement;
    expect(within(code).queryByRole("option", { name: "F" })).toBeNull();
    fireEvent.click(screen.getByLabelText("Do you have a weekly day off?"));
    expect(within(code).getByRole("option", { name: "F" })).toBeInTheDocument();
  });

  it("marks the field the server refused and writes nothing", async () => {
    open((call) =>
      call.method === "POST"
        ? { status: 400, body: { error: "invalid_value", message: "expected a time as hh:mm", sheet: "November", cell: "J10" } }
        : { body: workbook() },
    );
    const start = await screen.findByLabelText("Start 2026-11-02");
    fireEvent.change(start, { target: { value: "soon" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("November!J10: expected a time as hh:mm");
    expect(start).toHaveClass("ts-bad");
    // The edit is kept so it can be corrected.
    expect(screen.getByText("1 change not saved")).toBeInTheDocument();
  });

  it("on a conflict reloads and reapplies the kept edits with one tap", async () => {
    let gets = 0;
    const calls = open((call) => {
      if (call.method === "POST") return { status: 409, body: { error: "conflict", message: "changed", hash: "h9" } };
      gets += 1;
      return { body: workbook({ hash: gets > 1 ? "h9" : "h1" }) };
    });
    fireEvent.change(await screen.findByLabelText("Start 2026-11-02"), { target: { value: "8:00" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    fireEvent.click(await screen.findByRole("button", { name: "Reload" }));
    fireEvent.click(await screen.findByRole("button", { name: "Reapply" }));
    expect(screen.getByText("1 change not saved")).toBeInTheDocument();
    calls.length = 0;
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls[0]?.body).toMatchObject({ base_hash: "h9" }));
  });
});

describe("Sheet grid", () => {
  async function grid() {
    fireEvent.click(await screen.findByRole("tab", { name: "Grid" }));
  }

  it("shows visible sheets, skips hidden columns, marks inputs and shows formulas read-only", async () => {
    open(() => ({ body: workbook() }));
    await grid();
    expect(screen.getByRole("tab", { name: "November" })).toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Hidden" })).toBeNull();
    const cell = (ref: string) => document.querySelector(`[data-ref="${ref}"]`) as HTMLElement | null;
    expect(cell("F10")).toHaveClass("sheet-cell--input");
    expect(cell("D10")).toBeNull();
    fireEvent.click(cell("M10")!);
    expect(screen.getByText(/ƒ IF\(F10/)).toBeInTheDocument();
    expect(cell("M10")!.querySelector("input")).toBeNull();
  });

  it("edits a cell in place and uses a select where a list validation applies", async () => {
    const calls = open((call) => (call.method === "POST" ? { body: workbook({ hash: "h2" }) } : { body: workbook() }));
    await grid();
    const cell = (ref: string) => document.querySelector(`[data-ref="${ref}"]`) as HTMLElement;
    fireEvent.click(cell("F10"));
    const select = cell("F10").querySelector("select")!;
    expect(within(select).getByRole("option", { name: "R" })).toBeInTheDocument();
    fireEvent.change(select, { target: { value: "R" } });
    expect(cell("F10")).toHaveClass("sheet-cell--pending");
    fireEvent.click(cell("J10"));
    const input = cell("J10").querySelector("input")!;
    fireEvent.input(input, { target: { value: "9:30" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(cell("J10")).not.toHaveClass("sheet-cell--pending");
    fireEvent.click(cell("J10"));
    const again = cell("J10").querySelector("input")!;
    again.value = "9:30";
    fireEvent.keyDown(again, { key: "Tab" });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.method === "POST")!.body).toEqual({
      base_hash: "h1",
      edits: [
        { sheet: "November", cell: "F10", value: { t: "input", v: "R" } },
        { sheet: "November", cell: "J10", value: { t: "input", v: "9:30" } },
      ],
    });
  });

  it("a workbook without a timesheet opens straight in the grid", async () => {
    open(() => ({ body: workbook({ timesheet: null }) }));
    expect(await screen.findByRole("tab", { name: "November" })).toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Form" })).toBeNull();
  });
});

describe("Sheet versions and errors", () => {
  it("lists versions and restores after asking", async () => {
    vi.stubGlobal("confirm", vi.fn(() => true));
    const calls = open((call) => {
      if (call.url.endsWith("/sheet/versions")) {
        return {
          body: {
            current: { version: 2, hash: "h1", bytes: 10 },
            versions: [{ version: 1, hash: "h0", replaced_at: "2026-11-01T10:00:00Z", bytes: 9, download_url: "/v1/media/items/itm_1/original?version=1&download=1" }],
          },
        };
      }
      if (call.url.endsWith("/sheet/restore")) return { body: workbook({ hash: "h3", version: 3 }) };
      return { body: workbook({ version: 2 }) };
    });
    fireEvent.click(await screen.findByRole("button", { name: "Versions" }));
    const restore = await screen.findByRole("button", { name: "Restore" });
    const hrefs = screen.getAllByRole("link", { name: "Download" }).map((a) => a.getAttribute("href"));
    expect(hrefs).toContain("/v1/media/items/itm_1/original?version=1&download=1");
    fireEvent.click(restore);
    await waitFor(() => expect(calls.some((c) => c.url.endsWith("/sheet/restore"))).toBe(true));
    expect(calls.find((c) => c.url.endsWith("/sheet/restore"))!.body).toEqual({ version: 1, base_hash: "h1" });
    expect(window.confirm).toHaveBeenCalled();
  });

  it("says when a file only previews", async () => {
    open(() => ({ status: 415, body: { error: "unsupported", message: "only .xlsx" } }));
    expect(await screen.findByRole("alert")).toHaveTextContent("can only be previewed");
  });

  it("a read-only file cannot be saved", async () => {
    open(() => ({ body: workbook({ writable: false }) }));
    expect(await screen.findByText(/Read-only/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });
});
