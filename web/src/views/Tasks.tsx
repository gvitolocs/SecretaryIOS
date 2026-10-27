import { A, useNavigate, useParams, useSearchParams } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, type JSX, on, onCleanup, onMount, Show } from "solid-js";
import type { Group, Parsed, ProjectView, TaskCommand, TaskView, TasksView, ViewQuery } from "../api/tasks";
import {
  CheckCircleIcon,
  CloseIcon,
  EditIcon,
  FiltersIcon,
  FlagIcon,
  HashIcon,
  InboxIcon,
  MenuIcon,
  PlusIcon,
  RefreshIcon,
  RepeatIcon,
  SearchIcon,
  SubtaskIcon,
  TagIcon,
  TodayIcon,
  TrashIcon,
  UpcomingIcon,
  CalendarIcon,
} from "../components/Icons";
import { locale, t } from "../i18n";
import {
  actionError,
  hiddenIds,
  load,
  loadError,
  outbox,
  parseQuick,
  query,
  run,
  setActionError,
  showToast,
  startTasks,
  syncing,
  syncNow,
  toast,
  view,
  waitingAdds,
} from "../state/tasks";
import "../styles/tasks.css";

// Tasks: Todoist's layout (sidebar with Add task, Search, Inbox, Today,
// Upcoming, Filters & Labels, Completed and My Projects; Today with an
// Overdue group and a Reschedule action; round priority checkboxes, date
// chips with the repeat icon), in the Nebula/Pearl design system. The
// engine computes every list, date and count (docs/server-api.md, "Tasks").
// Routes: /tasks (Today), /tasks/inbox, /tasks/upcoming, /tasks/completed,
// /tasks/filters, /tasks/search?q=, /tasks/project/:id, /tasks/label/:name,
// /tasks/filter/:id.

const SEARCH_DEBOUNCE_MS = 250;
const PARSE_DEBOUNCE_MS = 180;

// ---------------------------------------------------------------- dates

function parseDay(d: string): Date {
  const [y, m, day] = d.split("-").map(Number);
  return new Date(y ?? 1970, (m ?? 1) - 1, day ?? 1);
}

function isoDay(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function addDays(d: string, n: number): string {
  const x = parseDay(d);
  x.setDate(x.getDate() + n);
  return isoDay(x);
}

function weekday(d: string, style: "long" | "short" = "long"): string {
  return parseDay(d).toLocaleDateString(locale(), { weekday: style });
}

function dayMonth(d: string, today?: string): string {
  const x = parseDay(d);
  const sameYear = !today || today.slice(0, 4) === d.slice(0, 4);
  return x.toLocaleDateString(locale(), { day: "numeric", month: "short", ...(sameYear ? {} : { year: "numeric" }) });
}

function relWord(rel: number): string | null {
  if (rel === 0) return t("tasks.today");
  if (rel === 1) return t("tasks.tomorrow");
  if (rel === -1) return t("tasks.yesterday");
  return null;
}

/** "Today", "Tomorrow", "Monday", "27 Oct" (+ time). */
export function dueLabel(due: NonNullable<TaskView["due"]>, today: string): string {
  const rel = due.rel_days;
  let s = relWord(rel) ?? (rel > 1 && rel < 7 ? weekday(due.date) : dayMonth(due.date, today));
  if (due.time) s += ` ${due.time}`;
  return s;
}

function dueTone(rel: number): string {
  if (rel < 0) return "overdue";
  if (rel === 0) return "today";
  if (rel === 1) return "tomorrow";
  if (rel < 7) return "week";
  return "later";
}

/** "22 Oct · Today · Thursday" like Todoist's date headers. */
function dateHeader(g: Group, today: string): string {
  if (!g.date) return g.title;
  const parts = [dayMonth(g.date, today)];
  const rel = relWord(g.rel_days ?? 99);
  if (rel) parts.push(rel);
  parts.push(weekday(g.date));
  return parts.join(" · ");
}

function nextWeekday(today: string, target: number): string {
  const d = parseDay(today);
  const diff = (target - d.getDay() + 7) % 7 || 7;
  d.setDate(d.getDate() + diff);
  return isoDay(d);
}

function relativeTime(ms: number): string {
  const diff = (ms - Date.now()) / 1000;
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: "auto" });
  if (Math.abs(diff) < 45) return rtf.format(0, "second");
  if (Math.abs(diff) < 3600) return rtf.format(Math.round(diff / 60), "minute");
  if (Math.abs(diff) < 86_400) return rtf.format(Math.round(diff / 3600), "hour");
  return rtf.format(Math.round(diff / 86_400), "day");
}

// ---------------------------------------------------------------- routes

function queryFor(section: string | undefined, id: string | undefined, q: string | undefined): ViewQuery {
  switch (section) {
    case "inbox":
      return { kind: "inbox" };
    case "upcoming":
      return { kind: "upcoming" };
    case "completed":
      return { kind: "completed" };
    case "search":
      return { kind: "search", query: q ?? "" };
    case "project":
      return { kind: "project", id };
    case "label":
      return { kind: "label", id };
    case "filter":
      return { kind: "filter", id };
    default:
      return { kind: "today" };
  }
}

// ---------------------------------------------------------------- small parts

function Checkbox(props: { task: TaskView; onToggle: () => void }) {
  return (
    <button
      type="button"
      class={`task-check task-check--p${props.task.priority}`}
      classList={{ "task-check--done": props.task.checked }}
      aria-label={props.task.checked ? t("tasks.reopen") : t("tasks.complete")}
      title={props.task.checked ? t("tasks.reopen") : t("tasks.complete")}
      onClick={(e) => {
        e.stopPropagation();
        props.onToggle();
      }}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="m7.5 12.4 3 3 6-6.4" />
      </svg>
    </button>
  );
}

