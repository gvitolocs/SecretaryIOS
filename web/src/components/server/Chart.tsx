import { createMemo, For, Show } from "solid-js";
import { chartPaths, chartScale, hourTicks, seriesStats, type HostHistory } from "../../api/systemOverview";
import { locale, t, type MessageKey } from "../../i18n";
import { num } from "./parts";

// Inline SVG charts for a host's 24 h history: a sparkline on the host card and a larger chart
// with hour ticks in the host sheet. The SVG stretches to its box (non-scaling strokes); the last
// value's dot, the tick labels and the 90 °C label are HTML so they never distort. Nulls are gaps.

export type Metric = "tctl" | "load1" | "mem";

export const METRICS: Record<Metric, { label: MessageKey; short: MessageKey; fmt: (v: number) => string; fixed?: [number, number]; floor?: number }> = {
  tctl: { label: "serverHealth.cpuTemp", short: "serverHealth.cpuTempShort", fmt: (v) => `${num(v)} °C`, fixed: [30, 100] },
  load1: { label: "serverHealth.load1", short: "serverHealth.load1Short", fmt: (v) => num(v, 2), floor: 0 },
  mem: { label: "serverHealth.memory", short: "serverHealth.memoryShort", fmt: (v) => `${num(v)}%`, fixed: [0, 100] },
};

/** "CPU temperature, last 24 hours: now 95 °C, low 61, high 96". */
export function chartLabel(metric: Metric, values: (number | null)[], current?: number | null): string {
  const m = METRICS[metric];
  const s = seriesStats([...values, current ?? null]);
  const what = t(m.label);
  const period = t("serverHealth.last24h");
  const now = current ?? s.now;
  if (now == null || s.min == null) return t("serverHealth.chartNoData", { what, period });
  const bare = (v: number) => (metric === "load1" ? num(v, 2) : num(v));
  return t("serverHealth.chartLabel", { what, period, now: m.fmt(now), min: bare(Math.min(s.min, now)), max: bare(Math.max(s.max!, now)) });
}

const HOT_C = 90;

export function Chart(props: { metric: Metric; values: (number | null)[]; current?: number | null; history?: HostHistory; large?: boolean }) {
  const W = 100;
  const H = () => (props.large ? 100 : 32);
  const m = () => METRICS[props.metric];
  const scale = createMemo(() => chartScale(props.values, { fixed: m().fixed, floor: m().floor }));
  const paths = createMemo(() => chartPaths(props.values, W, H(), scale()));
  const ticks = createMemo(() => (props.large && props.history ? hourTicks({ end: props.history.end, step_s: props.history.step_s, count: props.values.length }) : []));
  const hotY = () => {
    const [lo, hi] = scale();
    return props.metric === "tctl" && props.large && hi > lo ? (1 - (HOT_C - lo) / (hi - lo)) * 100 : null;
  };
  const hour = (ms: number) => new Intl.DateTimeFormat(locale(), { hour: "2-digit", minute: "2-digit" }).format(ms);
  return (
    <div class="sh-chart" classList={{ "sh-chart--large": !!props.large }} role="img" aria-label={chartLabel(props.metric, props.values, props.current)}>
      <Show when={props.values.filter((v) => v != null && Number.isFinite(v)).length >= 2} fallback={<span class="sh-collecting">{t("serverHealth.collecting")}</span>}>
      <div class="sh-chart-plot">
        <svg viewBox={`0 0 ${W} ${H()}`} preserveAspectRatio="none" aria-hidden="true">
          <For each={ticks()}>{(tk) => <line class="sh-chart-grid" x1={tk.x * W} x2={tk.x * W} y1="0" y2={H()} vector-effect="non-scaling-stroke" />}</For>
          <Show when={hotY() != null}>
            <line class="sh-chart-hot" x1="0" x2={W} y1={hotY()!} y2={hotY()!} vector-effect="non-scaling-stroke" />
          </Show>
          <path class="sh-chart-area" d={paths().area} />
          <path class="sh-chart-line" d={paths().line} vector-effect="non-scaling-stroke" />
        </svg>
        <Show when={paths().last}>{(p) => <span class="sh-chart-dot" style={{ left: `${p().x * 100}%`, top: `${p().y * 100}%` }} />}</Show>
        <Show when={hotY() != null}>
          <span class="sh-chart-hot-label" style={{ top: `${hotY()}%` }} aria-hidden="true">
            {t("serverHealth.hot")}
          </span>
        </Show>
      </div>
      <Show when={ticks().length}>
        <div class="sh-chart-ticks" aria-hidden="true">
          <For each={ticks()}>
            {(tk) => (
              <span style={{ left: `${tk.x * 100}%` }}>
                <time dateTime={new Date(tk.at).toISOString()}>{hour(tk.at)}</time>
              </span>
            )}
          </For>
        </div>
      </Show>
      </Show>
    </div>
  );
}

/** One metric on a host card: its name and current value over a sparkline. */
export function Spark(props: { metric: Metric; values: (number | null)[]; current: number | null }) {
  const m = () => METRICS[props.metric];
  const now = () => props.current ?? seriesStats(props.values).now;
  return (
    <div class="sh-spark">
      <div class="sh-spark-head">
        <span class="sh-spark-name">{t(m().short)}</span>
        <span class="sh-spark-value">{now() == null ? "—" : m().fmt(now()!)}</span>
      </div>
      <Chart metric={props.metric} values={props.values} current={props.current} />
    </div>
  );
}

/** The large chart in the host sheet with now / low / high under it. */
export function BigChart(props: { metric: Metric; history: HostHistory; current: number | null }) {
  const m = () => METRICS[props.metric];
  const values = () => props.history[props.metric];
  const s = createMemo(() => seriesStats([...values(), props.current]));
  const fmt = (v: number | null) => (v == null ? "—" : m().fmt(v));
  return (
    <figure class="sh-bigchart">
      <figcaption class="sh-bigchart-title">{t(m().label)}</figcaption>
      <Chart metric={props.metric} values={values()} current={props.current} history={props.history} large />
      <p class="sh-bigchart-stats" aria-hidden="true">
        <span>{t("serverHealth.chartNow", { value: fmt(props.current ?? s().now) })}</span>
        <span>{t("serverHealth.chartMin", { value: fmt(s().min) })}</span>
        <span>{t("serverHealth.chartMax", { value: fmt(s().max) })}</span>
      </p>
    </figure>
  );
}
