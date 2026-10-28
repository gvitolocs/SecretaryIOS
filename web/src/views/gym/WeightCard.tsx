import { A } from "@solidjs/router";
import { createMemo, createSignal, For, onMount, Show } from "solid-js";
import { parseWeightOnServer, type BodySeriesPoint, type BodyView } from "../../api/body";
import { toApiError } from "../../api/client";
import { locale, t } from "../../i18n";
import { readString, writeString } from "../../lib/storage";
import { formatWeight, weightValue, type WeightUnit } from "../../lib/units";
import { body, refreshBody, runBody } from "../../state/body";
import { currentTimeZone } from "../../state/engine";
import { daysBetween } from "./desk";

// The weight card of the desktop Training page: the current weight, its weekly
// change against the goal, a sparkline of the 7-day average, and a one-line
// weigh-in. The full log, editing and settings stay on /weight (views/Weight.tsx).

const WEIGHT_UNIT_KEY = "secretary.weightUnit";
const DAY_MS = 86_400_000;
/** Changes under this many grams read as "steady". */
const STEADY_GRAMS = 50;

type Range = "30" | "90" | "all";
const RANGES: { range: Range; label: "weight.range30" | "weight.range90" | "weight.rangeAll" }[] = [
  { range: "30", label: "weight.range30" },
  { range: "90", label: "weight.range90" },
  { range: "all", label: "weight.rangeAll" },
];

/** "−1.2 kg" with a real minus sign. */
function signed(label: string): string {
  return label.replace(/^-/, "−");
}

/** "80.0 kg" → ["80.0", "kg"]. */
function splitUnit(label: string): [string, string] {
  const i = label.lastIndexOf(" ");
  return i > 0 ? [label.slice(0, i), label.slice(i + 1)] : [label, ""];
}

export type ChangeTone = "good" | "bad" | "steady";

/** Whether the weekly change moves toward the goal (neutral with no goal). */
export function changeTone(view: BodyView): ChangeTone {
  const change = view.stats.weekly_change_grams ?? 0;
  const goal = view.stats.goal_grams;
  const current = view.stats.current_grams;
  if (Math.abs(change) < STEADY_GRAMS || goal === null || current === null) return "steady";
  return Math.sign(goal - current) === Math.sign(change) ? "good" : "bad";
}

export interface Spark {
  avg: string;
  area: string;
  dots: string;
  goalY: number | null;
  minY: number;
  maxY: number;
  min: number;
  max: number;
  from: number;
  to: number;
}

/**
 * The sparkline in a 0–100 box: the 7-day average as a line and area, raw
 * weigh-ins as dots, the goal as a y position. The y range covers the data
 * (and the goal when it is close) with a margin, so nothing touches an edge.
 */
export function sparkline(points: readonly BodySeriesPoint[], unit: WeightUnit, goalGrams: number | null): Spark | null {
  if (points.length === 0) return null;
  const raw = points.map((p) => weightValue(p.weight_grams, unit));
  const avg = points.map((p) => weightValue(p.avg7_grams, unit));
  const min = Math.min(...raw, ...avg);
  const max = Math.max(...raw, ...avg);
  const span = Math.max(max - min, unit === "lb" ? 1 : 0.5);
  const goal = goalGrams === null ? null : weightValue(goalGrams, unit);
  const goalNear = goal !== null && goal >= min - span && goal <= max + span;
  const lo = Math.min(min, goalNear ? goal : min) - span * 0.15;
  const hi = Math.max(max, goalNear ? goal : max) + span * 0.15;
  const from = points[0]!.measured_ms;
  const to = points[points.length - 1]!.measured_ms;
  const x = (ms: number) => (to === from ? 50 : ((ms - from) / (to - from)) * 100);
  const y = (v: number) => (1 - (v - lo) / (hi - lo)) * 100;
  const f = (n: number) => n.toFixed(2);
  const line = points.map((p, i) => `${i === 0 ? "M" : "L"}${f(x(p.measured_ms))} ${f(y(avg[i]!))}`).join(" ");
  const first = f(x(from));
  const last = f(x(to));
  return {
    avg: points.length === 1 ? `M${f(x(from) - 2)} ${f(y(avg[0]!))} L${f(x(from) + 2)} ${f(y(avg[0]!))}` : line,
    area: points.length === 1 ? "" : `${line} L${last} 100 L${first} 100 Z`,
    dots: points.map((p, i) => `M${f(x(p.measured_ms))} ${f(y(raw[i]!))}h0`).join(" "),
    goalY: goalNear && goal !== null ? y(goal) : null,
    minY: y(Math.min(...raw)),
    maxY: y(Math.max(...raw)),
    min: Math.min(...points.map((p) => p.weight_grams)),
    max: Math.max(...points.map((p) => p.weight_grams)),
    from,
    to,
  };
}

