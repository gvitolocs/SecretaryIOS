import { A, useBeforeLeave, useParams } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, Index, on, onCleanup, Show } from "solid-js";
import { type SheetError, sheetApi, toSheetError } from "../api/sheet";
import { BackIcon, DownloadIcon } from "../components/Icons";
import { t } from "../i18n";
import { folderHref } from "../lib/drive";
import {
  buildEdits,
  buildGrid,
  codesFor,
  dayCell,
  type DayColumn,
  defaultMonth,
  editKey,
  type GridModel,
  type GridSlot,
  isNegative,
  listAt,
  locationDir,
  type PendingEdit,
  rowSlots,
  rowWindow,
  sameText,
  type Sheet,
  type SheetVersions,
  styleCss,
  type Timesheet,
  type TsDay,
  type TsField,
  type TsMonth,
  visibleSheets,
  type Workbook,
} from "../lib/sheet";
import "../styles/drive.css";
import "../styles/sheet.css";

// An .xlsx from the Drive or the library (docs/xlsx.md): every sheet as a grid with the input cells
// marked, and the AU worktime template as a form per month. Edits stay here until Save; the server
// writes only those cells into the file and keeps the previous bytes as a version.

/** A cell the server refused. */
interface CellProblem {
  key: string;
  label: string;
  message: string;
}

type Pending = Record<string, PendingEdit>;