function DueChip(props: { task: TaskView; today: string }) {
  return (
    <Show when={props.task.due}>
      {(d) => (
        <span
          class={`due-chip due-chip--${dueTone(d().rel_days)}`}
          classList={{ "due-chip--guess": !!d().provisional }}
          title={d().provisional ? t("tasks.dateGuess") : d().is_recurring ? t("tasks.repeats", { rule: d().string }) : d().string}
        >
          <CalendarIcon />
          <span>{dueLabel(d(), props.today)}</span>
          <Show when={d().is_recurring}>
            <RepeatIcon />
          </Show>
        </span>
      )}
    </Show>
  );
}

function projectColor(color: string): string {
  return color || "charcoal";
}

// ---------------------------------------------------------------- date picker

function DatePicker(props: {
  today: string;
  title?: string;
  allowClear?: boolean;
  onPick: (date: string | null) => void;
  onClose: () => void;
}) {
  const [custom, setCustom] = createSignal(props.today);
  const options = createMemo(() => [
    { key: "today", label: t("tasks.today"), date: props.today, icon: <TodayIcon /> },
    { key: "tomorrow", label: t("tasks.tomorrow"), date: addDays(props.today, 1), icon: <CalendarIcon /> },
    { key: "weekend", label: t("tasks.thisWeekend"), date: nextWeekday(addDays(props.today, -1), 6), icon: <CalendarIcon /> },
    { key: "nextweek", label: t("tasks.nextWeek"), date: nextWeekday(props.today, 1), icon: <UpcomingIcon /> },
  ]);
  return (
    <div class="tasks-backdrop" onClick={props.onClose}>
      <div
        class="date-picker"
        role="dialog"
        aria-modal="true"
        aria-label={props.title ?? t("tasks.pickDate")}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.key === "Escape" && props.onClose()}
      >
        <Show when={props.title}>
          <h2 class="date-picker-title">{props.title}</h2>
        </Show>
        <ul class="date-options">
          <For each={options()}>
            {(o) => (
              <li>
                <button type="button" class="date-option" data-key={o.key} onClick={() => props.onPick(o.date)}>
                  <span class={`date-option-icon date-option-icon--${o.key}`}>{o.icon}</span>
                  <span class="date-option-label">{o.label}</span>
                  <span class="date-option-day">
                    {weekday(o.date, "short")} {dayMonth(o.date, props.today)}
                  </span>
                </button>
              </li>
            )}
          </For>
          <Show when={props.allowClear}>
            <li>
              <button type="button" class="date-option" data-key="none" onClick={() => props.onPick(null)}>
                <span class="date-option-icon date-option-icon--none">
                  <CloseIcon />
                </span>
                <span class="date-option-label">{t("tasks.noDate")}</span>
              </button>
            </li>
          </Show>
        </ul>
        <form
          class="date-custom"
          onSubmit={(e) => {
            e.preventDefault();
            if (custom()) props.onPick(custom());
          }}
        >
          <label class="field-label" for="task-date-custom">
            {t("tasks.pickDate")}
          </label>
          <div class="date-custom-row">
            <input
              id="task-date-custom"
              class="text-input"
              type="date"
              value={custom()}
              onInput={(e) => setCustom(e.currentTarget.value)}
            />
            <button type="submit" class="btn btn--primary btn--small">
              {t("tasks.set")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- quick add

function ParsedChips(props: { parsed: Parsed | null; today: string }) {
  return (
    <Show when={props.parsed}>
      {(p) => (
        <div class="qa-chips" aria-live="polite">
          <Show when={p().due_string}>
            <span class="qa-chip qa-chip--date">
              <CalendarIcon />
              {p().due_date
                ? (() => {
                    const date = (p().due_date as string).slice(0, 10);
                    const rel = Math.round((parseDay(date).getTime() - parseDay(props.today).getTime()) / 86_400_000);
                    const time = (p().due_date as string).slice(11, 16);
                    return `${relWord(rel) ?? dayMonth(date, props.today)}${time ? ` ${time}` : ""}`;
                  })()
                : p().due_string}
              <Show when={p().is_recurring}>
                <RepeatIcon />
              </Show>
            </span>
          </Show>
          <Show when={p().project}>
            <span class="qa-chip qa-chip--project">
              <HashIcon />
              {p().project}
              <Show when={p().section}>{(s) => <> / {s()}</>}</Show>
            </span>
          </Show>
          <For each={p().labels}>
            {(l) => (
              <span class="qa-chip qa-chip--label">
                <TagIcon />
                {l}
              </span>
            )}
          </For>
          <Show when={p().priority}>
            {(n) => (
              <span class={`qa-chip qa-chip--p${n()}`}>
                <FlagIcon />
                {t("tasks.priorityN", { n: n() })}
              </span>
            )}
          </Show>
        </div>
      )}
    </Show>
  );
}

function QuickAdd(props: {
  today: string;
  projectId?: string;
  sectionId?: string;
  defaultDue?: string;
  autofocus?: boolean;
  onDone: () => void;
}) {
  const [text, setText] = createSignal("");
  const [desc, setDesc] = createSignal("");
  const [parsed, setParsed] = createSignal<Parsed | null>(null);
  const [pending, setPending] = createSignal(false);
  let input: HTMLInputElement | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));
  onMount(() => {
    if (props.autofocus !== false) input?.focus();
  });

  function onInput(v: string) {
    setText(v);
    clearTimeout(timer);
    timer = setTimeout(async () => setParsed(await parseQuick(v)), PARSE_DEBOUNCE_MS);
  }

  async function submit(e?: Event) {
    e?.preventDefault();
    const line = text().trim();
    if (!line || pending()) return;
    setPending(true);
    const full = desc().trim() ? `${line} // ${desc().trim()}` : line;
    const r = await run({
      type: "quick_add",
      text: full,
      project_id: props.projectId,
      section_id: props.sectionId,
      default_due: props.defaultDue,
    });
    setPending(false);
    if (r) {
      setText("");
      setDesc("");
      setParsed(null);
      showToast({ text: t("tasks.addedToast") });
      input?.focus();
    }
  }

  return (
    <form
      class="quick-add"
      onSubmit={(e) => void submit(e)}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          props.onDone();
        }
      }}
    >
      <input
        ref={input}
        class="qa-input"
        type="text"
        placeholder={t("tasks.taskName")}
        aria-label={t("tasks.taskName")}
        value={text()}
        onInput={(e) => onInput(e.currentTarget.value)}
        maxlength={800}
        autocomplete="off"
        enterkeyhint="done"
      />
      <input
        class="qa-desc"
        type="text"
        placeholder={t("tasks.description")}
        aria-label={t("tasks.description")}
        value={desc()}
        onInput={(e) => setDesc(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Enter" && void submit(e)}
        autocomplete="off"
      />
      <ParsedChips parsed={parsed()} today={props.today} />
      <p class="qa-hint">{t("tasks.quickAddHint")}</p>
      <div class="qa-actions">
        <button type="button" class="btn btn--small" onClick={() => props.onDone()}>
          {t("tasks.cancel")}
        </button>
        <button type="submit" class="btn btn--small btn--primary" disabled={!text().trim() || pending()}>
          {t("tasks.addTask")}
        </button>
      </div>
    </form>
  );
}

