import { A, useLocation, useNavigate } from "@solidjs/router";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { GymCommand, GymView, Routine, Workout } from "../../api/gym";
import { runGym } from "../../state/gym";
import { daysBetween, exerciseSummary, nextRoutine, recentStats, routineLastDone, weekDays, type WeekDay } from "./desk";
import { PlayIcon, PlusCircleIcon } from "./icons";
import RoutineBuilder from "./RoutineBuilder";
import { isSettingsSearch, JOURNAL_HREF, SETTINGS_HREF, setsText, useGymClock } from "./shared";
import { gt, gymLocale } from "./strings";
import WeightCard from "./WeightCard";
import "../../styles/gym-desk.css";

// The desktop Training page (GymHome on a page at least WIDE_MIN_PX wide). One
// header row (title, date, sections, the one accent button), a main column
// (this week, routines, recent workouts) and a sticky rail (weight, last 30
// days). Styles: styles/gym-desk.css. The phone layout stays in GymHome.

function relativeDays(ms: number, now: number): string {
  const days = daysBetween(ms, now);
  const rtf = new Intl.RelativeTimeFormat(gymLocale(), { numeric: "auto" });
  if (days >= 14) return rtf.format(-Math.round(days / 7), "week");
  return rtf.format(-days, "day");
}

function shortDate(ms: number): string {
  return new Intl.DateTimeFormat(gymLocale(), { month: "short", day: "numeric" }).format(new Date(ms));
}

function weekdayLetter(ms: number): string {
  return new Intl.DateTimeFormat(gymLocale(), { weekday: "narrow" }).format(new Date(ms));
}

function weekdayName(ms: number): string {
  return new Intl.DateTimeFormat(gymLocale(), { weekday: "long" }).format(new Date(ms));
}

function CheckIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 12.5l4 4 8-9" />
    </svg>
  );
}

function MoreIcon() {
  return (
    <svg class="icon icon--fill" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="6" cy="12" r="1.6" />
      <circle cx="12" cy="12" r="1.6" />
      <circle cx="18" cy="12" r="1.6" />
    </svg>
  );
}

function GearIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
    </svg>
  );
}

/** Overview · Journal · Progress · settings: the Gym sections as one compact control. */
function DeskNav() {
  const location = useLocation();
  const path = () => location.pathname.replace(/\/+$/, "") || "/";
  const overview = () => (path() === "/gym" || path() === "/training") && !isSettingsSearch(location.search);
  const items = [
    { href: "/gym", label: () => gt("tabOverview"), current: overview },
    { href: JOURNAL_HREF, label: () => gt("tabJournal"), current: () => path().startsWith("/gym/history") },
    { href: "/gym/progress", label: () => gt("tabProgress"), current: () => path().startsWith("/gym/progress") },
  ];
  return (
    <nav class="trd-seg" aria-label={gt("sections")}>
      <For each={items}>
        {(item) => (
          <a href={item.href} class="trd-seg-item" aria-current={item.current() ? "page" : undefined}>
            {item.label()}
          </a>
        )}
      </For>
      <a href={SETTINGS_HREF} class="trd-seg-item trd-seg-icon" aria-label={gt("tabSettings")} title={gt("tabSettings")}>
        <GearIcon />
      </a>
    </nav>
  );
}

function WeekCard(props: { view: GymView; days: WeekDay[]; next: Routine | null }) {
  const done = () => props.view.summary.weekly_workouts;
  const goal = () => props.view.summary.weekly_goal;
  const dayLabel = (d: WeekDay) => {
    const name = weekdayName(d.ms);
    if (d.done) return gt("dayDone", { day: name });
    if (d.today) return gt("dayToday", { day: name });
    return name;
  };
  return (
    <section class="trd-card trd-week" aria-labelledby="trd-week-title">
      <div class="trd-week-text">
        <h2 class="trd-card-title" id="trd-week-title">
          {gt("thisWeek")}
        </h2>
        <p class="trd-week-count">
          <span class="trd-num">{gt("weekOf", { done: done(), goal: goal() })}</span>{" "}
          <span class="trd-muted">{done() >= goal() && goal() > 0 ? gt("weekGoalMet") : gt("weekCaption")}</span>
        </p>
        <Show when={props.next}>
          {(next) => (
            <p class="trd-muted trd-week-next">
              {gt("nextUp")} <span class="trd-strong">{next().name}</span>
            </p>
          )}
        </Show>
      </div>
      <ol class="trd-days">
        <For each={props.days}>
          {(d) => (
            <li
              class="trd-day"
              classList={{
                "trd-day--done": d.done,
                "trd-day--today": d.today,
                "trd-day--future": d.future && !d.done,
              }}
              aria-label={dayLabel(d)}
            >
              <span class="trd-day-letter" aria-hidden="true">
                {weekdayLetter(d.ms)}
              </span>
              <span class="trd-day-dot" aria-hidden="true">
                <Show when={d.done}>
                  <CheckIcon />
                </Show>
              </span>
            </li>
          )}
        </For>
      </ol>
    </section>
  );
}

