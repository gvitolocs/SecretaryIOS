import { A, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import type { ExerciseKind, GymView, SetKind, WorkoutExercise, WorkoutSet } from "../../api/gym";
import { PlusIcon } from "../../components/Icons";
import { formatCountdown, formatDuration } from "../../lib/units";
import { gym, restRemainingNow, runGym, tickClock } from "../../state/gym";
import ExercisePicker from "./Picker";
import { ConfirmButton, exerciseById, GymPage, SET_KIND_LABELS, setBadge } from "./shared";

const CLOCK_POLL_MS = 15_000;
const INT_RE = /^\d+$/;

type Field = "weight" | "reps" | "seconds" | "distance";

function fieldsFor(kind: ExerciseKind): Field[] {
  switch (kind) {
    case "reps_only":
      return ["reps"];
    case "timed":
      return ["seconds"];
    case "distance_time":
      return ["distance", "seconds"];
    default:
      return ["weight", "reps"];
  }
}

const FIELD_LABELS: Record<Field, string> = {
  weight: "Weight",
  reps: "Reps",
  seconds: "Seconds",
  distance: "Meters",
};

function SetRow(props: { view: GymView; set: WorkoutSet; index: number; kind: ExerciseKind }) {
  const [pending, setPending] = createSignal(false);
  const [focused, setFocused] = createSignal<Field | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [values, setValues] = createSignal<Record<Field, string>>({ weight: "0", reps: "0", seconds: "0", distance: "0" });

  const unit = () => props.view.state.settings.weight_unit;
  const fields = createMemo(() => fieldsFor(props.kind));
  const server = createMemo<Record<Field, string>>(() => ({
    weight: props.view.weight_labels[props.set.id] ?? "0",
    reps: String(props.set.reps),
    seconds: String(props.set.seconds),
    distance: String(props.set.distance_meters),
  }));

  // Follow the server copy (e.g. after a conflict refresh) unless the user is typing.
  createEffect(
    on(server, (s) => {
      const f = focused();
      setValues((v) => (f ? { ...s, [f]: v[f] } : s));
    }),
  );

  const dirty = () => fields().some((f) => values()[f].trim() !== server()[f]);

  function buildEdit(kind: SetKind) {
    const v = values();
    for (const f of fields()) {
      if (f !== "weight" && !INT_RE.test(v[f].trim())) {
        setError(`${FIELD_LABELS[f]} must be a whole number.`);
        return null;
      }
    }
    setError(null);
    const has = (f: Field) => fields().includes(f);
    return {
      type: "edit_set" as const,
      set_id: props.set.id,
      weight: has("weight") ? v.weight.trim() : "0",
      reps: has("reps") ? Number(v.reps.trim()) : props.set.reps,
      seconds: has("seconds") ? Number(v.seconds.trim()) : props.set.seconds,
      distance_meters: has("distance") ? Number(v.distance.trim()) : props.set.distance_meters,
      kind,
    };
  }

  async function run<T>(fn: () => Promise<T>): Promise<T | undefined> {
    if (pending()) return undefined;
    setPending(true);
    try {
      return await fn();
    } finally {
      setPending(false);
    }
  }

  function commit() {
    if (props.set.completed || !dirty()) return;
    const edit = buildEdit(props.set.kind);
    if (!edit) return;
    void run(() => runGym(edit));
  }

  function changeKind(kind: SetKind) {
    const edit = buildEdit(kind);
    if (!edit) return;
    void run(() => runGym(edit));
  }

  function toggleDone() {
    if (props.set.completed) {
      void run(() => runGym({ type: "complete_set", set_id: props.set.id, completed: false }));
      return;
    }
    let edit = null;
    if (dirty()) {
      edit = buildEdit(props.set.kind);
      if (!edit) return;
    }
    const setId = props.set.id;
    void run(async () => {
      if (edit) {
        const res = await runGym(edit);
        if (!res?.ok) return;
      }
      await runGym({ type: "complete_set", set_id: setId, completed: true });
    });
  }

  const label = () => `Set ${props.index + 1}`;

  return (
    <li
      class="set-row"
      classList={{ "set-row--done": props.set.completed, "set-row--pending": pending() }}
      aria-busy={pending()}
    >
      <span class="set-badge" classList={{ "set-badge--flag": props.set.kind !== "normal" }}>
        <span aria-hidden="true">{setBadge(props.set.kind, props.index)}</span>
        <select
          class="set-badge-select"
          aria-label={`${label()} type`}
          value={props.set.kind}
          disabled={props.set.completed || pending()}
          onChange={(e) => {
            const next = e.currentTarget.value as SetKind;
            e.currentTarget.value = props.set.kind;
            changeKind(next);
          }}
        >
          <For each={Object.keys(SET_KIND_LABELS) as SetKind[]}>
            {(k) => <option value={k}>{SET_KIND_LABELS[k]}</option>}
          </For>
        </select>
      </span>
      <div class="set-fields" classList={{ "set-fields--two": fields().length === 2 }}>
        <For each={fields()}>
          {(f) => (
            <label class="set-field">
              <span class="sr-only">
                {label()} {FIELD_LABELS[f]}
                {f === "weight" ? ` (${unit()})` : ""}
              </span>
              <input
                class="set-input"
                inputmode={f === "weight" ? "decimal" : "numeric"}
                autocomplete="off"
                value={values()[f]}
                disabled={props.set.completed || pending()}
                onFocus={() => setFocused(f)}
                onBlur={() => setFocused(null)}
                onInput={(e) => {
                  const text = e.currentTarget.value;
                  setValues((v) => ({ ...v, [f]: text }));
                }}
                onChange={commit}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                }}
              />
              <span class="set-suffix" aria-hidden="true">
                {f === "weight" ? unit() : f === "reps" ? "reps" : f === "seconds" ? "s" : "m"}
              </span>
            </label>
          )}
        </For>
      </div>
      <button
        type="button"
        class="set-done"
        aria-pressed={props.set.completed}
        aria-label={props.set.completed ? `Uncheck ${label().toLowerCase()}` : `Complete ${label().toLowerCase()}`}
        disabled={pending()}
        onClick={toggleDone}
      >
        <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="m5 12.5 4.5 4.5L19 7.5" />
        </svg>
      </button>
      <button
        type="button"
        class="icon-btn set-remove"
        aria-label={`Delete ${label().toLowerCase()}`}
        disabled={props.set.completed || pending()}
        onClick={() => void run(() => runGym({ type: "remove_set", set_id: props.set.id }))}
      >
        ×
      </button>
      <Show when={error()}>
        {(msg) => (
          <p class="field-error set-error" role="alert">
            {msg()}
          </p>
        )}
      </Show>
    </li>
  );
}

