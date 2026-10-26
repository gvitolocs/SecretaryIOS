import { A } from "@solidjs/router";
import { createMemo, createSignal, For, Match, Show, Switch } from "solid-js";
import { MEASUREMENT_KINDS, type ExerciseKind, type GymMeasurementKind, type GymRecord, type GymView, type Measurement } from "../../api/gym";
import { formatDate, formatDistance, formatDuration } from "../../lib/units";
import { runGym } from "../../state/gym";
import { BarChart } from "./BarChart";
import { ArrowUpRightIcon, PlusCircleIcon, TrophyIcon } from "./icons";
import { ConfirmButton, EmptyCard, exerciseById, Eyebrow, GymPage, Metric } from "./shared";
import { gt } from "./strings";

// Progress (ProgressScreen.swift): training volume, personal records, and
// body measurements (waist and body fat; body weight lives in the Weight tab).

/** Kind-specific record metrics, mirroring ProgressScreen.swift. */
export function RecordMetrics(props: { record: GymRecord; kind: ExerciseKind }) {
  return (
    <div class="gym-metrics gym-metrics--record">
      <Switch
        fallback={
          <>
            <Metric value={props.record.weight} label={gt("heaviestWeight")} />
            <Metric value={props.record.reps} label={gt("mostReps")} />
          </>
        }
      >
        <Match when={props.kind === "reps_only"}>
          <Metric value={props.record.reps} label={gt("mostReps")} />
        </Match>
        <Match when={props.kind === "timed"}>
          <Metric value={formatDuration(props.record.seconds)} label={gt("longestSet")} />
        </Match>
        <Match when={props.kind === "distance_time"}>
          <Metric value={formatDistance(props.record.distance_meters)} label={gt("longestDistance")} />
          <Metric value={formatDuration(props.record.seconds)} label={gt("longestDuration")} />
        </Match>
      </Switch>
    </div>
  );
}

/** "body_fat" -> "Body fat" (old gym body-weight rows stay readable). */
export function measurementKindLabel(kind: string): string {
  if (kind === "body_fat") return gt("bodyFat");
  if (kind === "waist") return gt("waist");
  if (kind === "body_weight") return gt("bodyWeight");
  const text = kind.replace(/_/g, " ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function MeasurementForm(props: { onDone: () => void }) {
  const [kind, setKind] = createSignal<GymMeasurementKind>("waist");
  const [value, setValue] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);
  const unit = () => MEASUREMENT_KINDS.find((k) => k.kind === kind())?.unit ?? "";

  async function save(e: SubmitEvent) {
    e.preventDefault();
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
      if (res?.ok) props.onDone();
    } finally {
      setSaving(false);
    }
  }

  return (
    <form class="card measurement-form" onSubmit={save} aria-label={gt("newMeasurement")}>
      <p class="gym-eyebrow">{gt("newMeasurement")}</p>
      <label class="field-label" for="measurement-kind">
        {gt("measurement")}
      </label>
      <select
        id="measurement-kind"
        class="text-input"
        value={kind()}
        onChange={(e) => setKind(e.currentTarget.value as GymMeasurementKind)}
      >
        <For each={MEASUREMENT_KINDS}>{(k) => <option value={k.kind}>{measurementKindLabel(k.kind)}</option>}</For>
      </select>
      <label class="field-label" for="measurement-value">
        {gt("valueIn", { unit: unit() })}
      </label>
      <div class="measurement-value">
        <input
          id="measurement-value"
          class="text-input gym-digits"
          inputmode="decimal"
          autocomplete="off"
          placeholder="0"
          value={value()}
          onInput={(e) => setValue(e.currentTarget.value)}
        />
        <span class="muted" aria-hidden="true">
          {unit()}
        </span>
      </div>
      <Show when={error()}>
        {(msg) => (
          <p class="field-error" role="alert">
            {msg()}
          </p>
        )}
      </Show>
      <div class="picker-create-actions">
        <button type="submit" class="btn btn--primary" disabled={saving()}>
          {gt("saveMeasurement")}
        </button>
        <button type="button" class="btn" onClick={() => props.onDone()}>
          {gt("cancel")}
        </button>
      </div>
    </form>
  );
}