function InlineAdd(props: { today: string; projectId?: string; sectionId?: string; defaultDue?: string }) {
  const [open, setOpen] = createSignal(false);
  return (
    <Show
      when={open()}
      fallback={
        <button type="button" class="add-task-row" onClick={() => setOpen(true)}>
          <span class="add-task-plus">
            <PlusIcon />
          </span>
          {t("tasks.addTask")}
        </button>
      }
    >
      <QuickAdd
        today={props.today}
        projectId={props.projectId}
        sectionId={props.sectionId}
        defaultDue={props.defaultDue}
        onDone={() => setOpen(false)}
      />
    </Show>
  );
}

// ---------------------------------------------------------------- task row

interface RowHandlers {
  today: () => string;
  showProject: boolean;
  open: (task: TaskView) => void;
  schedule: (task: TaskView) => void;
}

async function toggle(task: TaskView) {
  if (task.checked) {
    await run({ type: "uncomplete", task_id: task.id });
    return;
  }
  const r = await run({ type: "complete", task_id: task.id });
  if (r) showToast({ text: t("tasks.completedToast"), undo: { type: "uncomplete", task_id: task.id } });
}

function TaskRow(props: { task: TaskView; h: RowHandlers; depth?: number }) {
  const task = () => props.task;
  return (
    <li class="task-item" classList={{ "task-item--child": (props.depth ?? 0) > 0 }}>
      <div
        class="task-row"
        classList={{ "task-row--pending": task().pending, "task-row--done": task().checked }}
        role="button"
        tabindex="0"
        aria-label={`${t("tasks.openTask")}: ${task().content}`}
        onClick={() => props.h.open(task())}
        onKeyDown={(e) => {
          if (e.key === "Enter" && e.target === e.currentTarget) props.h.open(task());
        }}
      >
        <Checkbox task={task()} onToggle={() => void toggle(task())} />
        <div class="task-body">
          <span class="task-content">{task().content}</span>
          <Show when={task().description}>
            <span class="task-desc">{task().description.split("\n")[0]}</span>
          </Show>
          <div class="task-meta">
            <DueChip task={task()} today={props.h.today()} />
            <Show when={task().subtasks > 0}>
              <span class="task-sub">
                <SubtaskIcon />
                {t("tasks.subtaskCount", { done: task().subtasks_done, total: task().subtasks })}
              </span>
            </Show>
            <For each={task().labels}>
              {(l) => (
                <span class="task-label">
                  <TagIcon />
                  {l}
                </span>
              )}
            </For>
            <Show when={task().deadline}>
              {(d) => <span class="task-deadline">{t("tasks.deadline", { date: dayMonth(d(), props.h.today()) })}</span>}
            </Show>
            <Show when={task().parent_content}>
              {(p) => (
                <span class="task-parent">
                  <SubtaskIcon />
                  {p()}
                </span>
              )}
            </Show>
            <Show when={task().pending}>
              <span class="task-pending" title={t("tasks.waitingTodoist")}>
                <RefreshIcon />
              </span>
            </Show>
          </div>
        </div>
        <Show when={props.h.showProject}>
          <span class="task-project" data-color={projectColor(task().project_color)}>
            {task().project_name}
            <Show when={task().section_name}>{(s) => <> / {s()}</>}</Show>
            <HashIcon />
          </span>
        </Show>
        <div class="task-hover">
          <button
            type="button"
            class="icon-btn icon-btn--tiny"
            aria-label={t("tasks.edit")}
            title={t("tasks.edit")}
            onClick={(e) => {
              e.stopPropagation();
              props.h.open(task());
            }}
          >
            <EditIcon />
          </button>
          <button
            type="button"
            class="icon-btn icon-btn--tiny"
            aria-label={t("tasks.dueDate")}
            title={t("tasks.dueDate")}
            onClick={(e) => {
              e.stopPropagation();
              props.h.schedule(task());
            }}
          >
            <CalendarIcon />
          </button>
        </div>
      </div>
      <Show when={(task().children?.length ?? 0) > 0}>
        <ul class="task-list task-list--children">
          <For each={task().children}>{(c) => <TaskRow task={c} h={props.h} depth={(props.depth ?? 0) + 1} />}</For>
        </ul>
      </Show>
    </li>
  );
}

// ---------------------------------------------------------------- detail