export default function SheetView() {
  const params = useParams<{ id: string }>();
  const [wb, setWb] = createSignal<Workbook | null>(null);
  const [loadError, setLoadError] = createSignal<SheetError | null>(null);
  const [pending, setPending] = createSignal<Pending>({});
  /** Edits kept aside by a reload after a conflict, until reapplied or dropped. */
  const [stashed, setStashed] = createSignal<Pending>({});
  const [mode, setMode] = createSignal<"form" | "grid">("form");
  const [saving, setSaving] = createSignal(false);
  const [note, setNote] = createSignal("");
  const [failure, setFailure] = createSignal("");
  const [conflict, setConflict] = createSignal(false);
  const [problem, setProblem] = createSignal<CellProblem | null>(null);
  const [versions, setVersions] = createSignal<SheetVersions | null>(null);
  const [showVersions, setShowVersions] = createSignal(false);

  const editCount = createMemo(() => buildEdits(Object.values(pending())).length);

  async function load() {
    setLoadError(null);
    try {
      setWb(await sheetApi.get(params.id));
    } catch (e) {
      setLoadError(toSheetError(e));
    }
  }
  createEffect(on(() => params.id, () => void load()));

  useBeforeLeave((e) => {
    if (editCount() > 0 && !e.defaultPrevented && !window.confirm(t("sheet.leaveConfirm"))) e.preventDefault();
  });
  const onUnload = (e: BeforeUnloadEvent) => {
    if (editCount() > 0) e.preventDefault();
  };
  window.addEventListener("beforeunload", onUnload);
  onCleanup(() => window.removeEventListener("beforeunload", onUnload));

  function setEdit(sheet: string, cell: string, text: string, original: string) {
    const key = editKey(sheet, cell);
    setPending((p) => {
      const next = { ...p };
      if (sameText(text, original)) delete next[key];
      else next[key] = { sheet, cell, text, original: p[key]?.original ?? original };
      return next;
    });
    setNote("");
    if (problem()?.key === key) setProblem(null);
  }

  async function save() {
    const book = wb();
    const edits = buildEdits(Object.values(pending()));
    if (!book || edits.length === 0 || saving()) return;
    setSaving(true);
    setFailure("");
    setProblem(null);
    setNote("");
    try {
      const next = await sheetApi.save(book.itemId, book.hash, edits);
      setWb(next);
      setPending({});
      setNote(t("sheet.savedCells", { count: next.changed.length }));
    } catch (e) {
      const err = toSheetError(e);
      if (err.kind === "conflict") setConflict(true);
      else if (err.sheet && err.cell) {
        setProblem({ key: editKey(err.sheet, err.cell), label: `${err.sheet}!${err.cell}`, message: err.message });
      } else setFailure(err.message);
    } finally {
      setSaving(false);
    }
  }

  /** After a conflict: the new file, with the edits set aside for a tap to reapply. */
  async function reload() {
    const kept = pending();
    setPending({});
    setConflict(false);
    await load();
    setStashed(kept);
  }

  function reapply() {
    setPending(stashed());
    setStashed({});
  }

  async function openVersions() {
    const show = !showVersions();
    setShowVersions(show);
    if (show && wb()) {
      try {
        setVersions(await sheetApi.versions(wb()!.itemId));
      } catch (e) {
        setFailure(toSheetError(e).message);
      }
    }
  }

  async function restore(version: number) {
    const book = wb();
    if (!book || !window.confirm(t("sheet.restoreConfirm", { version }))) return;
    try {
      setWb(await sheetApi.restore(book.itemId, version, book.hash));
      setPending({});
      setVersions(await sheetApi.versions(book.itemId));
      setNote(t("sheet.saved"));
    } catch (e) {
      const err = toSheetError(e);
      if (err.kind === "conflict") setConflict(true);
      else setFailure(err.message);
    }
  }

  const backHref = () => {
    const book = wb();
    const dir = book ? locationDir(book.location) : null;
    return dir ? folderHref(dir.folder, dir.path) : "/drive";
  };
  const hasForm = () => !!wb()?.timesheet;
  const stashCount = () => Object.keys(stashed()).length;

  return (
    <div class="sheet">
      <header class="sheet-bar">
        <A class="drive-btn" href={backHref()}>
          <BackIcon /> {t("sheet.back")}
        </A>
        <div class="sheet-bar__title">
          <span class="sheet-bar__name">{wb()?.name ?? ""}</span>
          <Show when={wb()?.version}>{(v) => <span class="sheet-bar__meta">{t("sheet.version", { version: v() })}</span>}</Show>
        </div>
        <div class="sheet-bar__actions">
          <Show when={wb()}>
            <Show when={wb()?.downloadUrl}>
              {(url) => (
                <a class="drive-btn" href={url()} download={wb()?.name}>
                  <DownloadIcon /> <span class="drive-btn__label">{t("sheet.download")}</span>
                </a>
              )}
            </Show>
            <button type="button" class="drive-btn" aria-expanded={showVersions()} onClick={() => void openVersions()}>
              {t("sheet.versions")}
            </button>
            <button type="button" class="drive-btn" disabled={editCount() === 0} onClick={() => setPending({})}>
              {t("sheet.discard")}
            </button>
            <button
              type="button"
              class="drive-btn sheet-save"
              disabled={editCount() === 0 || saving() || !wb()?.writable}
              onClick={() => void save()}
            >
              {saving() ? t("sheet.saving") : t("sheet.save")}
            </button>
          </Show>
        </div>
      </header>

      <Show when={loadError()}>
        {(e) => (
          <p class="drive-note" role="alert">
            {e().kind === "unsupported" ? t("sheet.unsupported") : t("sheet.loadFailed")}
          </p>
        )}
      </Show>
      <Show when={!wb() && !loadError()}>
        <p class="drive-note" role="status">
          {t("sheet.loading")}
        </p>
      </Show>

      <Show when={wb()}>
        {(book) => (
          <>
            <div class="sheet-status">
              <Show when={!book().writable}>
                <p class="sheet-msg sheet-msg--warn">{t("sheet.readOnly")}</p>
              </Show>
              <Show when={editCount() > 0}>
                <p class="sheet-msg" role="status">
                  {t("sheet.pending", { count: editCount() })}
                </p>
              </Show>
              <Show when={note()}>
                <p class="sheet-msg sheet-msg--ok" role="status">
                  {note()}
                </p>
              </Show>
              <Show when={failure()}>
                <p class="sheet-msg sheet-msg--bad" role="alert">
                  {t("sheet.saveFailed", { message: failure() })}
                </p>
              </Show>
              <Show when={problem()}>
                {(p) => (
                  <p class="sheet-msg sheet-msg--bad" role="alert">
                    {t("sheet.cellError", { cell: p().label, message: p().message })}
                  </p>
                )}
              </Show>
              <Show when={conflict()}>
                <p class="sheet-msg sheet-msg--bad" role="alert">
                  {t("sheet.conflict")}{" "}
                  <button type="button" class="drive-btn" onClick={() => void reload()}>
                    {t("sheet.reload")}
                  </button>
                </p>
              </Show>
              <Show when={stashCount() > 0}>
                <p class="sheet-msg" role="status">
                  {t("sheet.stashed", { count: stashCount() })}{" "}
                  <button type="button" class="drive-btn" onClick={reapply}>
                    {t("sheet.reapply")}
                  </button>{" "}
                  <button type="button" class="drive-btn" onClick={() => setStashed({})}>
                    {t("sheet.dropStashed")}
                  </button>
                </p>
              </Show>
            </div>

            <Show when={showVersions()}>
              <section class="sheet-versions" aria-label={t("sheet.versionsTitle")}>
                <h2>{t("sheet.versionsTitle")}</h2>
                <Show when={versions()?.versions.length} fallback={<p class="drive-note">{t("sheet.noVersions")}</p>}>
                  <ul>
                    <For each={versions()?.versions ?? []}>
                      {(v) => (
                        <li>
                          <span>{t("sheet.versionRow", { version: v.version })}</span>
                          <Show when={v.replacedAt}>
                            {(at) => <span class="sheet-bar__meta">{new Date(at()).toLocaleString()}</span>}
                          </Show>
                          <Show when={v.downloadUrl}>
                            {(url) => (
                              <a class="drive-btn" href={url()}>
                                {t("sheet.download")}
                              </a>
                            )}
                          </Show>
                          <button type="button" class="drive-btn" disabled={!book().writable} onClick={() => void restore(v.version)}>
                            {t("sheet.restore")}
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </section>
            </Show>

            <Show when={hasForm()}>
              <div class="drive-chips drive-chips--small" role="tablist" aria-label={t("sheet.mode")}>
                <button type="button" role="tab" class="media-chip" aria-selected={mode() === "form"} onClick={() => setMode("form")}>
                  {t("sheet.form")}
                </button>
                <button type="button" role="tab" class="media-chip" aria-selected={mode() === "grid"} onClick={() => setMode("grid")}>
                  {t("sheet.grid")}
                </button>
              </div>
            </Show>

            <Show
              when={hasForm() && mode() === "form" ? book().timesheet : null}
              fallback={<SheetGrid book={book()} pending={pending()} problem={problem()} onEdit={setEdit} />}
            >
              {(ts) => <TimesheetForm ts={ts()} pending={pending()} problem={problem()} onEdit={setEdit} />}
            </Show>
          </>
        )}
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------- the timesheet form

type EditFn = (sheet: string, cell: string, text: string, original: string) => void;

function TimesheetForm(props: { ts: Timesheet; pending: Pending; problem: CellProblem | null; onEdit: EditFn }) {
  const [monthNo, setMonthNo] = createSignal(defaultMonth(props.ts));
  const month = createMemo(() => props.ts.months.find((m) => m.month === monthNo()) ?? null);
  const monthName = (m: TsMonth) => new Date(Date.UTC(m.year || 2000, m.month - 1, 1)).toLocaleDateString(undefined, { month: "long", timeZone: "UTC" });
  return (
    <div class="ts">
      <div class="drive-chips drive-chips--small ts-months" role="tablist" aria-label={t("sheet.months")}>
        <For each={props.ts.months}>
          {(m) => (
            <button type="button" role="tab" class="media-chip" aria-selected={m.month === monthNo()} onClick={() => setMonthNo(m.month)}>
              {monthName(m)}
            </button>
          )}
        </For>
      </div>

      <details class="ts-card">
        <summary>{t("sheet.details")}</summary>
        <div class="ts-fields">
          <For each={props.ts.master}>{(f) => <FieldInput field={f} pending={props.pending} problem={props.problem} onEdit={props.onEdit} />}</For>
        </div>
      </details>

      <Show when={month()} fallback={<p class="drive-note">{t("sheet.noMonth")}</p>}>
        {(m) => (
          <>
            <section class="ts-card" aria-label={t("sheet.monthSettings")}>
              <div class="ts-fields">
                <Show when={m().weeklyHours}>{(f) => <FieldInput field={f()} pending={props.pending} problem={props.problem} onEdit={props.onEdit} />}</Show>
                <Show when={m().dayOff}>{(f) => <FieldInput field={f()} pending={props.pending} problem={props.problem} onEdit={props.onEdit} />}</Show>
              </div>
            </section>

            <div class="ts-days" role="table" aria-label={monthName(m())}>
              <div class="ts-day ts-day--head" role="row">
                <span role="columnheader">{t("sheet.date")}</span>
                <span role="columnheader">{t("sheet.code")}</span>
                <span role="columnheader">{t("sheet.start")}</span>
                <span role="columnheader">{t("sheet.end")}</span>
                <span role="columnheader">{t("sheet.actual")}</span>
                <span role="columnheader">{t("sheet.remarks")}</span>
                <span role="columnheader">{t("sheet.norm")}</span>
                <span role="columnheader">{t("sheet.worked")}</span>
                <span role="columnheader">{t("sheet.diff")}</span>
              </div>
              <For each={m().days}>
                {(day) => <DayRow month={m()} day={day} pending={props.pending} problem={props.problem} onEdit={props.onEdit} />}
              </For>
            </div>

            <section class="ts-card ts-totals" aria-label={t("sheet.monthTotals")}>
              <dl>
                <div>
                  <dt>{t("sheet.norm")}</dt>
                  <dd>{m().norm}</dd>
                </div>
                <div>
                  <dt>{t("sheet.worked")}</dt>
                  <dd>{m().worked}</dd>
                </div>
                <div>
                  <dt>{t("sheet.diff")}</dt>
                  <dd classList={{ "ts-neg": isNegative(m().diff) }}>{m().diff}</dd>
                </div>
                <div>
                  <dt>{t("sheet.balanceCarried")}</dt>
                  <dd>{props.ts.balanceCarried}</dd>
                </div>
                <div>
                  <dt>{t("sheet.yearTotal")}</dt>
                  <dd classList={{ "ts-neg": isNegative(props.ts.total) }}>{props.ts.total}</dd>
                </div>
              </dl>
              <p class="ts-note">{t("sheet.registeredDays", { count: m().registeredDays })}</p>
              <p class="ts-note">{t("sheet.totalsNote")}</p>
            </section>
          </>
        )}
      </Show>
    </div>
  );
}

function FieldInput(props: { field: TsField; pending: Pending; problem: CellProblem | null; onEdit: EditFn }) {
  const f = () => props.field;
  const key = () => editKey(f().sheet, f().cell);
  const text = () => props.pending[key()]?.text ?? f().value;
  const set = (v: string) => props.onEdit(f().sheet, f().cell, v, f().value);
  return (
    <label class="ts-field" classList={{ "ts-bad": props.problem?.key === key(), "ts-edited": !!props.pending[key()] }}>
      <span class="ts-field__label">{f().label}</span>
      <Show
        when={f().kind === "bool"}
        fallback={
          <input
            type={f().kind === "date" ? "date" : "text"}
            inputmode={f().kind === "number" ? "decimal" : undefined}
            value={text()}
            disabled={!f().editable}
            onChange={(e) => set(e.currentTarget.value)}
          />
        }
      >
        <input type="checkbox" checked={text() === "yes"} disabled={!f().editable} onChange={(e) => set(e.currentTarget.checked ? "yes" : "no")} />
      </Show>
    </label>
  );
}

function DayRow(props: { month: TsMonth; day: TsDay; pending: Pending; problem: CellProblem | null; onEdit: EditFn }) {
  const cell = (column: DayColumn) => dayCell(props.month, column, props.day.row);
  const original = (column: DayColumn) => props.day[column];
  const value = (column: DayColumn) => {
    const c = cell(column);
    return (c && props.pending[editKey(props.month.sheet, c)]?.text) ?? original(column);
  };
  const set = (column: DayColumn, text: string) => {
    const c = cell(column);
    if (c) props.onEdit(props.month.sheet, c, text, original(column));
  };
  const flagged = (column: DayColumn) => {
    const c = cell(column);
    if (!c) return {};
    const key = editKey(props.month.sheet, c);
    return { "ts-bad": props.problem?.key === key, "ts-edited": !!props.pending[key] };
  };
  // The day-off answer the owner has typed but not saved changes which codes are allowed.
  const codes = () => {
    const off = props.month.dayOff;
    const typed = off ? props.pending[editKey(off.sheet, off.cell)]?.text : undefined;
    return codesFor(props.month, typed, value("code"));
  };
  const date = () => {
    const d = new Date(`${props.day.date}T00:00:00Z`);
    return Number.isNaN(d.getTime()) ? props.day.date : d.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short", timeZone: "UTC" });
  };
  // Times and remarks only count on an R day: narrow screens hide them on the others while empty.
  const plain = () => value("code").toUpperCase() !== "R" && (["start", "end", "actual", "remarks"] as const).every((c) => !value(c));
  const time = (column: DayColumn, label: string) => (
    <input
      class="ts-time"
      classList={flagged(column)}
      type="text"
      inputmode="numeric"
      autocomplete="off"
      placeholder={t("sheet.timePlaceholder")}
      aria-label={`${label} ${props.day.date}`}
      value={value(column)}
      onChange={(e) => set(column, e.currentTarget.value)}
    />
  );
  return (
    <div class="ts-day" role="row" classList={{ "ts-day--off": !props.day.workday, "ts-day--out": !props.day.employed, "ts-day--plain": plain() }}>
      <span class="ts-day__date" role="cell">
        {date()}
        <Show when={props.day.holiday}>{(h) => <span class="ts-day__holiday">{h()}</span>}</Show>
        <Show when={!props.day.employed}>
          <span class="ts-day__holiday">{t("sheet.notEmployed")}</span>
        </Show>
      </span>
      <span role="cell" data-label={t("sheet.code")}>
        <select class="ts-code" classList={flagged("code")} aria-label={`${t("sheet.code")} ${props.day.date}`} value={value("code")} onChange={(e) => set("code", e.currentTarget.value)}>
          <For each={codes()}>{(c) => <option value={c} selected={c === value("code")}>{c}</option>}</For>
        </select>
      </span>
      <span role="cell" class="ts-day__entry" data-label={t("sheet.start")}>
        {time("start", t("sheet.start"))}
      </span>
      <span role="cell" class="ts-day__entry" data-label={t("sheet.end")}>
        {time("end", t("sheet.end"))}
      </span>
      <span role="cell" class="ts-day__entry" data-label={t("sheet.actual")}>
        {time("actual", t("sheet.actual"))}
      </span>
      <span role="cell" class="ts-day__remarks ts-day__entry" data-label={t("sheet.remarks")}>
        <input
          type="text"
          classList={flagged("remarks")}
          aria-label={`${t("sheet.remarks")} ${props.day.date}`}
          value={value("remarks")}
          onChange={(e) => set("remarks", e.currentTarget.value)}
        />
      </span>
      <span role="cell" class="ts-num" data-label={t("sheet.norm")}>
        {props.day.norm}
      </span>
      <span role="cell" class="ts-num" data-label={t("sheet.worked")}>
        {props.day.worked}
      </span>
      <span role="cell" class="ts-num" classList={{ "ts-neg": isNegative(props.day.diff) }} data-label={t("sheet.diff")}>
        {props.day.diff}
      </span>
    </div>
  );
}

// ---------------------------------------------------------------- the grid

function SheetGrid(props: { book: Workbook; pending: Pending; problem: CellProblem | null; onEdit: EditFn }) {
  const sheets = createMemo(() => visibleSheets(props.book));
  const [sheetName, setSheetName] = createSignal<string | null>(null);
  const sheet = createMemo<Sheet | null>(() => sheets().find((s) => s.name === sheetName()) ?? sheets()[0] ?? null);
  const grid = createMemo(() => {
    const s = sheet();
    return s ? buildGrid(s) : null;
  });
  const [selected, setSelected] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal<string | null>(null);
  const [scroll, setScroll] = createSignal({ top: 0, height: 600 });
  let box: HTMLDivElement | undefined;

  const measure = () => {
    if (box) setScroll({ top: box.scrollTop, height: box.clientHeight || 600 });
  };

  createEffect(on(sheet, () => {
    setSelected(null);
    setEditing(null);
    if (box) box.scrollTop = 0;
    measure();
  }));

  const frozen = () => grid()?.frozenRows ?? 0;
  const windowed = createMemo<[number, number]>(() => {
    const g = grid();
    return g ? rowWindow(g, frozen(), scroll().top, scroll().height) : [0, -1];
  });
  const topSpacer = () => {
    const g = grid();
    if (!g) return 0;
    const first = g.rows[windowed()[0]];
    const base = g.rows[frozen()];
    return first && base ? first.top - base.top : 0;
  };
  const bottomSpacer = () => {
    const g = grid();
    if (!g) return 0;
    const last = g.rows[windowed()[1]];
    return last ? g.height - last.top - last.px : 0;
  };
  const indexes = (from: number, to: number) => Array.from({ length: Math.max(0, to - from + 1) }, (_, i) => from + i);

  const canEdit = (slot: GridSlot) => {
    const s = sheet();
    if (!s || !props.book.writable) return false;
    if (slot.cell) return slot.cell.editable;
    return !s.protected;
  };

  /** The next editable cell after `ref`, in reading order, for Tab and Enter. */
  function moveFrom(g: GridModel, ref: string, down: boolean) {
    const at = g.sheet.cells.find((c) => c.ref === ref);
    if (!at) return setEditing(null);
    const next = down
      ? g.sheet.cells.filter((c) => c.editable && c.c === at.c && c.r > at.r).sort((a, b) => a.r - b.r)[0]
      : g.sheet.cells.filter((c) => c.editable && ((c.r === at.r && c.c > at.c) || c.r > at.r)).sort((a, b) => a.r - b.r || a.c - b.c)[0];
    setSelected(next?.ref ?? ref);
    setEditing(next?.ref ?? null);
  }

  const selectedCell = () => {
    const g = grid();
    const ref = selected();
    return g && ref ? g.sheet.cells.find((c) => c.ref === ref) : undefined;
  };

  const Row = (rowProps: { index: number; from: number; to: number; head?: boolean }) => {
    const g = () => grid()!;
    const row = () => g().rows[rowProps.index]!;
    return (
      <tr style={{ height: `${row().px}px` }} class={rowProps.head ? "sheet-frozen-row" : undefined}>
        <th class="sheet-rowno" scope="row">
          {row().r}
        </th>
        <For each={rowSlots(g(), rowProps.index, rowProps.from, rowProps.to)}>
          {(slot) => <Cell slot={slot} sheet={g().sheet} />}
        </For>
      </tr>
    );
  };

  const Cell = (cellProps: { slot: GridSlot; sheet: Sheet }) => {
    const slot = cellProps.slot;
    const key = editKey(cellProps.sheet.name, slot.ref);
    const edit = () => props.pending[key];
    const original = slot.cell?.display ?? "";
    const style = () => (slot.cell ? props.book.styles[slot.cell.s] : undefined);
    const numeric = slot.cell?.t === "n";
    const editable = canEdit(slot);
    const list = listAt(cellProps.sheet, slot.r, slot.c);
    const commit = (text: string) => {
      props.onEdit(cellProps.sheet.name, slot.ref, text, original);
    };
    return (
      <td
        rowSpan={slot.rowSpan > 1 ? slot.rowSpan : undefined}
        colSpan={slot.colSpan > 1 ? slot.colSpan : undefined}
        style={styleCss(style(), numeric)}
        class="sheet-cell"
        classList={{
          "sheet-cell--input": !!slot.cell?.input,
          "sheet-cell--pending": !!edit(),
          "sheet-cell--bad": props.problem?.key === key,
          "sheet-cell--selected": selected() === slot.ref,
          "sheet-cell--formula": slot.cell?.f != null,
        }}
        data-ref={slot.ref}
        onClick={() => {
          setSelected(slot.ref);
          if (editable) setEditing(slot.ref);
        }}
      >
        <Show when={editing() === slot.ref && editable} fallback={edit()?.text ?? original}>
          <Show
            when={list}
            fallback={
              <input
                class="sheet-editor"
                ref={(el) => queueMicrotask(() => el.focus())}
                value={edit()?.text ?? original}
                aria-label={slot.ref}
                onBlur={(e) => {
                  commit(e.currentTarget.value);
                  setEditing((cur) => (cur === slot.ref ? null : cur));
                }}
                onKeyDown={(e) => {
                  if (e.key === "Escape") {
                    e.preventDefault();
                    e.currentTarget.value = edit()?.text ?? original;
                    setEditing(null);
                  } else if (e.key === "Enter" || e.key === "Tab") {
                    e.preventDefault();
                    commit(e.currentTarget.value);
                    moveFrom(grid()!, slot.ref, e.key === "Enter");
                  }
                }}
              />
            }
          >
            {(choices) => (
              <select
                class="sheet-editor"
                ref={(el) => queueMicrotask(() => el.focus())}
                aria-label={slot.ref}
                value={edit()?.text ?? original}
                onChange={(e) => {
                  commit(e.currentTarget.value);
                  setEditing(null);
                }}
                onBlur={() => setEditing((cur) => (cur === slot.ref ? null : cur))}
                onKeyDown={(e) => {
                  if (e.key === "Escape") setEditing(null);
                }}
              >
                <option value="" />
                <For each={choices()}>{(c) => <option value={c} selected={c === (edit()?.text ?? original)}>{c}</option>}</For>
              </select>
            )}
          </Show>
        </Show>
      </td>
    );
  };

  return (
    <div class="sheet-gridwrap">
      <div class="drive-chips drive-chips--small" role="tablist" aria-label={t("sheet.sheets")}>
        <For each={sheets()}>
          {(s) => (
            <button type="button" role="tab" class="media-chip" aria-selected={s.name === sheet()?.name} onClick={() => setSheetName(s.name)}>
              {s.name}
            </button>
          )}
        </For>
      </div>
      <div class="sheet-infobar">
        <Show when={selected()} fallback={<span class="sheet-legend"><i class="sheet-swatch sheet-swatch--input" /> {t("sheet.legendInput")} <i class="sheet-swatch sheet-swatch--pending" /> {t("sheet.legendPending")}</span>}>
          {(ref) => (
            <>
              <strong>{ref()}</strong>
              <Show
                when={selectedCell()?.f}
                fallback={<span>{selectedCell()?.editable ? "" : t("sheet.locked")}</span>}
              >
                {(f) => <code title={t("sheet.formulaReadOnly")}>ƒ {f()}</code>}
              </Show>
            </>
          )}
        </Show>
      </div>
      <Show when={grid()}>
        {(g) => (
          <div class="sheet-scroll" ref={box} onScroll={measure}>
            <table class="sheet-table" style={{ width: `${g().cols.reduce((a, c) => a + c.px, 44)}px` }}>
              <colgroup>
                <col style={{ width: "44px" }} />
                <Index each={g().cols}>{(c) => <col style={{ width: `${c().px}px` }} />}</Index>
              </colgroup>
              <thead>
                <tr>
                  <th class="sheet-corner" />
                  <Index each={g().cols}>{(c) => <th class="sheet-colname" scope="col">{c().name}</th>}</Index>
                </tr>
                <For each={indexes(0, frozen() - 1)}>{(i) => <Row index={i} from={0} to={frozen() - 1} head />}</For>
              </thead>
              <tbody>
                <Show when={topSpacer() > 0}>
                  <tr style={{ height: `${topSpacer()}px` }} aria-hidden="true" />
                </Show>
                <For each={indexes(windowed()[0], windowed()[1])}>{(i) => <Row index={i} from={windowed()[0]} to={windowed()[1]} />}</For>
                <Show when={bottomSpacer() > 0}>
                  <tr style={{ height: `${bottomSpacer()}px` }} aria-hidden="true" />
                </Show>
              </tbody>
            </table>
          </div>
        )}
      </Show>
      <Show when={grid()?.clipped}>
        <p class="drive-note">{t("sheet.clipped", { rows: grid()!.rows.length, cols: grid()!.cols.length })}</p>
      </Show>
      <Show when={sheet()?.truncated}>{(n) => <p class="drive-note">{t("sheet.truncated", { count: n() })}</p>}</Show>
    </div>
  );
}
