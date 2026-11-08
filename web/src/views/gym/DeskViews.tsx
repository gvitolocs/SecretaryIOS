import { useNavigate } from "@solidjs/router";
import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { MEASUREMENT_KINDS, type ExerciseKind, type GymMeasurementKind, type GymRecord, type GymView, type Measurement, type Workout, type WorkoutExercise } from "../../api/gym";
import { Button, Card, EmptyState, ListRow, PageGrid, RowList, Sheet } from "../../components/page";
import { formatDate, formatDateTime, formatDistance, formatDuration } from "../../lib/units";
import { runGym } from "../../state/gym";
import { BarChart } from "./BarChart";
import { measurementKindLabel } from "./Progress";
import { exerciseById, JOURNAL_HREF, setBadge, setKindLabel, setsText, setValueText, useStartWorkout } from "./shared";
import { gt, gymLocale } from "./strings";
import "../../styles/gym-desk.css";

// The desktop Journal, Progress and their detail pages, on the page system
// (components/page). GymPage puts them under the Gym header row, so switching
// sections keeps the header and the nav in place and only this content changes.
// The phone layouts stay in History.tsx, Progress.tsx and their details.

function monthLabel(ms: number): string {
  return new Intl.DateTimeFormat(gymLocale(), { month: "long", year: "numeric" }).format(new Date(ms));
}

function shortDate(ms: number): string {
  return new Intl.DateTimeFormat(gymLocale(), { month: "short", day: "numeric" }).format(new Date(ms));
}

const finishedAt = (w: Workout) => w.finished_ms ?? w.started_ms;

/** Two-step delete for a row: the first click arms it, the second deletes. */
function DeleteButton(props: { label: string; confirmLabel: string; ariaLabel: string; onConfirm: () => Promise<unknown> | void }) {
  const [armed, setArmed] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  async function confirm() {
    if (pending()) return;
    setPending(true);
    try {
      await props.onConfirm();
    } finally {
      setPending(false);
      setArmed(false);
    }
  }
  return (
    <Show
      when={armed()}
      fallback={
        <Button variant="ghost" aria-label={props.ariaLabel} onClick={() => setArmed(true)}>
          {props.label}
        </Button>
      }
    >
      <span class="trd-confirm" role="group" aria-label={props.ariaLabel}>
        <Button variant="danger" disabled={pending()} onClick={() => void confirm()}>
          {props.confirmLabel}
        </Button>
        <Button variant="ghost" disabled={pending()} onClick={() => setArmed(false)}>
          {gt("cancel")}
        </Button>
      </span>
    </Show>
  );
}

/** The Gym's all-time totals, as a rail card. */
function TotalsCard(props: { view: GymView }) {
  return (
    <Card title={gt("totals")}>
      <dl class="pg-stats">
        <div class="pg-stat">
          <dt>{gt("sessions")}</dt>
          <dd>{props.view.summary.workouts}</dd>
        </div>
        <div class="pg-stat">
          <dt>{gt("setsLogged")}</dt>
          <dd>{props.view.summary.completed_sets}</dd>
        </div>
        <div class="pg-stat">
          <dt>{gt("totalVolume")}</dt>
          <dd>{props.view.summary.volume}</dd>
        </div>
      </dl>
    </Card>
  );
}

// ---------- Journal ----------