function TaskDetail(props: { task: TaskView; data: TasksView; onClose: () => void; onSchedule: () => void }) {
  const task = () => props.task;
  const [content, setContent] = createSignal(task().content);
  const [desc, setDesc] = createSignal(task().description);
  const [labels, setLabels] = createSignal(task().labels.join(" "));
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  let first: HTMLInputElement | undefined;
  onMount(() => first?.focus());

  const project = createMemo(() => props.data.projects.find((p) => p.id === task().project_id));

  async function save(e?: Event) {
    e?.preventDefault();
    const cmd: TaskCommand & { type: "update" } = { type: "update", task_id: task().id };
    if (content().trim() && content().trim() !== task().content) cmd.content = content().trim();
    if (desc() !== task().description) cmd.description = desc();
    const ls = labels()
      .split(/[\s,]+/)
      .map((x) => x.replace(/^@/, ""))
      .filter(Boolean);
    if (ls.join(" ") !== task().labels.join(" ")) cmd.labels = ls;
    if (Object.keys(cmd).length > 2) {
      setBusy(true);
      const r = await run(cmd);
      setBusy(false);
      if (!r) return;
    }
    props.onClose();
  }

  async function setPriority(p: number) {
    if (p === task().priority) return;
    await run({ type: "update", task_id: task().id, priority: p });
  }

  async function remove() {
    setBusy(true);
    const r = await run({ type: "delete", task_id: task().id });
    setBusy(false);
    if (r) {
      showToast({ text: t("tasks.deletedToast"), undo: { type: "undo_delete", task_id: task().id } });
      props.onClose();
    }
  }

  async function moveTo(value: string) {
    const [kind, id] = value.split(":");
    if (!id) return;
    if (kind === "p" && id !== task().project_id) await run({ type: "move", task_id: task().id, project_id: id });
    if (kind === "s" && id !== task().section_id) await run({ type: "move", task_id: task().id, section_id: id });
  }

  return (
    <div class="tasks-backdrop" onClick={() => void save()}>
      <form
        class="task-detail"
        role="dialog"
        aria-modal="true"
        aria-label={task().content}
        onClick={(e) => e.stopPropagation()}
        onSubmit={(e) => void save(e)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            props.onClose();
          }
        }}
      >
        <div class="task-detail-head">
          <span class="task-detail-project" data-color={projectColor(project()?.color ?? "")}>
            <HashIcon />
            {task().project_name}
            <Show when={task().section_name}>{(s) => <> / {s()}</>}</Show>
          </span>
          <button type="button" class="icon-btn" aria-label={t("tasks.close")} onClick={() => props.onClose()}>
            <CloseIcon />
          </button>
        </div>
        <div class="task-detail-main">
          <Checkbox task={task()} onToggle={() => void toggle(task()).then(props.onClose)} />
          <div class="task-detail-fields">
            <input
              ref={first}
              class="task-detail-title"
              value={content()}
              onInput={(e) => setContent(e.currentTarget.value)}
              aria-label={t("tasks.taskName")}
              maxlength={500}
            />
            <textarea
              class="task-detail-desc"
              rows={3}
              placeholder={t("tasks.description")}
              aria-label={t("tasks.description")}
              value={desc()}
              onInput={(e) => setDesc(e.currentTarget.value)}
            />
          </div>
        </div>

        <div class="task-detail-grid">
          <div class="td-field">
            <span class="field-label">{t("tasks.dueDate")}</span>
            <button type="button" class="btn btn--small td-due" onClick={() => props.onSchedule()}>
              <Show when={task().due} fallback={<span class="muted">{t("tasks.noDate")}</span>}>
                <DueChip task={task()} today={props.data.today} />
              </Show>
            </button>
          </div>
          <div class="td-field">
            <span class="field-label">{t("tasks.priority")}</span>
            <div class="prio-picker" role="radiogroup" aria-label={t("tasks.priority")}>
              <For each={[1, 2, 3, 4]}>
                {(p) => (
                  <button
                    type="button"
                    role="radio"
                    aria-checked={task().priority === p}
                    class={`prio-btn prio-btn--p${p}`}
                    classList={{ "prio-btn--on": task().priority === p }}
                    title={t("tasks.priorityN", { n: p })}
                    onClick={() => void setPriority(p)}
                  >
                    <FlagIcon />
                    <span>P{p}</span>
                  </button>
                )}
              </For>
            </div>
          </div>
          <div class="td-field">
            <label class="field-label" for="td-labels">
              {t("tasks.labelsField")}
            </label>
            <input
              id="td-labels"
              class="text-input"
              value={labels()}
              placeholder={t("tasks.labelsHint")}
              onInput={(e) => setLabels(e.currentTarget.value)}
            />
          </div>
          <div class="td-field">
            <label class="field-label" for="td-move">
              {t("tasks.project")}
            </label>
            <select id="td-move" class="text-input" onChange={(e) => void moveTo(e.currentTarget.value)}>
              <For each={props.data.projects}>
                {(p: ProjectView) => (
                  <>
                    <option value={`p:${p.id}`} selected={p.id === task().project_id && !task().section_id}>
                      {p.name}
                    </option>
                    <For each={p.sections}>
                      {(s) => (
                        <option value={`s:${s.id}`} selected={s.id === task().section_id}>
                          {"  "}
                          {p.name} / {s.name}
                        </option>
                      )}
                    </For>
                  </>
                )}
              </For>
            </select>
          </div>
        </div>

        <section class="td-subtasks" aria-label={t("tasks.subtasks")}>
          <h3 class="td-subtitle">
            <SubtaskIcon />
            {t("tasks.subtasks")}
            <Show when={task().subtasks > 0}>
              <span class="muted small"> {t("tasks.subtaskCount", { done: task().subtasks_done, total: task().subtasks })}</span>
            </Show>
          </h3>
          <ul class="task-list">
            <For each={task().children ?? []}>
              {(c) => (
                <li class="td-sub">
                  <Checkbox task={c} onToggle={() => void toggle(c)} />
                  <span>{c.content}</span>
                </li>
              )}
            </For>
          </ul>
          <SubtaskAdd parentId={task().id} />
        </section>

        <div class="task-detail-foot">
          <Show
            when={confirmDelete()}
            fallback={
              <button type="button" class="btn btn--small btn--ghost-danger" onClick={() => setConfirmDelete(true)}>
                <TrashIcon />
                {t("tasks.delete")}
              </button>
            }
          >
            <span class="confirm-pair">
              <span class="small">{t("tasks.deleteConfirm", { title: task().content })}</span>
              <button type="button" class="btn btn--small btn--danger" disabled={busy()} onClick={() => void remove()}>
                {t("tasks.delete")}
              </button>
              <button type="button" class="btn btn--small" onClick={() => setConfirmDelete(false)}>
                {t("tasks.cancel")}
              </button>
            </span>
          </Show>
          <span class="spacer" />
          <button type="button" class="btn btn--small" onClick={() => props.onClose()}>
            {t("tasks.cancel")}
          </button>
          <button type="submit" class="btn btn--small btn--primary" disabled={busy()}>
            {t("tasks.save")}
          </button>
        </div>
      </form>
    </div>
  );
}

