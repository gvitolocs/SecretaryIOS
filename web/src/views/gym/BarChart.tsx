import { createMemo, For } from "solid-js";
import type { ChartPoint } from "../../api/gym";
import { gt, gymLocale } from "./strings";

// One bar per workout, keyed by workout id (ProgressScreen.swift BarMark with
// x = point.id): two sessions on the same day stay two bars. The x axis is
// hidden as in the original; each bar carries its date and volume as a title.

const W = 600;
const H = 240;
const TOP = 8;

export interface Bar {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
  title: string;
}

function axisNumber(value: number): string {
  return new Intl.NumberFormat(gymLocale(), { maximumFractionDigits: value < 10 ? 1 : 0 }).format(value);
}

/** Bar geometry in a W×H box, zero-based so heights compare honestly. */
export function barLayout(points: ChartPoint[], unit: string): { bars: Bar[]; max: number } {
  const max = Math.max(0, ...points.map((p) => (Number.isFinite(p.volume) ? p.volume : 0)));
  const slot = points.length > 0 ? W / points.length : W;
  const width = Math.min(slot * 0.68, 56);
  const plot = H - TOP;
  const bars = points.map((p, i) => {
    const value = Number.isFinite(p.volume) ? Math.max(0, p.volume) : 0;
    const height = max > 0 ? Math.max(3, (value / max) * plot) : 3;
    return {
      id: p.id,
      x: i * slot + (slot - width) / 2,
      y: H - height,
      width,
      height,
      title: gt("barTitle", { label: p.label, value: axisNumber(value), unit }),
    };
  });
  return { bars, max };
}

export function BarChart(props: { points: ChartPoint[]; unit: string; ariaLabel: string }) {
  const layout = createMemo(() => barLayout(props.points, props.unit));
  return (
    <div class="chart bar-chart">
      <div class="chart-plot">
        <span class="chart-y chart-y--max" aria-hidden="true">
          {axisNumber(layout().max)}
        </span>
        <span class="chart-y chart-y--min" aria-hidden="true">
          0
        </span>
        <svg class="chart-svg bar-chart-svg" viewBox={`0 0 ${W} ${H}`} role="img" aria-label={props.ariaLabel}>
          <line class="bar-chart-grid" x1="0" x2={W} y1={TOP} y2={TOP} />
          <line class="bar-chart-grid" x1="0" x2={W} y1={TOP + (H - TOP) / 2} y2={TOP + (H - TOP) / 2} />
          <line class="bar-chart-base" x1="0" x2={W} y1={H} y2={H} />
          <For each={layout().bars}>
            {(bar) => (
              <rect
                class="bar-chart-bar"
                x={bar.x}
                y={bar.y}
                width={bar.width}
                height={bar.height}
                rx={Math.min(8, bar.width / 4)}
              >
                <title>{bar.title}</title>
              </rect>
            )}
          </For>
        </svg>
      </div>
    </div>
  );
}