function ExerciseCard(props: { view: GymView; item: WorkoutExercise }) {
  const [adding, setAdding] = createSignal(false);
  const [notes, setNotes] = createSignal("");
  const [notesFocused, setNotesFocused] = createSignal(false);
  const exercise = createMemo(() => exerciseById(props.view, props.item.exercise_id));
  const kind = (): ExerciseKind => exercise()?.kind ?? "weight_reps";
  const previous = () => props.view.previous_labels[props.item.id];

  createEffect(() => {
    const serverNotes = props.item.notes;
    if (!notesFocused()) setNotes(serverNotes);
  });

  async function addSet() {
    if (adding()) return;
    setAdding(true);
    try {
      await runGym({ type: "add_set", exercise_id: props.item.id });
    } finally {
      setAdding(false);
    }
  }

  function saveNotes() {
    setNotesFocused(false);
    const text = notes();
    if (text !== props.item.notes) void runGym({ type: "set_notes", notes: text, exercise_id: props.item.id });
  }

  return (
    <section class="card exercise-card" aria-label={exercise()?.name ?? "Exercise"}>
      <header class="exercise-head">
        <div>
          <h3 class="exercise-name">{exercise()?.name ?? "Exercise"}</h3>
          <Show when={previous()}>{(p) => <p class="muted small">Previous · {p()}</p>}</Show>
        </div>
        <ConfirmButton
          label="Remove"
          confirmLabel="Remove exercise"
          onConfirm={() => runGym({ type: "remove_exercise", exercise_id: props.item.id })}
        />
      </header>
      <ol class="set-list">
        <For each={props.item.sets}>
          {(set, i) => <SetRow view={props.view} set={set} index={i()} kind={kind()} />}
        </For>
      </ol>
      <button type="button" class="btn btn--small btn--block add-set" disabled={adding()} onClick={() => void addSet()}>
        <PlusIcon /> Add set
      </button>
      <details class="exercise-notes" open={props.item.notes.length > 0}>
        <summary>Notes</summary>
        <label class="sr-only" for={`notes-${props.item.id}`}>
          Notes for {exercise()?.name ?? "exercise"}
        </label>
        <textarea
          id={`notes-${props.item.id}`}
          class="text-input notes-input"
          rows="2"
          maxlength="8000"
          value={notes()}
          onFocus={() => setNotesFocused(true)}
          onInput={(e) => setNotes(e.currentTarget.value)}
          onBlur={saveNotes}
        />
      </details>
    </section>
  );
}

