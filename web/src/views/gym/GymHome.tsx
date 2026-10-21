import { A, useLocation, useNavigate } from "@solidjs/router";
import { createMemo, createSignal, For, Show } from "solid-js";
import type { GymCommand, GymView, Routine, Workout } from "../../api/gym";
import { formatDate } from "../../lib/units";
import { runGym } from "../../state/gym";
import { ArrowUpRightIcon, CalendarIcon, PlayIcon, PlusCircleIcon, SparkleIcon } from "./icons";
import RoutineBuilder from "./RoutineBuilder";
import { SettingsBody } from "./Settings";
import { ConfirmButton, exerciseById, Eyebrow, GymPage, isSettingsSearch, Metric, setsText, useGymClock } from "./shared";
import { gt } from "./strings";

// Training (RootView.swift `TrainingView`): hero, weekly goal, the active
// workout or a start button, routines, recent workouts to repeat, totals.

function RoutineCard(props: { view: GymView; routine: Routine; onStart: (r: Routine) => Promise<void>; starting: boolean }) {
  const names = createMemo(() =>
    props.routine.exercises
      .map((re) => exerciseById(props.view, re.exercise_id)?.name)
      .filter((n): n is string => Boolean(n))
      .join(" · "),
  );
  return (
    <li class="card gym-row routine-card">
      <div class="gym-row-main">
        <h3 class="gym-row-title">{props.routine.name}</h3>
        <p class="muted small gym-clamp">{names()}</p>
        <Eyebrow class="routine-count">{gt("exercisesCount", { count: props.routine.exercises.length })}</Eyebrow>
      </div>
      <div class="gym-row-actions">
        <button
          type="button"
          class="gym-play"
          disabled={props.starting}
          onClick={() => void props.onStart(props.routine)}
          aria-label={gt("startNamed", { name: props.routine.name })}
        >
          <PlayIcon />
        </button>
        <ConfirmButton
          label={gt("delete")}
          confirmLabel={gt("deleteRoutine")}
          ariaLabel={gt("deleteNamed", { name: props.routine.name })}
          onConfirm={() => runGym({ type: "delete_routine", routine_id: props.routine.id })}
        />
      </div>
    </li>
  );
}

function RepeatCard(props: { view: GymView; workout: Workout; onRepeat: (w: Workout) => Promise<void>; starting: boolean }) {
  const label = () => props.view.workout_labels[props.workout.id];
  return (
    <li class="card gym-row">
      <div class="gym-row-main">
        <Eyebrow>{formatDate(props.workout.finished_ms ?? props.workout.started_ms)}</Eyebrow>
        <h3 class="gym-row-title">{props.workout.title}</h3>
        <Show when={label()}>
          {(l) => (
            <p class="muted small">
              {l().duration} · {setsText(l().sets)} · {l().volume}
            </p>
          )}
        </Show>
      </div>
      <div class="gym-row-actions">
        <button
          type="button"
          class="btn btn--small"
          disabled={props.starting}
          onClick={() => void props.onRepeat(props.workout)}
          aria-label={gt("repeatNamed", { title: props.workout.title })}
        >
          {gt("repeat")}
        </button>
      </div>
    </li>
  );
}

/** The forest "IN PROGRESS" card with a live duration. */
function ActiveCard(props: { view: GymView; workout: Workout }) {
  const clock = useGymClock();
  const summary = () => props.view.summary;
  return (
    <section class="gym-active" aria-label={gt("workoutInProgress")}>
      <p class="gym-active-label">{gt("inProgress")}</p>
      <h2 class="gym-active-title">{props.workout.title}</h2>
      <p class="gym-active-meta">
        <span class="gym-active-duration" role="timer" aria-label={gt("workoutDuration")}>
          {clock.duration()}
        </span>{" "}
        · {setsText(summary().active_completed_sets)} · {summary().active_volume}
      </p>
      <A href="/gym/workout" class="gym-active-continue">
        <span>{gt("continueWorkout")}</span>
        <ArrowUpRightIcon />
      </A>
    </section>
  );
}

