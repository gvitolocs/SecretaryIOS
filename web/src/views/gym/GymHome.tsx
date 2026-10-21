import { A, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import type { GymView, Routine, Workout } from "../../api/gym";
import { formatDate } from "../../lib/units";
import { runGym } from "../../state/gym";
import { ConfirmButton, exerciseById, GymPage, setsText } from "./shared";

function RoutineCard(props: { view: GymView; routine: Routine; onStart: (r: Routine) => Promise<void>; starting: boolean }) {
  const names = createMemo(() =>
    props.routine.exercises
      .map((re) => exerciseById(props.view, re.exercise_id)?.name)
      .filter((n): n is string => Boolean(n))
      .join(" · "),
  );
  return (
    <li class="card gym-row">
      <div class="gym-row-main">
        <h3 class="gym-row-title">{props.routine.name}</h3>
        <p class="muted small">{names()}</p>
        <p class="eyebrow">{props.routine.exercises.length} exercises</p>
      </div>
      <div class="gym-row-actions">
        <button
          type="button"
          class="btn btn--primary btn--small"
          disabled={props.starting}
          onClick={() => void props.onStart(props.routine)}
          aria-label={`Start ${props.routine.name}`}
        >
          Start
        </button>
        <ConfirmButton
          label="Delete"
          confirmLabel="Delete routine"
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
        <p class="eyebrow">{formatDate(props.workout.finished_ms ?? props.workout.started_ms)}</p>
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
          aria-label={`Repeat ${props.workout.title}`}
        >
          Repeat
        </button>
      </div>
    </li>
  );
}

function GymSettings(props: { view: GymView }) {
  const [unit, setUnit] = createSignal("kg");
  const [rest, setRest] = createSignal("90");
  const [goal, setGoal] = createSignal("3");
  const [error, setError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);

  createEffect(() => {
    const s = props.view.state.settings;
    setUnit(s.weight_unit);
    setRest(String(s.rest_seconds));
    setGoal(String(s.weekly_goal));
  });

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (saving()) return;
    if (!/^\d+$/.test(rest().trim()) || !/^\d+$/.test(goal().trim())) {
      setError("Rest time and weekly goal must be whole numbers.");
      return;
    }
    setError(null);
    setSaving(true);
    try {
      await runGym({
        type: "update_settings",
        weight_unit: unit(),
        rest_seconds: Number(rest().trim()),
        weekly_goal: Number(goal().trim()),
      });
    } finally {
      setSaving(false);
    }
  }

  return (
    <details class="settings-block">
      <summary>Training settings</summary>
      <form class="settings-form" onSubmit={save}>
        <label class="field-label" for="gym-unit">
          Weight unit
        </label>
        <select id="gym-unit" class="text-input" value={unit()} onChange={(e) => setUnit(e.currentTarget.value)}>
          <option value="kg">kg</option>
          <option value="lb">lb</option>
        </select>
        <label class="field-label" for="gym-rest">
          Default rest (seconds)
        </label>
        <input
          id="gym-rest"
          class="text-input"
          inputmode="numeric"
          value={rest()}
          onInput={(e) => setRest(e.currentTarget.value)}
        />
        <label class="field-label" for="gym-goal">
          Workouts per week
        </label>
        <input
          id="gym-goal"
          class="text-input"
          inputmode="numeric"
          value={goal()}
          onInput={(e) => setGoal(e.currentTarget.value)}
        />
        <Show when={error()}>{(msg) => <p class="field-error">{msg()}</p>}</Show>
        <button type="submit" class="btn btn--primary" disabled={saving()}>
          Save settings
        </button>
      </form>
    </details>
  );
}

function GymHomeBody(props: { view: GymView }) {
  const navigate = useNavigate();
  const [starting, setStarting] = createSignal(false);
  const summary = () => props.view.summary;
  const recent = createMemo(() => props.view.state.history.slice(0, 3));

  async function start(command: Parameters<typeof runGym>[0]) {
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
      <section class="card weekly" aria-label="This week">
        <div class="weekly-head">
          <span class="eyebrow">This week</span>
          <span class="weekly-count">
            {summary().weekly_workouts} / {summary().weekly_goal} workouts
          </span>
        </div>
        <progress
          class="weekly-bar"
          max={Math.max(1, summary().weekly_goal)}
          value={Math.min(summary().weekly_workouts, Math.max(1, summary().weekly_goal))}
          aria-label="Weekly goal progress"
        />
      </section>

      <Show
        when={props.view.state.active}
        fallback={
          <button
            type="button"
            class="btn btn--primary btn--block gym-start"
            disabled={starting()}
            onClick={() => void start({ type: "start_workout", title: "Workout" })}
          >
            Start empty workout
          </button>
        }
      >
        {(active) => (
          <section class="card active-card" aria-label="Workout in progress">
            <p class="eyebrow">In progress</p>
            <h2 class="active-title">{active().title}</h2>
            <p class="small">
              {summary().active_duration} · {setsText(summary().active_completed_sets)} · {summary().active_volume}
            </p>
            <A href="/gym/workout" class="btn btn--primary btn--block btn-link">
              Continue workout
            </A>
          </section>
        )}
      </Show>

      <section class="gym-section">
        <h2 class="section-title">Your routines</h2>
        <Show
          when={props.view.state.routines.length > 0}
          fallback={<p class="muted small">Save a workout as a routine to reuse it here.</p>}
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
          <h2 class="section-title">Repeat a workout</h2>
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

      <section class="metric-row" aria-label="Totals">
        <div class="metric-tile">
          <span class="metric-value">{summary().workouts}</span>
          <span class="metric-label">Sessions</span>
        </div>
        <div class="metric-tile">
          <span class="metric-value">{summary().completed_sets}</span>
          <span class="metric-label">Sets logged</span>
        </div>
        <div class="metric-tile">
          <span class="metric-value">{summary().volume}</span>
          <span class="metric-label">Total volume</span>
        </div>
      </section>

      <GymSettings view={props.view} />
    </>
  );
}

export default function GymHome() {
  return <GymPage>{(view) => <GymHomeBody view={view()} />}</GymPage>;
}