/** Rest countdown and workout clock. Local ticking only between server updates. */
function useWorkoutClock() {
  const [now, setNow] = createSignal(Date.now());

  // A fresh clock from the server resets the local tick base.
  createEffect(on(() => gym.clock?.receivedAt, () => setNow(Date.now())));

  const restRemaining = createMemo(() => {
    const c = gym.clock;
    if (!c) return 0;
    return restRemainingNow(c, Math.max(now(), c.receivedAt));
  });

  const elapsed = createMemo(() => {
    const c = gym.clock;
    if (!c || c.active_elapsed_seconds === undefined) return null;
    return c.active_elapsed_seconds + Math.max(0, Math.floor((now() - c.receivedAt) / 1000));
  });

  const ticking = createMemo(() => restRemaining() > 0 || elapsed() !== null);

  createEffect(() => {
    if (!ticking()) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    onCleanup(() => clearInterval(timer));
  });

  // When the local countdown runs out, ask the server once for the real clock.
  createEffect(
    on(restRemaining, (left, prev) => {
      if (left === 0 && prev !== undefined && prev > 0) void tickClock();
    }),
  );

  const duration = () => {
    const e = elapsed();
    if (e !== null) return formatDuration(e);
    return gym.clock?.active_duration ?? gym.view?.summary.active_duration ?? "";
  };

  return { restRemaining, duration };
}

