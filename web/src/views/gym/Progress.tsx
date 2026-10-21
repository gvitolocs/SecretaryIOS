import { A } from "@solidjs/router";
import { createMemo, For, Match, Show, Switch } from "solid-js";
import type { ChartPoint, ExerciseKind, GymRecord, GymView } from "../../api/gym";
import { LineChart } from "../../components/LineChart";
import { formatDistance, formatDuration } from "../../lib/units";
import { exerciseById, GymPage } from "./shared";

/** Server chart points to LineChart input. Labels are the server's, first and last only. */
export function volumeChart(points: ChartPoint[]) {
  return {
    series: [{ key: "volume", points: points.map((p, i) => ({ x: i, y: p.volume })), class: "chart-line--primary" }],
    labels: points.length > 1 ? [points[0]?.label ?? "", points[points.length - 1]?.label ?? ""] : [],
  };
}

/** Kind-specific record metrics, mirroring ProgressScreen.swift. */
export function RecordMetrics(props: { record: GymRecord; kind: ExerciseKind }) {
  return (
    <div class="metric-row">
      <Switch
        fallback={
          <>
            <div class="metric-tile">
              <span class="metric-value">{props.record.weight}</span>
              <span class="metric-label">Heaviest weight</span>
            </div>
            <div class="metric-tile">
              <span class="metric-value">{props.record.reps}</span>
              <span class="metric-label">Most reps</span>
            </div>
          </>
        }
      >
        <Match when={props.kind === "reps_only"}>
          <div class="metric-tile">
            <span class="metric-value">{props.record.reps}</span>
            <span class="metric-label">Most reps</span>
          </div>
        </Match>
        <Match when={props.kind === "timed"}>
          <div class="metric-tile">
            <span class="metric-value">{formatDuration(props.record.seconds)}</span>
            <span class="metric-label">Longest set</span>
          </div>
        </Match>
        <Match when={props.kind === "distance_time"}>
          <div class="metric-tile">
            <span class="metric-value">{formatDistance(props.record.distance_meters)}</span>
            <span class="metric-label">Longest distance</span>
          </div>
          <div class="metric-tile">
            <span class="metric-value">{formatDuration(props.record.seconds)}</span>
            <span class="metric-label">Longest duration</span>
          </div>
        </Match>
      </Switch>
    </div>
  );
}

function ProgressBody(props: { view: GymView }) {
  const chart = createMemo(() => volumeChart(props.view.chart));
  const kindOf = (id: string): ExerciseKind => exerciseById(props.view, id)?.kind ?? "weight_reps";

  return (
    <>
      <h1 class="view-title">Progress</h1>
      <section class="card" aria-label="Training volume">
        <p class="eyebrow">Training volume</p>
        <p class="big-number">{props.view.summary.volume}</p>
        <Show
          when={props.view.chart.length > 0}
          fallback={<p class="muted small">Your completed sessions will appear here.</p>}
        >
          <LineChart
            series={chart().series}
            xLabels={chart().labels}
            ariaLabel="Volume over the most recent workouts"
            height={140}
          />
        </Show>
        <p class="muted small">Last 14 workouts · warmups excluded</p>
      </section>

      <section class="gym-section">
        <h2 class="section-title">Personal records</h2>
        <Show
          when={props.view.records.length > 0}
          fallback={
            <div class="empty">
              <p class="empty-title">No records yet.</p>
              <p class="empty-sub">Complete working sets to set your first marks.</p>
            </div>
          }
        >
          <ul class="gym-list" aria-label="Personal records">
            <For each={props.view.records}>
              {(record) => (
                <li>
                  <A href={`/gym/progress/${record.exercise_id}`} class="card record-item">
                    <span class="record-name">{record.name}</span>
                    <RecordMetrics record={record} kind={kindOf(record.exercise_id)} />
                  </A>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </section>
    </>
  );
}

export default function ProgressView() {
  return <GymPage>{(view) => <ProgressBody view={view()} />}</GymPage>;
}