function TrainingBody(props: { view: GymView }) {
  const navigate = useNavigate();
  const [starting, setStarting] = createSignal(false);
  const [building, setBuilding] = createSignal(false);
  let createButton: HTMLButtonElement | undefined;
  const summary = () => props.view.summary;
  const recent = createMemo(() => props.view.state.history.slice(0, 3));
  const goal = () => Math.max(1, summary().weekly_goal);

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

  return (
    <>
      <header class="gym-hero">
        <div class="gym-hero-brand">
          <Eyebrow>{gt("brand")}</Eyebrow>
          <span class="gym-hero-sparkle">
            <SparkleIcon />
          </span>
        </div>
        <h1 class="gym-hero-title">
          {gt("heroLine1")}
          <br />
          {gt("heroLine2")}
        </h1>
      </header>

      <section class="gym-week" aria-label={gt("thisWeek")}>
        <div class="gym-week-head">
          <div>
            <Eyebrow>{gt("thisWeek")}</Eyebrow>
            <p class="gym-week-count">
              {gt("weekCount", { done: summary().weekly_workouts, goal: summary().weekly_goal })}
            </p>
          </div>
          <span class="gym-week-icon">
            <CalendarIcon />
          </span>
        </div>
        <progress
          class="weekly-bar"
          max={goal()}
          value={Math.min(summary().weekly_workouts, goal())}
          aria-label={gt("weekProgress")}
        />
      </section>

      <Show
        when={props.view.state.active}
        fallback={
          <button
            type="button"
            class="btn btn--primary btn--block gym-primary gym-start"
            disabled={starting()}
            onClick={() => void start({ type: "start_workout", title: gt("defaultWorkoutTitle") })}
          >
            <PlayIcon /> {gt("startEmpty")}
          </button>
        }
      >
        {(active) => <ActiveCard view={props.view} workout={active()} />}
      </Show>

      <section class="gym-section">
        <div class="gym-section-head">
          <h2 class="section-title">{gt("yourRoutines")}</h2>
          <button
            type="button"
            class="icon-btn gym-add"
            ref={createButton}
            aria-label={gt("createRoutine")}
            onClick={() => setBuilding(true)}
          >
            <PlusCircleIcon />
          </button>
        </div>
        <Show
          when={props.view.state.routines.length > 0}
          fallback={<p class="muted small">{gt("routinesEmpty")}</p>}
        >
          <ul class="gym-list">
            <For each={props.view.state.routines}>
              {(routine) => (
                <RoutineCard
                  view={props.view}
                  routine={routine}
                  starting={starting() || Boolean(props.view.state.active)}
                  onStart={(r) => start({ type: "start_workout", title: r.name, routine_id: r.id })}
                />
              )}
            </For>
          </ul>
        </Show>
      </section>

      <Show when={!props.view.state.active && recent().length > 0}>
        <section class="gym-section">
          <h2 class="section-title">{gt("repeatTitle")}</h2>
          <ul class="gym-list">
            <For each={recent()}>
              {(workout) => (
                <RepeatCard
                  view={props.view}
                  workout={workout}
                  starting={starting()}
                  onRepeat={(w) => start({ type: "repeat_workout", workout_id: w.id })}
                />
              )}
            </For>
          </ul>
        </section>
      </Show>

      <section class="gym-metrics" aria-label={gt("totals")}>
        <Metric value={summary().workouts} label={gt("sessions")} />
        <Metric value={summary().completed_sets} label={gt("setsLogged")} />
        <Metric value={summary().volume} label={gt("totalVolume")} />
      </section>

      <p class="gym-footer">{gt("footer")}</p>

      <RoutineBuilder
        open={building()}
        onClose={() => {
          setBuilding(false);
          createButton?.focus();
        }}
      />
    </>
  );
}

export default function GymHome() {
  const location = useLocation();
  const settings = createMemo(() => isSettingsSearch(location.search));
  return (
    <GymPage>
      {(view) => (
        <Show when={settings()} fallback={<TrainingBody view={view()} />}>
          <SettingsBody view={view()} />
        </Show>
      )}
    </GymPage>
  );
}