function MeasurementRow(props: { view: GymView; measurement: Measurement }) {
  const label = () => measurementKindLabel(props.measurement.kind);
  return (
    <li class="card measurement-row">
      <div class="measurement-main">
        <span class="measurement-kind">{label()}</span>
        <span class="muted small">{formatDate(props.measurement.timestamp_ms)}</span>
      </div>
      <span class="measurement-label">{props.view.measurement_labels[props.measurement.id] ?? ""}</span>
      <ConfirmButton
        label={gt("delete")}
        confirmLabel={gt("deleteMeasurement")}
        ariaLabel={gt("deleteMeasurementNamed", { kind: label() })}
        onConfirm={() => runGym({ type: "delete_measurement", measurement_id: props.measurement.id })}
      />
    </li>
  );
}

function Measurements(props: { view: GymView }) {
  const [adding, setAdding] = createSignal(false);
  let addButton: HTMLButtonElement | undefined;
  return (
    <section class="gym-section" aria-labelledby="measurements-title">
      <div class="gym-section-head">
        <h2 id="measurements-title" class="section-title">
          {gt("bodyMeasurements")}
        </h2>
        <button
          type="button"
          class="icon-btn gym-add"
          ref={addButton}
          aria-label={gt("addMeasurement")}
          aria-expanded={adding()}
          onClick={() => setAdding((a) => !a)}
        >
          <PlusCircleIcon />
        </button>
      </div>
      <Show when={adding()}>
        <MeasurementForm
          onDone={() => {
            setAdding(false);
            addButton?.focus();
          }}
        />
      </Show>
      <A href="/weight" class="card weight-link">
        <span class="weight-link-main">
          <span class="measurement-kind">{gt("bodyWeight")}</span>
          <span class="muted small">{gt("bodyWeightInWeightTab")}</span>
        </span>
        <ArrowUpRightIcon />
      </A>
      <Show
        when={props.view.state.measurements.length > 0}
        fallback={<p class="muted small">{gt("measurementsEmpty")}</p>}
      >
        <ul class="gym-list" aria-label={gt("measurements")}>
          <For each={props.view.state.measurements}>
            {(m) => <MeasurementRow view={props.view} measurement={m} />}
          </For>
        </ul>
      </Show>
    </section>
  );
}

function ProgressBody(props: { view: GymView }) {
  const kindOf = (id: string): ExerciseKind => exerciseById(props.view, id)?.kind ?? "weight_reps";
  const unit = () => props.view.state.settings.weight_unit;
  const count = createMemo(() => props.view.chart.length);

  return (
    <>
      <h1 class="view-title gym-title">{gt("progress")}</h1>
      <section class="card" aria-label={gt("trainingVolume")}>
        <Eyebrow>{gt("trainingVolume")}</Eyebrow>
        <p class="gym-big-number">{props.view.summary.volume}</p>
        <Show
          when={count() > 0}
          fallback={<p class="muted small">{gt("sessionsAppear")}</p>}
        >
          <BarChart
            points={props.view.chart}
            unit={unit()}
            ariaLabel={gt("chartLabel", { count: count() })}
          />
        </Show>
        <Eyebrow>{gt("warmupsExcluded")}</Eyebrow>
      </section>

      <section class="gym-section">
        <h2 class="section-title">{gt("personalRecords")}</h2>
        <Show
          when={props.view.records.length > 0}
          fallback={
            <EmptyCard
              icon={<TrophyIcon />}
              title={gt("firstMarkTitle")}
              detail={gt("firstMarkDetail")}
            />
          }
        >
          <ul class="gym-list" aria-label={gt("personalRecords")}>
            <For each={props.view.records}>
              {(record) => (
                <li>
                  <A href={`/gym/progress/${record.exercise_id}`} class="card record-item">
                    <span class="record-head">
                      <span class="record-trophy">
                        <TrophyIcon />
                      </span>
                      <span class="record-name">{record.name}</span>
                    </span>
                    <RecordMetrics record={record} kind={kindOf(record.exercise_id)} />
                  </A>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </section>

      <Measurements view={props.view} />
    </>
  );
}

export default function ProgressView() {
  return <GymPage>{(view) => <ProgressBody view={view()} />}</GymPage>;
}