function shortDate(ms: number, now: number): string {
  if (daysBetween(ms, now) === 0) return t("weight.today");
  return new Intl.DateTimeFormat(locale(), { month: "short", day: "numeric" }).format(new Date(ms));
}

export default function WeightCard() {
  const stored = readString("local", WEIGHT_UNIT_KEY);
  const [unitOverride, setUnitOverride] = createSignal<WeightUnit | null>(stored === "kg" || stored === "lb" ? stored : null);
  const [range, setRange] = createSignal<Range>("90");
  const [text, setText] = createSignal("");
  const [note, setNote] = createSignal("");
  const [noteOpen, setNoteOpen] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [pending, setPending] = createSignal(false);

  onMount(() => void refreshBody());

  const viewUnit = (): WeightUnit => body.view?.settings.unit ?? "kg";
  const unit = (): WeightUnit => unitOverride() ?? viewUnit();
  const otherUnit = (): WeightUnit => (unit() === "kg" ? "lb" : "kg");

  const points = createMemo(() => {
    const series = body.view?.series ?? [];
    const last = series[series.length - 1];
    if (!last || range() === "all") return series;
    const since = last.measured_ms - (Number(range()) - 1) * DAY_MS;
    return series.filter((p) => p.measured_ms >= since);
  });

  const spark = createMemo(() => sparkline(points(), viewUnit(), body.view?.stats.goal_grams ?? null));

  function chooseUnit(next: WeightUnit) {
    setUnitOverride(next);
    writeString("local", WEIGHT_UNIT_KEY, next);
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (pending()) return;
    const value = text().trim();
    if (!value) {
      setError(t("weight.enterWeight"));
      return;
    }
    const chosen = unit();
    const noteText = note().trim();
    setError(null);
    setPending(true);
    try {
      // The engine parses `weight` in the display unit; for the other unit ask it to parse first.
      let weight: { weight: string } | { weight_grams: number };
      if (chosen === viewUnit()) {
        weight = { weight: value };
      } else {
        try {
          weight = { weight_grams: await parseWeightOnServer(value, chosen, currentTimeZone()) };
        } catch (err) {
          setError(toApiError(err).message);
          return;
        }
      }
      const result = await runBody({ type: "log_entry", ...weight, ...(noteText ? { note: noteText } : {}) });
      if (result?.ok) {
        setText("");
        setNote("");
        setNoteOpen(false);
      } else if (result?.error) {
        setError(result.error);
      }
    } finally {
      setPending(false);
    }
  }

  const quickLog = () => (
    <form class="trd-log" onSubmit={(e) => void submit(e)} aria-label={t("weight.log")}>
      <div class="trd-log-row">
        <label class="sr-only" for="trd-weight">
          {t("weight.weightIn", { unit: unit() })}
        </label>
        <input
          id="trd-weight"
          class="pg-input"
          type="text"
          inputmode="decimal"
          placeholder={t("weight.weightIn", { unit: unit() })}
          value={text()}
          onInput={(e) => setText(e.currentTarget.value)}
          autocomplete="off"
          disabled={pending()}
        />
        <button class="pg-btn" type="submit" disabled={pending()}>
          {pending() ? t("weight.logging") : t("weight.quickLog")}
        </button>
      </div>
      <Show when={noteOpen()}>
        <label class="sr-only" for="trd-weight-note">
          {t("weight.noteOptional")}
        </label>
        <input
          id="trd-weight-note"
          class="pg-input"
          type="text"
          placeholder={t("weight.noteOptional")}
          value={note()}
          onInput={(e) => setNote(e.currentTarget.value)}
          maxlength="1000"
          autocomplete="off"
          disabled={pending()}
        />
      </Show>
      <Show when={error()}>{(message) => <p class="form-error trd-error">{message()}</p>}</Show>
      <p class="trd-log-links">
        <button type="button" class="pg-link" onClick={() => chooseUnit(otherUnit())}>
          {t("weight.useUnit", { unit: otherUnit() })}
        </button>
        <Show when={!noteOpen()}>
          <span aria-hidden="true">·</span>
          <button type="button" class="pg-link" onClick={() => setNoteOpen(true)}>
            {t("weight.addNote")}
          </button>
        </Show>
        <span aria-hidden="true">·</span>
        <A href="/weight" class="pg-link">
          {t("weight.allEntries")}
        </A>
      </p>
    </form>
  );

  return (
    <section class="pg-card trd-weight" aria-labelledby="trd-weight-title">
      <div class="pg-card-head">
        <h2 class="pg-card-title" id="trd-weight-title">
          {t("weight.title")}
        </h2>
        <Show when={(body.view?.series.length ?? 0) > 1}>
          <div class="trd-range" role="group" aria-label={t("weight.rangeGroup")}>
            <For each={RANGES}>
              {(r) => (
                <button
                  type="button"
                  class="trd-range-btn"
                  aria-pressed={range() === r.range ? "true" : "false"}
                  onClick={() => setRange(r.range)}
                >
                  {t(r.label)}
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>

      <Show
        when={body.view}
        fallback={
          <p class="pg-muted" role="status">
            {body.loadError ?? t("weight.loading")}
          </p>
        }
      >
        {(view) => (
          <Show
            when={view().stats.count > 0}
            fallback={<p class="pg-empty-text">{t("weight.emptyDesk")}</p>}
          >
            <div class="trd-weight-now">
              <p class="trd-weight-value">
                <span>{splitUnit(view().stat_labels.current ?? "—")[0]}</span>
                <span class="trd-weight-unit">{splitUnit(view().stat_labels.current ?? "")[1]}</span>
              </p>
              <Show when={view().stat_labels.weekly_change}>
                {(change) => (
                  <span class={`trd-chip trd-chip--${changeTone(view())}`}>
                    {t("weight.thisWeekChange", { change: signed(change()) })}
                  </span>
                )}
              </Show>
            </div>
            <p class="pg-muted trd-weight-line">
              <Show when={view().stat_labels.avg7}>{(avg) => t("weight.avgLine", { avg: avg() })}</Show>
              <span aria-hidden="true"> · </span>
              <Show
                when={view().stat_labels.goal}
                fallback={
                  <A href="/weight" class="pg-link">
                    {t("weight.setGoal")}
                  </A>
                }
              >
                {(goal) => t("weight.goalLine", { goal: goal() })}
              </Show>
            </p>

            <Show when={spark()}>
              {(s) => (
                <figure class="trd-spark" aria-label={t("weight.chartLabel")}>
                  <div class="trd-spark-plot">
                    <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
                      <defs>
                        <linearGradient id="trd-spark-fill" x1="0" y1="0" x2="0" y2="1">
                          <stop offset="0" stop-color="currentColor" stop-opacity="0.22" />
                          <stop offset="1" stop-color="currentColor" stop-opacity="0" />
                        </linearGradient>
                      </defs>
                      <Show when={s().goalY !== null}>
                        <line class="trd-spark-goal" x1="0" x2="100" y1={s().goalY!} y2={s().goalY!} />
                      </Show>
                      <path class="trd-spark-area" d={s().area} fill="url(#trd-spark-fill)" />
                      <path class="trd-spark-dots" d={s().dots} />
                      <path class="trd-spark-line" d={s().avg} />
                    </svg>
                    <span class="trd-spark-label" style={{ top: `${s().maxY}%` }}>
                      {formatWeight(s().max, viewUnit())}
                    </span>
                    <Show when={s().maxY !== s().minY}>
                      <span class="trd-spark-label" style={{ top: `${s().minY}%` }}>
                        {formatWeight(s().min, viewUnit())}
                      </span>
                    </Show>
                    <Show
                      when={
                        s().goalY !== null && Math.abs(s().goalY! - s().minY) > 12 && Math.abs(s().goalY! - s().maxY) > 12
                      }
                    >
                      <span class="trd-spark-label trd-spark-label--goal" style={{ top: `${s().goalY}%` }}>
                        {t("weight.goalShort")}
                      </span>
                    </Show>
                  </div>
                  <figcaption class="trd-spark-x">
                    <span>{shortDate(s().from, Date.now())}</span>
                    <span>{shortDate(s().to, Date.now())}</span>
                  </figcaption>
                </figure>
              )}
            </Show>
          </Show>
        )}
      </Show>

      {quickLog()}
    </section>
  );
}
