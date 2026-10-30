import { useSearchParams } from "@solidjs/router";
import type { Calendar } from "@event-calendar/core";
import { batch, createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import {
  acceptProposal,
  createEvent,
  deleteEvent,
  dismissProposal,
  listEvents,
  listProposals,
  syncCalendars,
  updateEvent,
  type CalEvent,
  type CalProposal,
} from "../api/calendar";
import { toApiError } from "../api/client";
import EventCalendar, { type EcApi } from "../components/calendar/EventCalendar";
import { EventDetail, ProposalDetail } from "../components/calendar/EventDetail";
import { EventEditor } from "../components/calendar/EventEditor";
import { EditIcon, PlusIcon, RefreshIcon, TrashIcon } from "../components/Icons";
import { Button, ErrorState, LoadingState, Page, PageHeader, PrimaryAction, SectionNav, Sheet, useWide } from "../components/page";
import { locale, t } from "../i18n";
import {
  calendarColor,
  calendarName,
  defaultStart,
  draftFromForm,
  formForEvent,
  formForRange,
  matchDeepLink,
  proposalToEc,
  proposalWhen,
  startLabel,
  timesOf,
  toEcEvent,
  whenLabel,
  type EcExtra,
  type EventForm,
} from "../lib/calendar";
import "../styles/calendar.css";

// The Calendar tab: Lucina's own calendar (always writable) with the iPhone, Google and subscribed calendars
// alongside. Month, week (3 days on a phone), day and list views on EventCalendar (components/calendar/EventCalendar);
// tap an event for its sheet, tap or drag over a slot to create one, drag an editable event to move it.
// `?event=<id>&at=<ms>` opens one event (notifications, chat cards, Links).

type ViewKey = "month" | "week" | "3day" | "day" | "list";

const EC_VIEW: Record<ViewKey, string> = {
  month: "dayGridMonth",
  week: "timeGridWeek",
  "3day": "timeGrid3Day",
  day: "timeGridDay",
  list: "list30",
};

const VIEW_KEY = "secretary.calendar.view";
const HIDDEN_KEY = "secretary.calendar.hidden";
const REFRESH_MS = 5 * 60_000;
const TOAST_MS = 8_000;
const PULSE_MS = 2_400;

function readStore(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStore(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Private mode: the choice lasts for this visit only.
  }
}

function isViewKey(v: unknown): v is ViewKey {
  return v === "month" || v === "week" || v === "3day" || v === "day" || v === "list";
}

function el(tag: string, cls: string, text?: string): HTMLElement {
  const n = document.createElement(tag);
  n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
}

interface Toast {
  text: string;
  undo?: () => Promise<void>;
}

type Selected = { kind: "event"; ev: CalEvent } | { kind: "proposal"; p: CalProposal };

interface Editor {
  mode: "new" | "edit";
  id?: string;
  form: EventForm;
  error?: string;
  saving: boolean;
}

export default function CalendarView() {
  const [params, setParams] = useSearchParams();
  const { wide, trackSlot } = useWide();

  const stored = readStore(VIEW_KEY);
  const [choice, setChoice] = createSignal<ViewKey | null>(isViewKey(params.view) ? params.view : isViewKey(stored) ? stored : null);
  const viewKey = createMemo<ViewKey>(() => {
    const c = choice();
    if (!c) return wide() ? "week" : "list";
    if (wide() && c === "3day") return "week";
    if (!wide() && c === "week") return "3day";
    return c;
  });

  const [events, setEvents] = createSignal<CalEvent[]>([]);
  const [proposals, setProposals] = createSignal<CalProposal[]>([]);
  const [status, setStatus] = createSignal<"loading" | "ready" | "error">("loading");
  const [loadError, setLoadError] = createSignal("");
  const [title, setTitle] = createSignal("");
  const [range, setRange] = createSignal<{ start: Date; end: Date } | null>(null);
  const [hidden, setHidden] = createSignal<Set<string>>(new Set(parseHidden(readStore(HIDDEN_KEY))));
  const [selected, setSelected] = createSignal<Selected | null>(null);
  const [editor, setEditor] = createSignal<Editor | null>(null);
  const [confirmDelete, setConfirmDelete] = createSignal<CalEvent | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [toast, setToast] = createSignal<Toast | null>(null);
  const [pulse, setPulse] = createSignal<string | null>(null);
  const [syncing, setSyncing] = createSignal(false);

  let api: EcApi | undefined;
  const cache = new Map<string, CalEvent[]>();
  let seq = 0;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let pulseTimer: ReturnType<typeof setTimeout> | undefined;

  // The deep link waiting for its events.
  const linkAt = () => {
    const n = Number(params.at);
    return params.at && Number.isFinite(n) ? n : undefined;
  };
  let pendingLink: { id: string; at?: number } | null = typeof params.event === "string" && params.event ? { id: params.event, at: linkAt() } : null;
  const initialDate = (() => {
    const at = linkAt();
    if (at !== undefined) return new Date(at);
    const d = typeof params.date === "string" && /^\d{4}-\d{2}-\d{2}$/.test(params.date) ? params.date : null;
    return d ?? undefined;
  })();

  function parseHidden(raw: string | null): string[] {
    try {
      const v = raw ? (JSON.parse(raw) as unknown) : [];
      return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
    } catch {
      return [];
    }
  }

  function showToast(text: string, undo?: Toast["undo"]) {
    clearTimeout(toastTimer);
    setToast({ text, undo });
    toastTimer = setTimeout(() => setToast(null), TOAST_MS);
  }

  function highlight(id: string) {
    clearTimeout(pulseTimer);
    setPulse(id);
    pulseTimer = setTimeout(() => setPulse(null), PULSE_MS);
  }

  const rangeKey = (r: { start: Date; end: Date }) => `${r.start.getTime()}-${r.end.getTime()}`;

  async function load(r = range()) {
    if (!r) return;
    const my = ++seq;
    try {
      const page = await listEvents(r.start, r.end);
      if (my !== seq) return;
      const list = Array.isArray(page.events) ? page.events : [];
      cache.set(rangeKey(r), list);
      batch(() => {
        setEvents(list);
        setStatus("ready");
        setLoadError("");
      });
      resolveLink(list, r);
    } catch (e) {
      if (my !== seq) return;
      if (status() !== "ready") {
        setLoadError(toApiError(e).message);
        setStatus("error");
      }
    }
  }

  async function loadProposals() {
    try {
      const r = await listProposals();
      setProposals(Array.isArray(r.proposals) ? r.proposals.filter((p) => p.expiresAtMs > Date.now()) : []);
    } catch {
      // Proposals are extra: the calendar works without them.
    }
  }

  function resolveLink(list: CalEvent[], r: { start: Date; end: Date }) {
    const link = pendingLink;
    if (!link) return;
    if (link.at !== undefined && (link.at < r.start.getTime() || link.at >= r.end.getTime())) return;
    pendingLink = null;
    setParams({ event: undefined, at: undefined }, { replace: true });
    const hit = matchDeepLink(list, link.id, link.at);
    if (hit) {
      setSelected({ kind: "event", ev: hit });
      highlight(hit.id);
    } else {
      showToast(t("calendar.notFound"));
    }
  }

  // A new deep link while the page is open (a notification tapped on this page).
  createEffect(
    on(
      () => params.event,
      (id) => {
        if (typeof id !== "string" || !id || pendingLink?.id === id) return;
        pendingLink = { id, at: linkAt() };
        if (pendingLink.at !== undefined) api?.gotoDate(new Date(pendingLink.at));
        void load();
      },
      { defer: true },
    ),
  );

  const refreshAll = () => {
    cache.clear();
    void load();
    void loadProposals();
  };

  onMount(() => {
    void loadProposals();
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") refreshAll();
    }, REFRESH_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") refreshAll();
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);
    onCleanup(() => {
      clearInterval(timer);
      clearTimeout(toastTimer);
      clearTimeout(pulseTimer);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
    });
  });

  // ---------- what EventCalendar shows ----------

  const calendars = createMemo(() => {
    const seen = new Map<string, string>();
    seen.set("Lucina", calendarColor("Lucina", "lucina"));
    for (const e of events()) {
      const name = calendarName(e);
      if (!seen.has(name)) seen.set(name, calendarColor(name, e.source));
    }
    return [...seen].map(([name, color]) => ({ name, color }));
  });

  const ecEvents = createMemo<Calendar.EventInput[]>(() => {
    const off = hidden();
    const now = Date.now();
    const lit = pulse();
    const out: Calendar.EventInput[] = [];
    for (const e of events()) {
      if (off.has(calendarName(e))) continue;
      const x = toEcEvent(e, now);
      if (lit === e.id) x.classNames = [...(x.classNames as string[]), "cal-ev--pulse"];
      out.push(x);
    }
    if (!off.has("Lucina")) {
      for (const p of proposals()) {
        const x = proposalToEc(p);
        if (x) out.push(x);
      }
    }
    return out;
  });

  function toggleCalendar(name: string) {
    const next = new Set(hidden());
    if (next.has(name)) next.delete(name);
    else next.add(name);
    setHidden(next);
    writeStore(HIDDEN_KEY, JSON.stringify([...next]));
  }

  function pickView(key: ViewKey) {
    setChoice(key);
    writeStore(VIEW_KEY, key);
  }

  // ---------- creating, editing, moving, deleting ----------

  function openNew(start: Date, end: Date, allDay: boolean) {
    setSelected(null);
    setEditor({ mode: "new", form: formForRange(start, end, allDay), saving: false });
  }

  function openNewDefault() {
    const viewDate = range()?.start ?? new Date();
    const visible = range();
    const now = new Date();
    const base = visible && now >= visible.start && now < visible.end ? now : viewDate;
    const s = defaultStart(base, now);
    openNew(s, new Date(s.getTime() + 3_600_000), false);
  }

  function openEdit(ev: CalEvent) {
    setSelected(null);
    setEditor({ mode: "edit", id: ev.id, form: formForEvent(ev), saving: false });
  }

  async function saveEditor() {
    const ed = editor();
    if (!ed || ed.saving) return;
    const r = draftFromForm(ed.form);
    if ("error" in r) {
      setEditor({ ...ed, error: t(`calendar.${r.error}`) });
      return;
    }
    setEditor({ ...ed, saving: true, error: undefined });
    try {
      if (ed.mode === "new") {
        const res = await createEvent(r.draft);
        setEditor(null);
        showToast(t("calendar.created", { title: r.draft.title }));
        if (res.event) {
          setEvents((list) => [...list.filter((x) => x.id !== res.event!.id), res.event!]);
          highlight(res.event.id);
        }
      } else if (ed.id) {
        await updateEvent(ed.id, { ...r.draft, conferenceUrl: ed.form.video.trim() });
        setEditor(null);
        showToast(t("calendar.updated", { title: r.draft.title }));
        highlight(ed.id);
      }
      refreshAll();
    } catch (e) {
      const cur = editor();
      if (cur) setEditor({ ...cur, saving: false, error: t("calendar.failed", { reason: toApiError(e).message }) });
    }
  }

  async function onMoved(info: { event: Calendar.Event; oldEvent: Calendar.Event; revert: () => void }) {
    const ev = (info.event.extendedProps as EcExtra).ev;
    if (!ev || !ev.editable) {
      info.revert();
      showToast(t("calendar.readOnly"));
      return;
    }
    const body = timesOf(info.event.start, info.event.end, info.event.allDay);
    const back = timesOf(info.oldEvent.start, info.oldEvent.end, info.oldEvent.allDay);
    try {
      await updateEvent(ev.id, body);
      showToast(t("calendar.moved", { title: ev.title, when: startLabel(info.event.start, info.event.allDay, locale()) }), async () => {
        await updateEvent(ev.id, back);
        showToast(t("calendar.undone"));
        refreshAll();
      });
      refreshAll();
    } catch (e) {
      info.revert();
      showToast(t("calendar.failed", { reason: toApiError(e).message }));
    }
  }

  async function doDelete() {
    const ev = confirmDelete();
    if (!ev || busy()) return;
    setBusy(true);
    try {
      await deleteEvent(ev.id);
      batch(() => {
        setConfirmDelete(null);
        setSelected(null);
        setEvents((list) => list.filter((x) => x.id !== ev.id));
      });
      showToast(t("calendar.deleted", { title: ev.title }));
      refreshAll();
    } catch (e) {
      showToast(t("calendar.failed", { reason: toApiError(e).message }));
    } finally {
      setBusy(false);
    }
  }

  async function accept(p: CalProposal) {
    if (busy()) return;
    setBusy(true);
    try {
      const res = await acceptProposal(p.id);
      setSelected(null);
      setProposals((list) => list.filter((x) => x.id !== p.id));
      showToast(t("calendar.accepted", { title: p.event.title }));
      if (res.event) highlight(res.event.id);
      refreshAll();
    } catch (e) {
      showToast(t("calendar.failed", { reason: toApiError(e).message }));
    } finally {
      setBusy(false);
    }
  }

  async function dismiss(p: CalProposal) {
    if (busy()) return;
    setBusy(true);
    try {
      await dismissProposal(p.id);
      setSelected(null);
      setProposals((list) => list.filter((x) => x.id !== p.id));
      showToast(t("calendar.dismissed"));
    } catch (e) {
      showToast(t("calendar.failed", { reason: toApiError(e).message }));
    } finally {
      setBusy(false);
    }
  }

  async function sync() {
    if (syncing()) return;
    setSyncing(true);
    try {
      await syncCalendars();
      showToast(t("calendar.synced"));
      refreshAll();
    } catch (e) {
      showToast(t("calendar.failed", { reason: toApiError(e).message }));
    } finally {
      setSyncing(false);
    }
  }

  // ---------- EventCalendar options ----------

  const time: Intl.DateTimeFormatOptions = { hour: "2-digit", minute: "2-digit", hourCycle: "h23" };

  function eventContent(info: Calendar.EventContentInfo): Calendar.Content {
    const ext = info.event.extendedProps as EcExtra;
    const type = info.view.type;
    const list = type.startsWith("list");
    const month = type === "dayGridMonth";
    const short = !list && !month && !info.event.allDay && info.event.end.getTime() - info.event.start.getTime() < 45 * 60_000;
    const body = el("div", short ? "cal-ev-body cal-ev-body--inline" : "cal-ev-body");
    if (ext.proposal) body.append(el("span", "cal-ev-badge", t("calendar.proposed")));
    // Month cells give the start only (on a phone not even that: the cells are narrow).
    if (info.timeText && !list && !info.event.allDay && !(month && !wide())) {
      const text = month ? new Intl.DateTimeFormat(locale(), time).format(info.event.start) : info.timeText;
      body.append(el("span", "cal-ev-time", text));
    }
    body.append(el("span", "cal-ev-title", String(info.event.title ?? "")));
    const loc = ext.ev?.location ?? ext.proposal?.event.location;
    if (loc && !month) body.append(el("span", "cal-ev-loc", loc));
    if (list && ext.ev) body.append(el("span", "cal-ev-cal", calendarName(ext.ev)));
    if (!list) return { domNodes: [body] };
    // List: a time column (start and end on two lines), then the event.
    const row = el("div", "cal-list-row");
    const when = el("div", "cal-list-time");
    if (info.event.allDay) when.textContent = t("calendar.allDay");
    else {
      const fmt = new Intl.DateTimeFormat(locale(), time);
      when.append(el("span", "", fmt.format(info.event.start)), el("span", "cal-list-end", fmt.format(info.event.end)));
    }
    row.append(when, el("span", ext.proposal ? "cal-dot cal-dot--proposal" : "cal-dot"), body);
    return { domNodes: [row] };
  }

  const options: Calendar.Options = {
    date: initialDate,
    firstDay: 1,
    headerToolbar: { start: "", center: "", end: "" },
    views: {
      dayGridMonth: { dayHeaderFormat: { weekday: "short" } },
      timeGrid3Day: { type: "timeGridWeek", duration: { days: 3 } },
      list30: { type: "listWeek", duration: { days: 30 } },
    },
    nowIndicator: true,
    // Events at the same time sit side by side, not stacked over each other.
    slotEventOverlap: false,
    selectable: true,
    editable: true,
    dayMaxEvents: true,
    scrollTime: "06:30:00",
    slotLabelFormat: time,
    eventTimeFormat: time,
    dayHeaderFormat: { weekday: "short", day: "numeric" },
    allDayContent: t("calendar.allDay"),
    noEventsContent: t("calendar.nothing"),
    longPressDelay: 350,
    eventLongPressDelay: 350,
    selectLongPressDelay: 350,
    eventContent,
    datesSet: (info) => {
      const r = { start: info.start, end: info.end };
      batch(() => {
        setRange(r);
        setTitle(info.view.title);
        const cached = cache.get(rangeKey(r));
        if (cached) setEvents(cached);
      });
      void load(r);
    },
    eventClick: (info) => {
      const ext = info.event.extendedProps as EcExtra;
      if (ext.ev) setSelected({ kind: "event", ev: ext.ev });
      else if (ext.proposal) setSelected({ kind: "proposal", p: ext.proposal });
    },
    dateClick: (info) => {
      if (info.view.type === "dayGridMonth") {
        const s = new Date(info.date.getFullYear(), info.date.getMonth(), info.date.getDate(), 9);
        openNew(s, new Date(s.getTime() + 3_600_000), false);
      } else if (info.allDay) {
        openNew(info.date, new Date(info.date.getTime() + 86_400_000), true);
      } else {
        openNew(info.date, new Date(info.date.getTime() + 3_600_000), false);
      }
    },
    select: (info) => {
      api?.unselect();
      if (info.view.type === "dayGridMonth" && info.end.getTime() - info.start.getTime() <= 86_400_000) {
        const s = new Date(info.start.getFullYear(), info.start.getMonth(), info.start.getDate(), 9);
        openNew(s, new Date(s.getTime() + 3_600_000), false);
        return;
      }
      openNew(info.start, info.end, info.allDay);
    },
    eventDrop: (info) => void onMoved(info),
    eventResize: (info) => void onMoved(info),
  };

  // Narrow phone columns: the block's height already says when it ends.
  const [ready, setReady] = createSignal(false);
  createEffect(() => {
    if (ready()) api?.setOption("displayEventEnd", wide());
  });

  const height = createMemo(() =>
    viewKey() === "list" ? "auto" : wide() ? "max(560px, calc(100dvh - 240px))" : "max(420px, calc(100dvh - 345px))",
  );

  const navItems = createMemo(() => {
    const keys: ViewKey[] = wide() ? ["month", "week", "day", "list"] : ["month", "3day", "day", "list"];
    const label: Record<ViewKey, string> = {
      month: t("calendar.viewMonth"),
      week: t("calendar.viewWeek"),
      "3day": t("calendar.view3day"),
      day: t("calendar.viewDay"),
      list: t("calendar.viewList"),
    };
    return keys.map((k) => ({ label: label[k], current: viewKey() === k, onSelect: () => pickView(k) }));
  });

  const selectedEvent = () => {
    const s = selected();
    return s?.kind === "event" ? s.ev : null;
  };
  const selectedProposal = () => {
    const s = selected();
    return s?.kind === "proposal" ? s.p : null;
  };

  return (
    <Page class="cal-page" label={t("calendar.title")} ref={trackSlot}>
      <PageHeader
        title={title() || t("calendar.title")}
        nav={<SectionNav label={t("calendar.views")} items={navItems()} />}
        action={
          <Show when={wide()}>
            <PrimaryAction icon={<PlusIcon />} onClick={openNewDefault}>
              {t("calendar.newEvent")}
            </PrimaryAction>
          </Show>
        }
      />

      <div class="cal-toolbar">
        <div class="cal-nav">
          <Button onClick={() => api?.gotoDate(new Date())}>{t("calendar.today")}</Button>
          <button type="button" class="pg-icon-btn" aria-label={t("calendar.prev")} title={t("calendar.prev")} onClick={() => api?.prev()}>
            <svg class="pg-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path d="m14.5 6-6 6 6 6" />
            </svg>
          </button>
          <button type="button" class="pg-icon-btn" aria-label={t("calendar.next")} title={t("calendar.next")} onClick={() => api?.next()}>
            <svg class="pg-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path d="m9.5 6 6 6-6 6" />
            </svg>
          </button>
          <button
            type="button"
            class="pg-icon-btn"
            classList={{ "cal-spin": syncing() }}
            aria-label={t("calendar.sync")}
            title={t("calendar.sync")}
            disabled={syncing()}
            onClick={() => void sync()}
          >
            <RefreshIcon />
          </button>
          <Show when={!wide()}>
            <button type="button" class="cal-fab" aria-label={t("calendar.newEvent")} title={t("calendar.newEvent")} onClick={openNewDefault}>
              <PlusIcon />
            </button>
          </Show>
        </div>
        <ul class="cal-legend" aria-label={t("calendar.calendars")}>
          <For each={calendars()}>
            {(c) => {
              const off = () => hidden().has(c.name);
              return (
                <li>
                  <button
                    type="button"
                    class="cal-chip"
                    classList={{ "cal-chip--off": off() }}
                    aria-pressed={!off()}
                    title={off() ? t("calendar.showCalendar", { name: c.name }) : t("calendar.hideCalendar", { name: c.name })}
                    onClick={() => toggleCalendar(c.name)}
                  >
                    <span class="cal-dot" style={{ "--cal-color": c.color }} aria-hidden="true" />
                    {c.name}
                  </button>
                </li>
              );
            }}
          </For>
        </ul>
      </div>

      <Show when={status() === "error"}>
        <ErrorState message={t("calendar.loadFailed", { reason: loadError() })} retryLabel={t("calendar.retry")} onRetry={refreshAll} />
      </Show>
      <Show when={status() === "loading"}>
        <LoadingState label={t("calendar.loading")} />
      </Show>

      <div class="cal-frame" classList={{ "cal-frame--list": viewKey() === "list", "cal-frame--hidden": status() === "error" }}>
        <EventCalendar
          class="cal-ec"
          options={options}
          events={ecEvents()}
          view={EC_VIEW[viewKey()]}
          height={height()}
          locale={locale()}
          onReady={(x) => {
            api = x;
            setReady(true);
          }}
        />
      </div>

      <Show when={toast()}>
        {(tt) => (
          <div class="cal-toast" role="status">
            <span>{tt().text}</span>
            <Show when={tt().undo}>
              {(undo) => (
                <button
                  type="button"
                  class="cal-toast-btn"
                  onClick={() => {
                    setToast(null);
                    void undo()().catch((e) => showToast(t("calendar.failed", { reason: toApiError(e).message })));
                  }}
                >
                  {t("calendar.undo")}
                </button>
              )}
            </Show>
          </div>
        )}
      </Show>

      <Sheet
        open={!!selectedEvent()}
        onClose={() => setSelected(null)}
        title={selectedEvent()?.title ?? ""}
        class="cal-sheet"
        footer={
          <Show when={selectedEvent()?.editable}>
            <Button variant="danger" onClick={() => setConfirmDelete(selectedEvent())}>
              <TrashIcon />
              {t("calendar.delete")}
            </Button>
            <Button onClick={() => openEdit(selectedEvent()!)}>
              <EditIcon />
              {t("calendar.edit")}
            </Button>
          </Show>
        }
      >
        <Show when={selectedEvent()}>{(ev) => <EventDetail event={ev()} />}</Show>
      </Sheet>

      <Sheet
        open={!!selectedProposal()}
        onClose={() => setSelected(null)}
        title={selectedProposal()?.event.title ?? ""}
        class="cal-sheet"
        footer={
          <Show when={selectedProposal()}>
            {(p) => (
              <>
                <Button variant="ghost" disabled={busy()} onClick={() => void dismiss(p())}>
                  {t("calendar.dismiss")}
                </Button>
                <button type="button" class="pg-cta" disabled={busy()} onClick={() => void accept(p())}>
                  {t("calendar.accept")}
                </button>
              </>
            )}
          </Show>
        }
      >
        <Show when={selectedProposal()}>{(p) => <ProposalDetail proposal={p()} when={proposalWhen(p(), locale())} />}</Show>
      </Sheet>

      <Sheet
        open={!!editor()}
        onClose={() => setEditor(null)}
        title={editor()?.mode === "edit" ? t("calendar.editorEdit") : t("calendar.editorNew")}
        class="cal-sheet"
        footer={
          <>
            <Button variant="ghost" onClick={() => setEditor(null)}>
              {t("calendar.cancel")}
            </Button>
            <button type="button" class="pg-cta" disabled={editor()?.saving} onClick={() => void saveEditor()}>
              {editor()?.saving ? t("calendar.saving") : t("calendar.save")}
            </button>
          </>
        }
      >
        <Show when={editor()}>
          {(ed) => (
            <EventEditor
              form={ed().form}
              error={ed().error}
              onChange={(form) => {
                const cur = editor();
                if (cur) setEditor({ ...cur, form, error: undefined });
              }}
              onSubmit={() => void saveEditor()}
            />
          )}
        </Show>
      </Sheet>

      <Sheet
        open={!!confirmDelete()}
        onClose={() => setConfirmDelete(null)}
        title={t("calendar.deleteTitle")}
        class="cal-sheet cal-sheet--confirm"
        footer={
          <>
            <Button variant="ghost" onClick={() => setConfirmDelete(null)}>
              {t("calendar.cancel")}
            </Button>
            <Button variant="danger" disabled={busy()} onClick={() => void doDelete()}>
              {busy() ? t("calendar.deleting") : t("calendar.delete")}
            </Button>
          </>
        }
      >
        <Show when={confirmDelete()}>
          {(ev) => (
            <>
              <p>{t("calendar.deleteConfirm", { title: ev().title, when: whenLabel(ev(), locale()) })}</p>
              <Show when={ev().recurring}>
                <p class="pg-muted">{t("calendar.deleteOccurrence")}</p>
              </Show>
            </>
          )}
        </Show>
      </Sheet>
    </Page>
  );
}