function WorkoutBody(props: { view: GymView }) {
  const navigate = useNavigate();
  const clock = useWorkoutClock();
  const [pickerOpen, setPickerOpen] = createSignal(false);
  const [renaming, setRenaming] = createSignal(false);
  const [title, setTitle] = createSignal("");
  const [notes, setNotes] = createSignal("");
  const [notesFocused, setNotesFocused] = createSignal(false);
  const [routineName, setRoutineName] = createSignal<string | null>(null);
  const [routineSaved, setRoutineSaved] = createSignal(false);
  const [finishing, setFinishing] = createSignal(false);
  const [resting, setResting] = createSignal(false);
  let addButton: HTMLButtonElement | undefined;

  const active = () => props.view.state.active;
  const summary = () => props.view.summary;
  const activeExerciseIds = createMemo(() => (active()?.exercises ?? []).map((e) => e.exercise_id));

  onMount(() => {
    const timer = setInterval(() => {
      if (document.visibilityState === "visible" && active()) void tickClock();
    }, CLOCK_POLL_MS);
    onCleanup(() => clearInterval(timer));
  });

  createEffect(() => {
    const serverNotes = active()?.notes ?? "";
    if (!notesFocused()) setNotes(serverNotes);
  });

  function saveNotes() {
    setNotesFocused(false);
    const text = notes();
    if (active() && text !== active()?.notes) void runGym({ type: "set_notes", notes: text });
  }

  async function rename(e: SubmitEvent) {
    e.preventDefault();
    const res = await runGym({ type: "rename_workout", title: title() });
    if (res?.ok) setRenaming(false);
  }

  async function startRest() {
    if (resting()) return;
    setResting(true);
    try {
      await runGym({ type: "start_rest", seconds: props.view.state.settings.rest_seconds });
    } finally {
      setResting(false);
    }
  }

  async function finish() {
    if (finishing()) return;
    setFinishing(true);
    try {
      const res = await runGym({ type: "finish_workout" });
      const newest = res?.ok ? res.view?.state.history[0] : undefined;
      if (newest) navigate(`/gym/history/${newest.id}`);
      else if (res?.ok) navigate("/gym/history");
    } finally {
      setFinishing(false);
    }
  }

  async function saveRoutine(e: SubmitEvent) {
    e.preventDefault();
    const name = routineName();
    if (name === null) return;
    const res = await runGym({ type: "save_routine", name, from_active: true });
    if (res?.ok) {
      setRoutineName(null);
      setRoutineSaved(true);
    }
  }

  return (
    <Show
      when={active()}
      fallback={
        <div class="empty">
          <p class="empty-title">No workout in progress.</p>
          <A href="/gym" class="btn btn-link">
            Back to Gym
          </A>
        </div>
      }
    >
      {(workout) => (
        <>
          <header class="workout-head">
            <A href="/gym" class="back-link">
              ‹ Gym
            </A>
            <Show
              when={renaming()}
              fallback={
                <div class="workout-title-row">
                  <h1 class="view-title workout-title">{workout().title}</h1>
                  <button
                    type="button"
                    class="btn btn--small"
                    onClick={() => {
                      setTitle(workout().title);
                      setRenaming(true);
                    }}
                  >
                    Rename
                  </button>
                </div>
              }
            >
              <form class="inline-form" onSubmit={rename}>
                <label class="sr-only" for="workout-title">
                  Workout name
                </label>
                <input
                  id="workout-title"
                  class="text-input"
                  maxlength="120"
                  value={title()}
                  onInput={(e) => setTitle(e.currentTarget.value)}
                />
                <button type="submit" class="btn btn--primary btn--small">
                  Save
                </button>
                <button type="button" class="btn btn--small" onClick={() => setRenaming(false)}>
                  Cancel
                </button>
              </form>
            </Show>
          </header>

          <section class="metric-row" aria-label="Workout stats">
            <div class="metric-tile">
              <span class="metric-value">{clock.duration()}</span>
              <span class="metric-label">Duration</span>
            </div>
            <div class="metric-tile">
              <span class="metric-value">{summary().active_volume}</span>
              <span class="metric-label">Volume</span>
            </div>
            <div class="metric-tile">
              <span class="metric-value">{summary().active_completed_sets}</span>
              <span class="metric-label">Sets</span>
            </div>
          </section>

          <Show
            when={clock.restRemaining() > 0}
            fallback={
              <button type="button" class="btn btn--small rest-start" disabled={resting()} onClick={() => void startRest()}>
                Start rest timer ({formatCountdown(props.view.state.settings.rest_seconds)})
              </button>
            }
          >
            <div class="rest-banner" role="timer" aria-label="Rest timer">
              <span class="rest-text">Rest · {formatCountdown(clock.restRemaining())}</span>
              <button type="button" class="btn btn--small" onClick={() => void runGym({ type: "stop_rest" })}>
                Skip
              </button>
            </div>
          </Show>

          <label class="sr-only" for="workout-notes">
            Workout notes
          </label>
          <textarea
            id="workout-notes"
            class="text-input notes-input"
            rows="2"
            maxlength="8000"
            placeholder="Workout notes"
            value={notes()}
            onFocus={() => setNotesFocused(true)}
            onInput={(e) => setNotes(e.currentTarget.value)}
            onBlur={saveNotes}
          />

          <For each={workout().exercises}>{(item) => <ExerciseCard view={props.view} item={item} />}</For>

          <button
            type="button"
            class="btn btn--primary btn--block add-exercise"
            ref={addButton}
            onClick={() => setPickerOpen(true)}
          >
            <PlusIcon /> Add exercise
          </button>

          <section class="workout-actions" aria-label="Finish">
            <button type="button" class="btn btn--primary" disabled={finishing()} onClick={() => void finish()}>
              Finish workout
            </button>
            <ConfirmButton
              label="Discard workout"
              confirmLabel="Discard for good"
              onConfirm={async () => {
                const res = await runGym({ type: "cancel_workout" });
                if (res?.ok) navigate("/gym");
              }}
            />
            <Show
              when={routineName() !== null}
              fallback={
                <button
                  type="button"
                  class="btn btn--small"
                  onClick={() => {
                    setRoutineSaved(false);
                    setRoutineName(workout().title);
                  }}
                >
                  Save as routine
                </button>
              }
            >
              <form class="inline-form" onSubmit={saveRoutine}>
                <label class="sr-only" for="routine-name">
                  Routine name
                </label>
                <input
                  id="routine-name"
                  class="text-input"
                  maxlength="120"
                  value={routineName() ?? ""}
                  onInput={(e) => setRoutineName(e.currentTarget.value)}
                />
                <button type="submit" class="btn btn--primary btn--small">
                  Save
                </button>
                <button type="button" class="btn btn--small" onClick={() => setRoutineName(null)}>
                  Cancel
                </button>
              </form>
            </Show>
            <Show when={routineSaved()}>
              <p class="muted small" role="status">
                Saved to your routines.
              </p>
            </Show>
          </section>

          <ExercisePicker
            open={pickerOpen()}
            activeExerciseIds={activeExerciseIds()}
            onClose={() => {
              setPickerOpen(false);
              addButton?.focus();
            }}
          />
        </>
      )}
    </Show>
  );
}

export default function WorkoutView() {
  return <GymPage subnav={false}>{(view) => <WorkoutBody view={view()} />}</GymPage>;
}