/** The ⋯ menu of a routine row: delete, with a confirm step. */
function RoutineMenu(props: { routine: Routine }) {
  const [open, setOpen] = createSignal(false);
  const [confirming, setConfirming] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  let root: HTMLDivElement | undefined;
  let trigger: HTMLButtonElement | undefined;

  function close(focus = false) {
    setOpen(false);
    setConfirming(false);
    if (focus) trigger?.focus();
  }

  onMount(() => {
    const onDown = (e: PointerEvent) => {
      if (open() && root && !root.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (open() && e.key === "Escape") close(true);
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey);
    });
  });

  async function remove() {
    if (pending()) return;
    setPending(true);
    try {
      await runGym({ type: "delete_routine", routine_id: props.routine.id });
    } finally {
      setPending(false);
      close();
    }
  }

  return (
    <div class="trd-menu" ref={root}>
      <button
        type="button"
        class="trd-icon-btn"
        ref={trigger}
        aria-haspopup="menu"
        aria-expanded={open() ? "true" : "false"}
        aria-label={gt("routineMenu", { name: props.routine.name })}
        onClick={() => (open() ? close() : setOpen(true))}
      >
        <MoreIcon />
      </button>
      <Show when={open()}>
        <div class="trd-menu-pop" role="menu">
          <Show
            when={confirming()}
            fallback={
              <button type="button" role="menuitem" class="trd-menu-item trd-menu-item--danger" onClick={() => setConfirming(true)}>
                {gt("deleteRoutine")}
              </button>
            }
          >
            <p class="trd-menu-prompt">{gt("deleteConfirm", { name: props.routine.name })}</p>
            <div class="trd-menu-actions">
              <button type="button" role="menuitem" class="trd-btn trd-btn--danger" disabled={pending()} onClick={() => void remove()}>
                {gt("delete")}
              </button>
              <button type="button" role="menuitem" class="trd-btn trd-btn--ghost" disabled={pending()} onClick={() => close(true)}>
                {gt("cancel")}
              </button>
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

function RoutineRow(props: {
  view: GymView;
  routine: Routine;
  next: boolean;
  lastDone: number | undefined;
  now: number;
  disabled: boolean;
  onStart: (r: Routine) => void;
}) {
  const summary = createMemo(() => exerciseSummary(props.view, props.routine));
  return (
    <li class="trd-row">
      <button
        type="button"
        class="trd-play"
        disabled={props.disabled}
        onClick={() => props.onStart(props.routine)}
        aria-label={gt("startNamed", { name: props.routine.name })}
      >
        <PlayIcon />
      </button>
      <div class="trd-row-main">
        <p class="trd-row-title">
          <span class="trd-row-name">{props.routine.name}</span>
          <Show when={props.next}>
            <span class="trd-badge">{gt("nextBadge")}</span>
          </Show>
        </p>
        <p class="trd-row-sub trd-muted">
          {summary().names.join(" · ")}
          <Show when={summary().more > 0}>
            <span class="trd-more"> {gt("moreCount", { count: summary().more })}</span>
          </Show>
        </p>
      </div>
      <p class="trd-row-meta trd-muted">
        {props.lastDone !== undefined ? gt("lastDone", { when: relativeDays(props.lastDone, props.now) }) : gt("notDoneYet")}
      </p>
      <RoutineMenu routine={props.routine} />
    </li>
  );
}

function WorkoutRow(props: { view: GymView; workout: Workout; disabled: boolean; onRepeat: (w: Workout) => void }) {
  const label = () => props.view.workout_labels[props.workout.id];
  return (
    <li class="trd-row">
      <div class="trd-row-main">
        <p class="trd-row-title">
          <A href={`${JOURNAL_HREF}/${props.workout.id}`} class="trd-row-name trd-row-link">
            {props.workout.title}
          </A>
        </p>
        <p class="trd-row-sub trd-muted trd-num">
          {shortDate(props.workout.finished_ms ?? props.workout.started_ms)}
          <Show when={label()}>{(l) => ` · ${l().duration} · ${setsText(l().sets)} · ${l().volume}`}</Show>
        </p>
      </div>
      <button
        type="button"
        class="trd-btn trd-btn--ghost"
        disabled={props.disabled}
        onClick={() => props.onRepeat(props.workout)}
        aria-label={gt("repeatNamed", { title: props.workout.title })}
      >
        {gt("repeat")}
      </button>
    </li>
  );
}

/** The workout in progress, as a quiet card; the header's button continues it. */
function ActiveRow(props: { view: GymView; workout: Workout }) {
  const clock = useGymClock();
  return (
    <section class="trd-card trd-active" aria-label={gt("workoutInProgress")}>
      <span class="trd-live" aria-hidden="true" />
      <div class="trd-row-main">
        <p class="trd-muted trd-active-label">{gt("inProgress")}</p>
        <p class="trd-row-title">
          <span class="trd-row-name">{props.workout.title}</span>
        </p>
      </div>
      <p class="trd-num trd-active-meta">
        <span role="timer" aria-label={gt("workoutDuration")}>
          {clock.duration()}
        </span>
        <span class="trd-muted">
          {" "}
          · {setsText(props.view.summary.active_completed_sets)} · {props.view.summary.active_volume}
        </span>
      </p>
    </section>
  );
}

function LastDays(props: { view: GymView; now: number }) {
  const stats = createMemo(() => recentStats(props.view, props.now));
  const volume = () => {
    const v = stats().volume;
    if (v === null) return "—";
    // Like the server's volume labels ("6350 kg"), so the rows and this total read alike.
    const n = new Intl.NumberFormat(gymLocale(), { maximumFractionDigits: 0, useGrouping: false }).format(v);
    return `${n} ${props.view.state.settings.weight_unit}`;
  };
  return (
    <section class="trd-card" aria-labelledby="trd-last-title">
      <h2 class="trd-card-title" id="trd-last-title">
        {gt("last30")}
      </h2>
      <Show when={stats().sessions > 0} fallback={<p class="trd-empty">{gt("last30Empty")}</p>}>
        <dl class="trd-stats">
          <div class="trd-stat">
            <dt class="trd-muted">{gt("sessions")}</dt>
            <dd class="trd-num">{stats().sessions}</dd>
          </div>
          <div class="trd-stat">
            <dt class="trd-muted">{gt("volume")}</dt>
            <dd class="trd-num">{volume()}</dd>
          </div>
          <Show when={stats().best}>
            {(best) => (
              <div class="trd-stat">
                <dt class="trd-muted">{gt("bestLift")}</dt>
                <dd class="trd-num">
                  {best().exercise}{" "}
                  <span class="trd-muted">
                    {props.view.weight_labels[best().set.id] ?? ""} {props.view.state.settings.weight_unit} × {best().set.reps}
                  </span>
                </dd>
              </div>
            )}
          </Show>
        </dl>
      </Show>
    </section>
  );
}

export default function TrainingDesk(props: { view: GymView }) {
  const navigate = useNavigate();
  const [starting, setStarting] = createSignal(false);
  const [building, setBuilding] = createSignal(false);
  let createButton: HTMLButtonElement | undefined;

  // The day changes rarely; a minute tick keeps "today" and "N days ago" right on a page left open.
  const [now, setNow] = createSignal(Date.now());
  onMount(() => {
    const timer = setInterval(() => setNow(Date.now()), 60_000);
    onCleanup(() => clearInterval(timer));
  });

  const history = () => props.view.state.history;
  const days = createMemo(() => weekDays(history(), now()));
  const next = createMemo(() => nextRoutine(props.view));
  const lastDone = createMemo(() => routineLastDone(props.view));
  const recent = createMemo(() => history().slice(0, 5));
  const busy = () => starting() || Boolean(props.view.state.active);
  const today = () =>
    new Intl.DateTimeFormat(gymLocale(), { weekday: "long", day: "numeric", month: "long" }).format(new Date(now()));

  async function start(command: GymCommand) {
    if (starting()) return;
    setStarting(true);
    try {
      const res = await runGym(command);
      if (res?.ok) navigate("/gym/workout");
    } finally {
      setStarting(false);
    }
  }

  const startRoutine = (r: Routine) => void start({ type: "start_workout", title: r.name, routine_id: r.id });

  return (
    <div class="trd">
      <header class="trd-head">
        <div class="trd-titles">
          <h1 class="trd-title">{gt("tabTraining")}</h1>
          <p class="trd-muted trd-date">{today()}</p>
        </div>
        <div class="trd-head-actions">
          <DeskNav />
          <Show
            when={props.view.state.active}
            fallback={
              <button
                type="button"
                class="trd-cta"
                disabled={starting()}
                onClick={() => void start({ type: "start_workout", title: gt("defaultWorkoutTitle") })}
              >
                <PlayIcon />
                {gt("startWorkout")}
              </button>
            }
          >
            <A href="/gym/workout" class="trd-cta">
              <PlayIcon />
              {gt("continueWorkout")}
            </A>
          </Show>
        </div>
      </header>

      <div class="trd-grid">
        <div class="trd-main">
          <Show when={props.view.state.active}>{(active) => <ActiveRow view={props.view} workout={active()} />}</Show>

          <WeekCard view={props.view} days={days()} next={next()} />

          <section class="trd-card trd-list-card" aria-labelledby="trd-routines-title">
            <div class="trd-card-head">
              <h2 class="trd-card-title" id="trd-routines-title">
                {gt("routinesTitle")}
              </h2>
              <Show when={props.view.state.routines.length > 0}>
                <button type="button" class="trd-btn trd-btn--ghost" ref={createButton} onClick={() => setBuilding(true)}>
                  <PlusCircleIcon />
                  {gt("newRoutine")}
                </button>
              </Show>
            </div>
            <Show
              when={props.view.state.routines.length > 0}
              fallback={
                <div class="trd-empty-block">
                  <p class="trd-empty">{gt("routinesEmptyDesk")}</p>
                  <button type="button" class="trd-btn" ref={createButton} onClick={() => setBuilding(true)}>
                    <PlusCircleIcon />
                    {gt("createRoutine")}
                  </button>
                </div>
              }
            >
              <ul class="trd-rows">
                <For each={props.view.state.routines}>
                  {(routine) => (
                    <RoutineRow
                      view={props.view}
                      routine={routine}
                      next={next()?.id === routine.id}
                      lastDone={lastDone().get(routine.id)}
                      now={now()}
                      disabled={busy()}
                      onStart={startRoutine}
                    />
                  )}
                </For>
              </ul>
            </Show>
          </section>

          <section class="trd-card trd-list-card" aria-labelledby="trd-recent-title">
            <div class="trd-card-head">
              <h2 class="trd-card-title" id="trd-recent-title">
                {gt("recentTitle")}
              </h2>
              <Show when={history().length > 0}>
                <A href={JOURNAL_HREF} class="trd-btn trd-btn--ghost">
                  {gt("openJournal")}
                </A>
              </Show>
            </div>
            <Show
              when={recent().length > 0}
              fallback={
                <div class="trd-empty-block">
                  <p class="trd-empty">{gt("recentEmpty")}</p>
                  <Show when={!props.view.state.active}>
                    <button
                      type="button"
                      class="trd-link"
                      disabled={starting()}
                      onClick={() => {
                        const r = next();
                        if (r) startRoutine(r);
                        else void start({ type: "start_workout", title: gt("defaultWorkoutTitle") });
                      }}
                    >
                      {gt("startFirst")}
                    </button>
                  </Show>
                </div>
              }
            >
              <ul class="trd-rows">
                <For each={recent()}>
                  {(workout) => (
                    <WorkoutRow
                      view={props.view}
                      workout={workout}
                      disabled={busy()}
                      onRepeat={(w) => void start({ type: "repeat_workout", workout_id: w.id })}
                    />
                  )}
                </For>
              </ul>
            </Show>
          </section>
        </div>

        <aside class="trd-rail">
          <WeightCard />
          <LastDays view={props.view} now={now()} />
        </aside>
      </div>

      <RoutineBuilder
        open={building()}
        onClose={() => {
          setBuilding(false);
          createButton?.focus();
        }}
      />
    </div>
  );
}
