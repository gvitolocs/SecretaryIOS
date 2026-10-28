import { useNavigate } from "@solidjs/router";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { GymCommand, GymView, Routine, Workout } from "../../api/gym";
import { Button, Card, EmptyState, ListRow, PageGrid, RowList } from "../../components/page";
import { runGym } from "../../state/gym";
import { daysBetween, exerciseSummary, nextRoutine, recentStats, routineLastDone, weekDays, type WeekDay } from "./desk";
import { PlayIcon, PlusCircleIcon } from "./icons";
import RoutineBuilder from "./RoutineBuilder";
import { JOURNAL_HREF, setsText, useGymClock } from "./shared";
import { gt, gymLocale } from "./strings";
import WeightCard from "./WeightCard";
import "../../styles/gym-desk.css";

// The desktop Training overview (GymHome on a page at least WIDE_MIN_PX wide),
// on the page system (components/page): a main column (this week, routines,
// recent workouts) and a sticky rail (weight, last 30 days). The header row
// is GymPage's, shared by every Gym section. Styles: styles/gym-desk.css.
// The phone layout stays in GymHome.

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
    <section class="pg-card trd-week" aria-labelledby="trd-week-title">
      <div class="trd-week-text">
        <h2 class="pg-card-title" id="trd-week-title">
          {gt("thisWeek")}
        </h2>
        <p class="trd-week-count">
          <span class="pg-num">{gt("weekOf", { done: done(), goal: goal() })}</span>{" "}
          <span class="pg-muted">{done() >= goal() && goal() > 0 ? gt("weekGoalMet") : gt("weekCaption")}</span>
        </p>
        <Show when={props.next}>
          {(next) => (
            <p class="pg-muted trd-week-next">
              {gt("nextUp")} <span class="pg-strong">{next().name}</span>
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
        class="pg-icon-btn"
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
              <button type="button" role="menuitem" class="pg-btn pg-btn--danger" disabled={pending()} onClick={() => void remove()}>
                {gt("delete")}
              </button>
              <button type="button" role="menuitem" class="pg-btn pg-btn--ghost" disabled={pending()} onClick={() => close(true)}>
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
    <ListRow
      leading={
        <button
          type="button"
          class="trd-play"
          disabled={props.disabled}
          onClick={() => props.onStart(props.routine)}
          aria-label={gt("startNamed", { name: props.routine.name })}
        >
          <PlayIcon />
        </button>
      }
      title={props.routine.name}
      badge={props.next ? gt("nextBadge") : undefined}
      sub={
        <>
          {summary().names.join(" · ")}
          <Show when={summary().more > 0}>
            <span class="trd-more"> {gt("moreCount", { count: summary().more })}</span>
          </Show>
        </>
      }
      meta={props.lastDone !== undefined ? gt("lastDone", { when: relativeDays(props.lastDone, props.now) }) : gt("notDoneYet")}
      trailing={<RoutineMenu routine={props.routine} />}
    />
  );
}

function WorkoutRow(props: { view: GymView; workout: Workout; disabled: boolean; onRepeat: (w: Workout) => void }) {
  const label = () => props.view.workout_labels[props.workout.id];
  return (
    <ListRow
      title={props.workout.title}
      href={`${JOURNAL_HREF}/${props.workout.id}`}
      sub={
        <>
          {shortDate(props.workout.finished_ms ?? props.workout.started_ms)}
          <Show when={label()}>{(l) => ` · ${l().duration} · ${setsText(l().sets)} · ${l().volume}`}</Show>
        </>
      }
      trailing={
        <Button
          variant="ghost"
          disabled={props.disabled}
          onClick={() => props.onRepeat(props.workout)}
          aria-label={gt("repeatNamed", { title: props.workout.title })}
        >
          {gt("repeat")}
        </Button>
      }
    />
  );
}

/** The workout in progress, as a quiet card; the header's button continues it. */
function ActiveRow(props: { view: GymView; workout: Workout }) {
  const clock = useGymClock();
  return (
    <section class="pg-card trd-active" aria-label={gt("workoutInProgress")}>
      <span class="trd-live" aria-hidden="true" />
      <div class="pg-row-main">
        <p class="pg-muted trd-active-label">{gt("inProgress")}</p>
        <p class="pg-row-title">
          <span class="pg-row-name">{props.workout.title}</span>
        </p>
      </div>
      <p class="pg-num trd-active-meta">
        <span role="timer" aria-label={gt("workoutDuration")}>
          {clock.duration()}
        </span>
        <span class="pg-muted">
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
    <Card title={gt("last30")}>
      <Show when={stats().sessions > 0} fallback={<EmptyState text={gt("last30Empty")} />}>
        <dl class="pg-stats">
          <div class="pg-stat">
            <dt class="pg-muted">{gt("sessions")}</dt>
            <dd class="pg-num">{stats().sessions}</dd>
          </div>
          <div class="pg-stat">
            <dt class="pg-muted">{gt("volume")}</dt>
            <dd class="pg-num">{volume()}</dd>
          </div>
          <Show when={stats().best}>
            {(best) => (
              <div class="pg-stat">
                <dt class="pg-muted">{gt("bestLift")}</dt>
                <dd class="pg-num">
                  {best().exercise}{" "}
                  <span class="pg-muted">
                    {props.view.weight_labels[best().set.id] ?? ""} {props.view.state.settings.weight_unit} × {best().set.reps}
                  </span>
                </dd>
              </div>
            )}
          </Show>
        </dl>
      </Show>
    </Card>
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
      <PageGrid
        rail={
          <>
            <WeightCard />
            <LastDays view={props.view} now={now()} />
          </>
        }
      >
        <Show when={props.view.state.active}>{(active) => <ActiveRow view={props.view} workout={active()} />}</Show>

        <WeekCard view={props.view} days={days()} next={next()} />

        <Card
          list
          title={gt("routinesTitle")}
          actions={
            <Show when={props.view.state.routines.length > 0}>
              <Button variant="ghost" ref={createButton} onClick={() => setBuilding(true)}>
                <PlusCircleIcon />
                {gt("newRoutine")}
              </Button>
            </Show>
          }
        >
          <Show
            when={props.view.state.routines.length > 0}
            fallback={
              <EmptyState
                text={gt("routinesEmptyDesk")}
                action={
                  <Button ref={createButton} onClick={() => setBuilding(true)}>
                    <PlusCircleIcon />
                    {gt("createRoutine")}
                  </Button>
                }
              />
            }
          >
            <RowList>
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
            </RowList>
          </Show>
        </Card>

        <Card
          list
          title={gt("recentTitle")}
          actions={
            <Show when={history().length > 0}>
              <a href={JOURNAL_HREF} class="pg-btn pg-btn--ghost">
                {gt("openJournal")}
              </a>
            </Show>
          }
        >
          <Show
            when={recent().length > 0}
            fallback={
              <EmptyState
                text={gt("recentEmpty")}
                action={
                  <Show when={!props.view.state.active}>
                    <button
                      type="button"
                      class="pg-link"
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
                }
              />
            }
          >
            <RowList>
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
            </RowList>
          </Show>
        </Card>
      </PageGrid>

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