function SubtaskAdd(props: { parentId: string }) {
  const [text, setText] = createSignal("");
  async function add(e: Event) {
    e.preventDefault();
    e.stopPropagation();
    const c = text().trim();
    if (!c) return;
    const r = await run({ type: "add", content: c, parent_id: props.parentId });
    if (r) setText("");
  }
  return (
    <div class="td-sub-add">
      <PlusIcon />
      <input
        class="qa-desc"
        value={text()}
        placeholder={t("tasks.addSubtask")}
        aria-label={t("tasks.addSubtask")}
        onInput={(e) => setText(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Enter" && void add(e)}
      />
    </div>
  );
}

// ---------------------------------------------------------------- sidebar

function NavItem(props: { href: string; icon: JSX.Element; label: string; count?: number; active: boolean; tone?: string }) {
  return (
    <li>
      <A
        href={props.href}
        class={`tasks-nav-item ${props.tone ? `tasks-nav-item--${props.tone}` : ""}`}
        classList={{ "tasks-nav-item--active": props.active }}
        aria-current={props.active ? "page" : undefined}
      >
        <span class="tasks-nav-icon">{props.icon}</span>
        <span class="tasks-nav-label">{props.label}</span>
        <Show when={props.count}>
          <span class="tasks-nav-count">{props.count}</span>
        </Show>
      </A>
    </li>
  );
}

function Sidebar(props: { data: TasksView | null; section: string; id?: string; onQuickAdd: () => void; onNavigate: () => void }) {
  const projects = createMemo(() => (props.data?.projects ?? []).filter((p) => !p.inbox));
  return (
    <nav class="tasks-sidebar" aria-label={t("tasks.title")} onClick={(e) => (e.target as HTMLElement).closest("a") && props.onNavigate()}>
      <button type="button" class="tasks-add-btn" onClick={() => props.onQuickAdd()}>
        <span class="tasks-add-plus">
          <PlusIcon />
        </span>
        {t("tasks.addTask")}
      </button>
      <ul class="tasks-nav">
        <NavItem href="/tasks/search" icon={<SearchIcon />} label={t("tasks.search")} active={props.section === "search"} />
        <NavItem
          href="/tasks/inbox"
          icon={<InboxIcon />}
          label={t("tasks.inbox")}
          count={props.data?.counts.inbox}
          active={props.section === "inbox"}
          tone="inbox"
        />
        <NavItem
          href="/tasks"
          icon={<TodayIcon />}
          label={t("tasks.today")}
          count={props.data?.counts.today}
          active={props.section === "today"}
          tone="today"
        />
        <NavItem href="/tasks/upcoming" icon={<UpcomingIcon />} label={t("tasks.upcoming")} active={props.section === "upcoming"} tone="upcoming" />
        <NavItem
          href="/tasks/filters"
          icon={<FiltersIcon />}
          label={t("tasks.filtersLabels")}
          active={props.section === "filters" || props.section === "label" || props.section === "filter"}
          tone="filters"
        />
        <NavItem href="/tasks/completed" icon={<CheckCircleIcon />} label={t("tasks.completed")} active={props.section === "completed"} tone="completed" />
      </ul>
      <Show when={projects().length > 0}>
        <h2 class="tasks-nav-head">{t("tasks.myProjects")}</h2>
        <ul class="tasks-nav tasks-nav--projects">
          <For each={projects()}>
            {(p) => (
              <li classList={{ "tasks-nav-sub": !!p.parent_id }}>
                <A
                  href={`/tasks/project/${encodeURIComponent(p.id)}`}
                  class="tasks-nav-item"
                  classList={{ "tasks-nav-item--active": props.section === "project" && props.id === p.id }}
                >
                  <span class="tasks-nav-icon project-hash" data-color={projectColor(p.color)}>
                    <HashIcon />
                  </span>
                  <span class="tasks-nav-label">{p.name}</span>
                  <Show when={p.count}>
                    <span class="tasks-nav-count">{p.count}</span>
                  </Show>
                </A>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <SyncLine data={props.data} />
    </nav>
  );
}

function SyncLine(props: { data: TasksView | null }) {
  const [err, setErr] = createSignal<string | null>(null);
  const s = () => props.data?.sync;
  return (
    <div class="tasks-sync" classList={{ [`tasks-sync--${s()?.status ?? "ok"}`]: true }}>
      <Show when={s()}>
        {(sync) => (
          <>
            <span class="tasks-sync-dot" aria-hidden="true" />
            <span class="tasks-sync-text">
              {sync().status === "disconnected"
                ? t("tasks.syncDisconnected")
                : sync().status === "reconnect"
                  ? t("tasks.syncReconnect")
                  : sync().status === "rate_limited"
                    ? t("tasks.syncRateLimited")
                    : sync().status === "error"
                      ? t("tasks.syncError", { error: sync().error ?? "" })
                      : sync().last_sync_ms
                        ? t("tasks.syncOk", { time: relativeTime(sync().last_sync_ms as number) })
                        : t("tasks.syncNever")}
              <Show when={sync().pending > 0 && sync().status !== "disconnected"}>
                {" · "}
                {t("tasks.pendingEdits", { count: sync().pending })}
              </Show>
            </span>
            <Show
              when={sync().status === "disconnected" || sync().status === "reconnect"}
              fallback={
                <button
                  type="button"
                  class="btn btn--small tasks-sync-btn"
                  classList={{ "is-spinning": syncing() }}
                  disabled={syncing()}
                  onClick={async () => setErr(await syncNow())}
                >
                  <RefreshIcon />
                  {syncing() ? t("tasks.syncing") : t("tasks.syncNow")}
                </button>
              }
            >
              <A href="/links" class="btn btn--small btn--primary">
                {t("tasks.connectInLinks")}
              </A>
            </Show>
            <Show when={err()}>{(m) => <span class="tasks-sync-err">{m()}</span>}</Show>
          </>
        )}
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------- pages

function FiltersLabels(props: { data: TasksView }) {
  return (
    <div class="fl-page">
      <section class="fl-section">
        <h2 class="tasks-group-title">{t("tasks.filters")}</h2>
        <Show when={props.data.filters.length > 0} fallback={<p class="muted small">{t("tasks.noFilters")}</p>}>
          <ul class="fl-list">
            <For each={props.data.filters}>
              {(f) => (
                <li>
                  <A href={`/tasks/filter/${encodeURIComponent(f.id)}`} class="fl-item">
                    <span class="fl-icon" data-color={projectColor(f.color)}>
                      <FiltersIcon />
                    </span>
                    <span class="fl-name">{f.name}</span>
                    <span class="fl-query">{f.query}</span>
                    <span class="tasks-nav-count">{f.count || ""}</span>
                  </A>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </section>
      <section class="fl-section">
        <h2 class="tasks-group-title">{t("tasks.labels")}</h2>
        <Show when={props.data.labels.length > 0} fallback={<p class="muted small">{t("tasks.noLabels")}</p>}>
          <ul class="fl-list">
            <For each={props.data.labels}>
              {(l) => (
                <li>
                  <A href={`/tasks/label/${encodeURIComponent(l.name)}`} class="fl-item">
                    <span class="fl-icon" data-color={projectColor(l.color)}>
                      <TagIcon />
                    </span>
                    <span class="fl-name">{l.name}</span>
                    <span class="tasks-nav-count">{l.count || ""}</span>
                  </A>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </section>
    </div>
  );
}

function GroupBlock(props: {
  g: Group;
  data: TasksView;
  h: RowHandlers;
  hidden: Set<string>;
  onReschedule: () => void;
}) {
  const tasks = createMemo(() => props.g.tasks.filter((x) => !props.hidden.has(x.id) && !props.data.undo.includes(x.id)));
  const kind = () => props.data.list.kind;
  const addable = () => props.g.kind !== "overdue" && props.g.kind !== "completed" && kind() !== "search" && kind() !== "filter" && kind() !== "label";
  const title = () =>
    props.g.kind === "overdue" ? t("tasks.overdue") : props.g.kind === "date" || props.g.kind === "completed" ? dateHeader(props.g, props.data.today) : props.g.title;
  const defaultDue = () => (props.g.kind === "date" && props.g.date ? (props.g.rel_days === 0 ? "today" : props.g.date) : undefined);
  return (
    <section class={`tasks-group tasks-group--${props.g.kind}`} aria-label={title() || undefined}>
      <Show when={title()}>
        <header class="tasks-group-head">
          <h2 class="tasks-group-title">
            {title()}
            <Show when={props.g.kind !== "date" || tasks().length > 0}>
              <span class="tasks-group-count">{tasks().length || ""}</span>
            </Show>
          </h2>
          <Show when={props.g.kind === "overdue"}>
            <button type="button" class="link-btn tasks-reschedule" onClick={() => props.onReschedule()}>
              {t("tasks.reschedule")}
            </button>
          </Show>
        </header>
      </Show>
      <ul class="task-list">
        <For each={tasks()}>{(task) => <TaskRow task={task} h={props.h} />}</For>
      </ul>
      <Show when={addable()}>
        <InlineAdd
          today={props.data.today}
          defaultDue={defaultDue()}
          projectId={kind() === "project" || kind() === "inbox" ? props.data.list.id : undefined}
          sectionId={props.g.kind === "section" ? props.g.key : undefined}
        />
      </Show>
    </section>
  );
}

function Empty(props: { kind: string; q?: string }) {
  return (
    <div class="tasks-empty">
      <span class="tasks-empty-art" aria-hidden="true">
        <CheckCircleIcon />
      </span>
      <p class="empty-title">
        {props.kind === "today"
          ? t("tasks.emptyToday")
          : props.kind === "inbox"
            ? t("tasks.emptyInbox")
            : props.kind === "completed"
              ? t("tasks.completedEmpty")
              : props.kind === "search" && props.q
                ? t("tasks.noResults", { q: props.q })
                : t("tasks.emptyList")}
      </p>
      <Show when={props.kind === "today"}>
        <p class="muted small">{t("tasks.emptyTodaySub")}</p>
      </Show>
    </div>
  );
}

function GoalRing(props: { done: number; goal: number }) {
  const pct = () => (props.goal > 0 ? Math.min(1, props.done / props.goal) : 0);
  const r = 9;
  const c = 2 * Math.PI * r;
  return (
    <span
      class="goal"
      classList={{ "goal--reached": props.goal > 0 && props.done >= props.goal }}
      title={props.done >= props.goal ? t("tasks.goalReached") : t("tasks.goalTitle", { goal: props.goal })}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <circle class="goal-track" cx="12" cy="12" r={r} />
        <circle class="goal-fill" cx="12" cy="12" r={r} stroke-dasharray={`${c * pct()} ${c}`} transform="rotate(-90 12 12)" />
      </svg>
      <span class="goal-text">{t("tasks.goal", { done: props.done, goal: props.goal })}</span>
    </span>
  );
}

// ---------------------------------------------------------------- the view

export default function TasksView() {
  const params = useParams<{ section?: string; id?: string }>();
  const [search, setSearch] = useSearchParams<{ q?: string }>();
  const navigate = useNavigate();
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [quickOpen, setQuickOpen] = createSignal(false);
  const [detail, setDetail] = createSignal<string | null>(null);
  const [scheduling, setScheduling] = createSignal<TaskView | null>(null);
  const [rescheduling, setRescheduling] = createSignal(false);
  const [q, setQ] = createSignal(search.q ?? "");
  let searchInput: HTMLInputElement | undefined;

  const section = () => params.section ?? "today";
  const data = () => view();

  createEffect(
    on(
      () => [params.section, params.id, search.q] as const,
      ([s, id, qq]) => {
        const sec = s === "filters" ? "today" : s;
        void load(queryFor(sec, id ? decodeURIComponent(id) : undefined, qq));
        setMenuOpen(false);
      },
    ),
  );

  onMount(() => {
    onCleanup(startTasks());
    let gPressed = 0;
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (el && (el.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName))) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "Escape") {
        setQuickOpen(false);
        setMenuOpen(false);
        return;
      }
      if (Date.now() - gPressed < 1200) {
        const to: Record<string, string> = { t: "/tasks", i: "/tasks/inbox", u: "/tasks/upcoming", f: "/tasks/filters", c: "/tasks/completed" };
        const dest = to[e.key.toLowerCase()];
        gPressed = 0;
        if (dest) {
          e.preventDefault();
          navigate(dest);
          return;
        }
      }
      if (e.key === "g") {
        gPressed = Date.now();
        return;
      }
      if (e.key === "q") {
        e.preventDefault();
        setQuickOpen(true);
      } else if (e.key === "/") {
        e.preventDefault();
        if (section() !== "search") navigate("/tasks/search");
        queueMicrotask(() => searchInput?.focus());
      }
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(searchTimer));
  function onSearch(v: string) {
    setQ(v);
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => setSearch({ q: v || undefined }, { replace: true }), SEARCH_DEBOUNCE_MS);
  }

  const hidden = createMemo(() => hiddenIds());
  const listCount = createMemo(() => {
    const d = data();
    if (!d) return 0;
    const n = (x: TaskView): number => 1 + (x.children ?? []).reduce((a, c) => a + n(c), 0);
    return d.list.groups.flatMap((g) => g.tasks).filter((x) => !hidden().has(x.id) && !d.undo.includes(x.id)).reduce((a, x) => a + n(x), 0);
  });
  const findTask = (id: string): TaskView | null => {
    const d = data();
    if (!d) return null;
    const walk = (xs: TaskView[]): TaskView | null => {
      for (const x of xs) {
        if (x.id === id) return x;
        const c = walk(x.children ?? []);
        if (c) return c;
      }
      return null;
    };
    return walk(d.list.groups.flatMap((g) => g.tasks));
  };
  const detailTask = createMemo(() => (detail() ? findTask(detail() as string) : null));

  const title = createMemo(() => {
    const d = data();
    switch (section()) {
      case "inbox":
        return t("tasks.inbox");
      case "upcoming":
        return t("tasks.upcoming");
      case "completed":
        return t("tasks.completed");
      case "filters":
        return t("tasks.filtersLabels");
      case "search":
        return t("tasks.search");
      case "project":
      case "label":
      case "filter":
        return d?.list.title || "";
      default:
        return t("tasks.today");
    }
  });

  const handlers: RowHandlers = {
    today: () => data()?.today ?? "",
    get showProject() {
      return section() !== "project" && section() !== "inbox";
    },
    open: (task) => setDetail(task.id),
    schedule: (task) => setScheduling(task),
  };

  async function pickDate(task: TaskView, date: string | null) {
    setScheduling(null);
    if (date === null) await run({ type: "update", task_id: task.id, clear_due: true });
    else await run({ type: "reschedule", task_id: task.id, date });
  }

  async function rescheduleOverdue(date: string | null) {
    setRescheduling(false);
    if (!date) return;
    const n = data()?.counts.overdue ?? 0;
    const r = await run({ type: "reschedule_overdue", date });
    if (r) showToast({ text: t("tasks.rescheduledToast", { count: n }) });
  }

  const showList = () => section() !== "filters";
  const subtitleDate = () => {
    const d = data();
    if (!d || section() !== "today") return null;
    return parseDay(d.today).toLocaleDateString(locale(), { weekday: "long", day: "numeric", month: "long" });
  };

  return (
    <div class="tasks" classList={{ "tasks--menu": menuOpen() }}>
      <div class="tasks-inner">
        <div class="tasks-scrim" onClick={() => setMenuOpen(false)} aria-hidden="true" />
        <Sidebar
          data={data()}
          section={section()}
          id={params.id ? decodeURIComponent(params.id) : undefined}
          onQuickAdd={() => {
            setMenuOpen(false);
            setQuickOpen(true);
          }}
          onNavigate={() => setMenuOpen(false)}
        />
        <main class="tasks-main" aria-labelledby="tasks-title">
          <header class="tasks-head">
            <button type="button" class="icon-btn tasks-menu-btn" aria-label={t("tasks.menu")} onClick={() => setMenuOpen(!menuOpen())}>
              <MenuIcon />
            </button>
            <div class="tasks-head-text">
              <h1 class="view-title" id="tasks-title">
                {title()}
              </h1>
              <p class="tasks-sub">
                <Show when={subtitleDate()}>{(s) => <span class="tasks-sub-date">{s()}</span>}</Show>
                <Show when={showList() && data()}>
                  <span class="tasks-sub-count">
                    <CheckCircleIcon />
                    {t("tasks.taskCount", { count: listCount() })}
                  </span>
                </Show>
              </p>
            </div>
            <Show when={data()}>{(d) => <GoalRing done={d().goal.completed_today} goal={d().goal.daily_goal} />}</Show>
          </header>

          <Show when={section() === "search"}>
            <div class="tasks-search">
              <SearchIcon />
              <input
                ref={searchInput}
                type="search"
                class="text-input"
                placeholder={t("tasks.searchPlaceholder")}
                aria-label={t("tasks.searchPlaceholder")}
                value={q()}
                onInput={(e) => onSearch(e.currentTarget.value)}
                autofocus
              />
            </div>
          </Show>

          <Show when={outbox().length > 0}>
            <div class="notice tasks-offline" role="status">
              {t("tasks.offlineQueued", { count: outbox().length })}
            </div>
          </Show>
          <Show when={actionError()}>
            {(m) => (
              <div class="notice" role="alert">
                <span>{m()}</span>
                <button type="button" class="banner-close" onClick={() => setActionError(null)} aria-label={t("tasks.dismiss")}>
                  ×
                </button>
              </div>
            )}
          </Show>
          <Show when={(data()?.conflicts.length ?? 0) > 0}>
            <section class="tasks-conflicts" aria-label={t("tasks.conflictsTitle")}>
              <h2 class="tasks-conflicts-title">{t("tasks.conflictsTitle")}</h2>
              <ul>
                <For each={data()?.conflicts ?? []}>
                  {(c) => (
                    <li class="tasks-conflict">
                      <span>
                        <strong>{c.content}</strong> — {c.message}
                      </span>
                      <button type="button" class="link-btn" onClick={() => void run({ type: "dismiss_conflict", conflict_id: c.id })}>
                        {t("tasks.dismiss")}
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </section>
          </Show>
          <Show when={data()?.list.warning && section() !== "filters"}>
            <p class="tasks-warning">{t("tasks.filterWarning", { detail: data()?.list.warning ?? "" })}</p>
          </Show>

          <div class="tasks-scroll">
            <Show
              when={data()}
              fallback={
                <Show when={loadError()} fallback={<p class="muted center" role="status">{t("tasks.loading")}</p>}>
                  {(m) => (
                    <div class="empty" role="alert">
                      <p class="empty-title">{m()}</p>
                      <button class="btn" type="button" onClick={() => void load(query())}>
                        {t("common.tryAgain")}
                      </button>
                    </div>
                  )}
                </Show>
              }
            >
              {(d) => (
                <Show when={showList()} fallback={<FiltersLabels data={d()} />}>
                  <Show when={waitingAdds().length > 0}>
                    <ul class="task-list task-list--waiting">
                      <For each={waitingAdds()}>
                        {(text) => (
                          <li class="task-item">
                            <div class="task-row task-row--pending" title={t("tasks.waiting")}>
                              <span class="task-check task-check--p4" aria-hidden="true" />
                              <span class="task-body">
                                <span class="task-content">{text}</span>
                                <span class="task-desc">{t("tasks.waiting")}</span>
                              </span>
                            </div>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                  <For each={d().list.groups}>
                    {(g) => <GroupBlock g={g} data={d()} h={handlers} hidden={hidden()} onReschedule={() => setRescheduling(true)} />}
                  </For>
                  <Show when={listCount() === 0 && d().list.kind !== "upcoming" && !(d().list.kind === "search" && !q())}>
                    <Empty kind={String(d().list.kind)} q={q()} />
                  </Show>
                  <p class="tasks-shortcuts muted small">{t("tasks.shortcuts")}</p>
                </Show>
              )}
            </Show>
          </div>
        </main>
      </div>

      <button type="button" class="tasks-fab" aria-label={t("tasks.addTask")} onClick={() => setQuickOpen(true)}>
        <PlusIcon />
      </button>

      <Show when={quickOpen() && data()}>
        <div class="tasks-backdrop" onClick={() => setQuickOpen(false)}>
          <div class="qa-modal" role="dialog" aria-modal="true" aria-label={t("tasks.quickAddTitle")} onClick={(e) => e.stopPropagation()}>
            <QuickAdd
              today={data()?.today ?? ""}
              projectId={section() === "project" ? data()?.list.id : undefined}
              defaultDue={section() === "today" ? "today" : undefined}
              onDone={() => setQuickOpen(false)}
            />
          </div>
        </div>
      </Show>

      <Show when={scheduling() && data()}>
        <DatePicker
          today={data()?.today ?? ""}
          allowClear
          onPick={(d) => void pickDate(scheduling() as TaskView, d)}
          onClose={() => setScheduling(null)}
        />
      </Show>

      <Show when={rescheduling() && data()}>
        <DatePicker today={data()?.today ?? ""} title={t("tasks.rescheduleTitle")} onPick={(d) => void rescheduleOverdue(d)} onClose={() => setRescheduling(false)} />
      </Show>

      <Show when={detailTask() && data()}>
        <TaskDetail
          task={detailTask() as TaskView}
          data={data() as TasksView}
          onClose={() => setDetail(null)}
          onSchedule={() => setScheduling(detailTask())}
        />
      </Show>

      <Show when={toast()}>
        {(tt) => (
          <div class="tasks-toast" role="status">
            <span>{tt().text}</span>
            <Show when={tt().undo}>
              {(u) => (
                <button
                  type="button"
                  class="link-btn"
                  onClick={() => {
                    showToast(null);
                    void run(u());
                  }}
                >
                  {t("tasks.undo")}
                </button>
              )}
            </Show>
            <button type="button" class="banner-close" onClick={() => showToast(null)} aria-label={t("tasks.dismiss")}>
              ×
            </button>
          </div>
        )}
      </Show>
    </div>
  );
}