function JournalRow(props: { view: GymView; workout: Workout; disabled: boolean; onRepeat: (w: Workout) => void }) {
  const label = () => props.view.workout_labels[props.workout.id];
  const names = createMemo(() =>
    props.workout.exercises
      .map((e) => exerciseById(props.view, e.exercise_id)?.name)
      .filter((n): n is string => Boolean(n))
      .join(" · "),
  );
  return (
    <ListRow
      title={props.workout.title}
      href={`${JOURNAL_HREF}/${props.workout.id}`}
      sub={
        <>
          {shortDate(finishedAt(props.workout))}
          <Show when={label()}>{(l) => ` · ${l().duration} · ${setsText(l().sets)} · ${l().volume}`}</Show>
        </>
      }
      meta={<span class="trd-names">{names()}</span>}
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

/** Finished workouts by month, newest first, with the totals in the rail. */
export function JournalDesk(props: { view: GymView }) {
  const { starting, start } = useStartWorkout();
  const months = createMemo(() => {
    const groups: { label: string; workouts: Workout[] }[] = [];
    for (const w of props.view.state.history) {
      const label = monthLabel(finishedAt(w));
      const last = groups[groups.length - 1];
      if (last?.label === label) last.workouts.push(w);
      else groups.push({ label, workouts: [w] });
    }
    return groups;
  });
  const busy = () => starting() || Boolean(props.view.state.active);

  return (
    <PageGrid rail={<TotalsCard view={props.view} />}>
      <Show
        when={months().length > 0}
        fallback={
          <Card label={gt("journal")}>
            <EmptyState
              title={gt("storyTitle")}
              text={gt("storyDetail")}
              action={
                <Show when={!props.view.state.active}>
                  <button type="button" class="pg-link" disabled={starting()} onClick={() => void start({ type: "start_workout", title: gt("defaultWorkoutTitle") })}>
                    {gt("startFirst")}
                  </button>
                </Show>
              }
            />
          </Card>
        }
      >
        <For each={months()}>
          {(month) => (
            <Card list title={month.label}>
              <RowList label={gt("finishedWorkouts")}>
                <For each={month.workouts}>
                  {(workout) => (
                    <JournalRow
                      view={props.view}
                      workout={workout}
                      disabled={busy()}
                      onRepeat={(w) => void start({ type: "repeat_workout", workout_id: w.id })}
                    />
                  )}
                </For>
              </RowList>
            </Card>
          )}
        </For>
      </Show>
    </PageGrid>
  );
}

function BackLink(props: { href: string; children: JSX.Element }) {
  return (
    <a href={props.href} class="pg-link pg-back">
      {props.children}
    </a>
  );
}

function ExerciseCard(props: { view: GymView; item: WorkoutExercise }) {
  const exercise = createMemo(() => exerciseById(props.view, props.item.exercise_id));
  return (
    <Card list title={exercise()?.name ?? gt("exercise")}>
      <RowList>
        <For each={props.item.sets}>
          {(set, i) => (
            <li class="pg-row trd-set" classList={{ "trd-set--skipped": !set.completed }}>
              <span class="trd-set-badge" classList={{ "trd-set-badge--flag": set.kind !== "normal" }} title={setKindLabel(set.kind)}>
                {setBadge(set.kind, i())}
              </span>
              <span class="sr-only">{gt("setKindSr", { kind: setKindLabel(set.kind) })}</span>
              <span class="pg-row-main pg-num">{setValueText(props.view, set, exercise()?.kind ?? "weight_reps")}</span>
              <span class="pg-row-meta">{gt(set.completed ? "setDone" : "setSkipped")}</span>
            </li>
          )}
        </For>
      </RowList>
      <Show when={props.item.notes}>
        <p class="pg-muted preserve trd-notes">{props.item.notes}</p>
      </Show>
    </Card>
  );
}

/** One journal entry: its exercises, with the summary and its actions in the rail. */
export function WorkoutDetailDesk(props: { view: GymView; workout: Workout }) {
  const navigate = useNavigate();
  const { starting, start } = useStartWorkout();
  const label = () => props.view.workout_labels[props.workout.id];
  return (
    <>
      <BackLink href={JOURNAL_HREF}>{gt("backJournal")}</BackLink>
      <h2 class="trd-detail-title">{props.workout.title}</h2>
      <PageGrid
        rail={
          <Card title={gt("summary")}>
            <dl class="pg-stats">
              <div class="pg-stat">
                <dt>{gt("date")}</dt>
                <dd>{formatDateTime(finishedAt(props.workout))}</dd>
              </div>
              <Show when={label()}>
                {(l) => (
                  <>
                    <div class="pg-stat">
                      <dt>{gt("duration")}</dt>
                      <dd>{l().duration}</dd>
                    </div>
                    <div class="pg-stat">
                      <dt>{gt("setsLabel")}</dt>
                      <dd>{l().sets}</dd>
                    </div>
                    <div class="pg-stat">
                      <dt>{gt("volume")}</dt>
                      <dd>{l().volume}</dd>
                    </div>
                  </>
                )}
              </Show>
            </dl>
            <Show when={props.workout.notes}>
              <p class="preserve trd-notes">{props.workout.notes}</p>
            </Show>
            <div class="trd-detail-actions">
              <Button
                disabled={starting() || Boolean(props.view.state.active)}
                onClick={() => void start({ type: "repeat_workout", workout_id: props.workout.id })}
              >
                {gt("repeatWorkout")}
              </Button>
              <DeleteButton
                label={gt("delete")}
                confirmLabel={gt("deleteWorkout")}
                ariaLabel={gt("deleteWorkoutPrompt")}
                onConfirm={async () => {
                  const res = await runGym({ type: "delete_workout", workout_id: props.workout.id });
                  if (res?.ok) navigate(JOURNAL_HREF);
                }}
              />
            </div>
            <Show when={props.view.state.active}>
              <p class="pg-muted trd-notes">{gt("repeatBlocked")}</p>
            </Show>
          </Card>
        }
      >
        <For each={props.workout.exercises}>{(item) => <ExerciseCard view={props.view} item={item} />}</For>
      </PageGrid>
    </>
  );
}

/** A journal link that matches no workout. */
export function WorkoutMissingDesk() {
  return (
    <PageGrid>
      <Card>
        <EmptyState
          title={gt("workoutNotFound")}
          text={gt("storyDetail")}
          action={
            <a href={JOURNAL_HREF} class="pg-link">
              {gt("backToJournal")}
            </a>
          }
        />
      </Card>
    </PageGrid>
  );
}

// ---------- Progress ----------

/** "Heaviest weight 100 kg · Most reps 12", by the exercise's kind. */
export function recordText(record: GymRecord, kind: ExerciseKind): string {
  switch (kind) {
    case "reps_only":
      return `${gt("mostReps")} ${record.reps}`;
    case "timed":
      return `${gt("longestSet")} ${formatDuration(record.seconds)}`;
    case "distance_time":
      return `${gt("longestDistance")} ${formatDistance(record.distance_meters)} · ${gt("longestDuration")} ${formatDuration(record.seconds)}`;
    default:
      return `${gt("heaviestWeight")} ${record.weight} · ${gt("mostReps")} ${record.reps}`;
  }
}

function MeasurementSheet(props: { open: boolean; onClose: () => void }) {
  const [kind, setKind] = createSignal<GymMeasurementKind>("waist");
  const [value, setValue] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);
  const unit = () => MEASUREMENT_KINDS.find((k) => k.kind === kind())?.unit ?? "";

  async function save(e?: Event) {
    e?.preventDefault();
    if (saving()) return;
    const text = value().trim();
    // The engine parses the decimal; this only catches an empty field.
    if (!text) {
      setError(gt("enterValue"));
      return;
    }
    setError(null);
    setSaving(true);
    try {
      const res = await runGym({ type: "record_measurement", kind: kind(), value: text });
      if (res?.ok) {
        setValue("");
        props.onClose();
      }
    } finally {
      setSaving(false);
    }
  }

  return (
    <Sheet
      open={props.open}
      onClose={props.onClose}
      title={gt("newMeasurement")}
      footer={
        <>
          <Button variant="ghost" onClick={() => props.onClose()}>
            {gt("cancel")}
          </Button>
          <button type="submit" form="trd-measure-form" class="pg-cta" disabled={saving()}>
            {gt("saveMeasurement")}
          </button>
        </>
      }
    >
      <form id="trd-measure-form" class="trd-form" onSubmit={(e) => void save(e)} aria-label={gt("newMeasurement")}>
        <label class="pg-field">
          <span class="pg-field-label">{gt("measurement")}</span>
          <select class="pg-input" value={kind()} onChange={(e) => setKind(e.currentTarget.value as GymMeasurementKind)}>
            <For each={MEASUREMENT_KINDS}>{(k) => <option value={k.kind}>{measurementKindLabel(k.kind)}</option>}</For>
          </select>
        </label>
        <label class="pg-field">
          <span class="pg-field-label">{gt("valueIn", { unit: unit() })}</span>
          <input
            class="pg-input pg-num"
            inputmode="decimal"
            autocomplete="off"
            placeholder="0"
            value={value()}
            onInput={(e) => setValue(e.currentTarget.value)}
          />
        </label>
        <Show when={error()}>
          {(msg) => (
            <p class="field-error" role="alert">
              {msg()}
            </p>
          )}
        </Show>
      </form>
    </Sheet>
  );
}

function MeasurementsCard(props: { view: GymView }) {
  const [adding, setAdding] = createSignal(false);
  const rows = (m: Measurement) => {
    const label = measurementKindLabel(m.kind);
    return (
      <ListRow
        title={label}
        sub={formatDate(m.timestamp_ms)}
        meta={<span class="trd-measure-value">{props.view.measurement_labels[m.id] ?? ""}</span>}
        trailing={
          <DeleteButton
            label={gt("delete")}
            confirmLabel={gt("deleteMeasurement")}
            ariaLabel={gt("deleteMeasurementNamed", { kind: label })}
            onConfirm={() => runGym({ type: "delete_measurement", measurement_id: m.id })}
          />
        }
      />
    );
  };
  return (
    <Card
      list
      title={gt("bodyMeasurements")}
      actions={
        <Button variant="ghost" onClick={() => setAdding(true)} aria-label={gt("addMeasurement")}>
          {gt("add")}
        </Button>
      }
    >
      <RowList label={gt("measurements")}>
        <ListRow title={gt("bodyWeight")} href="/weight" sub={gt("bodyWeightInWeightTab")} />
        <For each={props.view.state.measurements}>{(m) => rows(m)}</For>
      </RowList>
      <Show when={props.view.state.measurements.length === 0}>
        <EmptyState text={gt("measurementsEmpty")} />
      </Show>
      <MeasurementSheet open={adding()} onClose={() => setAdding(false)} />
    </Card>
  );
}

/** Training volume and personal records, with body measurements in the rail. */
export function ProgressDesk(props: { view: GymView }) {
  const kindOf = (id: string): ExerciseKind => exerciseById(props.view, id)?.kind ?? "weight_reps";
  const count = createMemo(() => props.view.chart.length);
  return (
    <PageGrid rail={<MeasurementsCard view={props.view} />}>
      <Card title={gt("trainingVolume")} actions={<span class="pg-muted trd-caption">{gt("warmupsExcluded")}</span>}>
        <p class="pg-big">{props.view.summary.volume}</p>
        <Show when={count() > 0} fallback={<EmptyState text={gt("sessionsAppear")} />}>
          <div class="trd-chart">
            <BarChart points={props.view.chart} unit={props.view.state.settings.weight_unit} ariaLabel={gt("chartLabel", { count: count() })} />
          </div>
        </Show>
      </Card>
      <Card list title={gt("personalRecords")}>
        <Show
          when={props.view.records.length > 0}
          fallback={<EmptyState title={gt("firstMarkTitle")} text={gt("firstMarkDetail")} />}
        >
          <RowList label={gt("personalRecords")}>
            <For each={props.view.records}>
              {(record) => (
                <ListRow
                  title={record.name}
                  href={`/gym/progress/${record.exercise_id}`}
                  sub={recordText(record, kindOf(record.exercise_id))}
                  meta={record.estimated_1rm ? `${gt("estimated1rm")} ${record.estimated_1rm}` : undefined}
                />
              )}
            </For>
          </RowList>
        </Show>
      </Card>
    </PageGrid>
  );
}

/** One exercise: its volume trend, with the best set in the rail. */
export function ExerciseDetailDesk(props: { view: GymView; exerciseId: string }) {
  const exercise = createMemo(() => exerciseById(props.view, props.exerciseId));
  const record = createMemo(() => props.view.records.find((r) => r.exercise_id === props.exerciseId));
  const name = () => exercise()?.name ?? record()?.name ?? gt("exercise");
  const kind = () => exercise()?.kind ?? "weight_reps";
  // Drawn only when the server sends at least two points (absent on older engines).
  const trend = createMemo(() => {
    const points = props.view.exercise_charts?.[props.exerciseId] ?? [];
    return points.length >= 2 ? points : null;
  });
  return (
    <>
      <BackLink href="/gym/progress">{gt("backProgress")}</BackLink>
      <h2 class="trd-detail-title">{name()}</h2>
      <PageGrid
        rail={
          <Card title={gt("bestSet")}>
            <Show when={record()} fallback={<EmptyState title={gt("noSetsYet")} text={gt("noSetsDetail")} />}>
              {(r) => (
                <dl class="pg-stats">
                  <Show
                    when={kind() === "weight_reps"}
                    fallback={
                      <div class="pg-stat">
                        <dt>{gt("bestSet")}</dt>
                        <dd>{recordText(r(), kind())}</dd>
                      </div>
                    }
                  >
                    <div class="pg-stat">
                      <dt>{gt("heaviestWeight")}</dt>
                      <dd>{r().weight}</dd>
                    </div>
                    <div class="pg-stat">
                      <dt>{gt("mostReps")}</dt>
                      <dd>{r().reps}</dd>
                    </div>
                    <div class="pg-stat">
                      <dt>{gt("volume")}</dt>
                      <dd>{r().volume}</dd>
                    </div>
                  </Show>
                  <Show when={r().estimated_1rm}>
                    {(orm) => (
                      <div class="pg-stat">
                        <dt>{gt("estimated1rm")}</dt>
                        <dd>{orm()}</dd>
                      </div>
                    )}
                  </Show>
                </dl>
              )}
            </Show>
          </Card>
        }
      >
        <Card title={gt("volumeTrend")}>
          <Show when={trend()} fallback={<EmptyState text={gt("noTrend")} />}>
            {(t) => (
              <div class="trd-chart">
                <BarChart points={t()} unit={props.view.state.settings.weight_unit} ariaLabel={gt("trendLabel", { name: name() })} />
              </div>
            )}
          </Show>
        </Card>
      </PageGrid>
    </>
  );
}
