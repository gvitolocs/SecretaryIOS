// One take on Lucina Prompter: which points are waiting, started or done, where the
// speaker is, and what he passed over. Pure functions over an immutable Take, so the
// rules are unit-tested and the view only renders.
//
// - A verdict (from lucina-memo: the local matcher at once, Jev a moment later) with
//   p ≥ DONE_P crosses a point out; p ≥ STARTED_P marks it started.
// - Points of a later section need NEXT_DONE_P: then the take moves on, and the open
//   points he left behind go to the Missed row. Saying a missed point later still ticks it.
// - Done is sticky: repeats change nothing; only "reset section" or a tap undoes it.
// - When every point of the section is done the take moves on by itself (autoAdvance).

import type { ScriptSection } from "./prompterScript";

export type PointState = "waiting" | "started" | "done";
export type CandidateKind = "current" | "missed" | "next";

export const DONE_P = 0.75;
export const STARTED_P = 0.4;
export const NEXT_DONE_P = 0.85;

export interface Verdict {
  id: string;
  p: number;
  source?: string;
}

export interface Candidate {
  id: string;
  text: string;
  kind: CandidateKind;
}

export interface DoneEvent {
  id: string;
  /** Milliseconds since the take started. */
  at: number;
  source: string;
}

export interface Take {
  sections: ScriptSection[];
  current: number;
  state: Readonly<Record<string, PointState>>;
  /** Passed-over points, in the order he left them. */
  missed: readonly string[];
  /** Points in the order he made them. */
  done: readonly DoneEvent[];
  autoAdvance: boolean;
}

export function newTake(sections: ScriptSection[], opts: { autoAdvance?: boolean } = {}): Take {
  const state: Record<string, PointState> = {};
  for (const s of sections) for (const p of s.points) state[p.id] = "waiting";
  const first = Math.max(
    0,
    sections.findIndex((s) => s.points.length > 0),
  );
  return {
    sections,
    current: first,
    state,
    missed: [],
    done: [],
    autoAdvance: opts.autoAdvance ?? true,
  };
}

/** Where a point is: its section, its place, its text. */
export function locate(take: Take, id: string): { section: number; index: number; text: string } | null {
  for (let s = 0; s < take.sections.length; s++) {
    const i = take.sections[s]!.points.findIndex((p) => p.id === id);
    if (i >= 0) return { section: s, index: i, text: take.sections[s]!.points[i]!.text };
  }
  return null;
}

const isOpen = (take: Take, id: string) => take.state[id] !== "done";

/** The point to read next: the first started one of the section, else the first waiting one. */
export function currentPoint(take: Take): string | null {
  const pts = take.sections[take.current]?.points ?? [];
  return pts.find((p) => take.state[p.id] === "started")?.id ?? pts.find((p) => isOpen(take, p.id))?.id ?? null;
}

/** The section after `current` that has points, if any. */
function nextWithPoints(take: Take, from: number): number {
  for (let s = from + 1; s < take.sections.length; s++) if (take.sections[s]!.points.length) return s;
  return -1;
}

/** The points the judge listens for: this section, the Missed row, the next section. */
export function candidates(take: Take): Candidate[] {
  const out: Candidate[] = [];
  const cur = take.sections[take.current];
  for (const p of cur?.points ?? []) if (isOpen(take, p.id)) out.push({ id: p.id, text: p.text, kind: "current" });
  for (const id of take.missed) {
    const at = locate(take, id);
    if (at && at.section !== take.current && isOpen(take, id)) out.push({ id, text: at.text, kind: "missed" });
  }
  const n = nextWithPoints(take, take.current);
  if (n >= 0) for (const p of take.sections[n]!.points) if (isOpen(take, p.id)) out.push({ id: p.id, text: p.text, kind: "next" });
  return out;
}

/** Move to section `to`; going forward sends the open points left behind to Missed. */
export function goTo(take: Take, to: number): Take {
  if (to < 0 || to >= take.sections.length || to === take.current) return take;
  let missed = [...take.missed];
  if (to > take.current) {
    for (let s = take.current; s < to; s++) {
      for (const p of take.sections[s]!.points) {
        if (isOpen(take, p.id) && !missed.includes(p.id)) missed.push(p.id);
      }
    }
  }
  missed = missed.filter((id) => isOpen(take, id));
  return { ...take, current: to, missed };
}

export const nextSection = (take: Take): Take => {
  const n = nextWithPoints(take, take.current);
  return n >= 0 ? goTo(take, n) : take;
};

export const prevSection = (take: Take): Take => {
  for (let s = take.current - 1; s >= 0; s--) if (take.sections[s]!.points.length) return goTo(take, s);
  return take;
};

/** Every point of the current section back to waiting. */
export function resetSection(take: Take): Take {
  const ids = new Set((take.sections[take.current]?.points ?? []).map((p) => p.id));
  const state = { ...take.state };
  for (const id of ids) state[id] = "waiting";
  return {
    ...take,
    state,
    missed: take.missed.filter((id) => !ids.has(id)),
    done: take.done.filter((d) => !ids.has(d.id)),
  };
}

/** Jump to the section of a point (a tap on the Missed row). */
export function jumpTo(take: Take, id: string): Take {
  const at = locate(take, id);
  return at ? goTo(take, at.section) : take;
}

/** A tap on a point: done ↔ waiting. */
export function toggle(take: Take, id: string, at: number): Take {
  if (!locate(take, id)) return take;
  const state = { ...take.state };
  if (state[id] === "done") {
    state[id] = "waiting";
    return { ...take, state, done: take.done.filter((d) => d.id !== id) };
  }
  state[id] = "done";
  return maybeAdvance({
    ...take,
    state,
    missed: take.missed.filter((m) => m !== id),
    done: [...take.done, { id, at, source: "manual" }],
  });
}

function maybeAdvance(take: Take): Take {
  if (!take.autoAdvance) return take;
  const pts = take.sections[take.current]?.points ?? [];
  if (!pts.length || pts.some((p) => isOpen(take, p.id))) return take;
  return nextSection(take);
}

/** Apply one batch of verdicts at `at` ms into the take. */
export function applyVerdicts(take: Take, verdicts: readonly Verdict[], at: number): { take: Take; ticked: string[] } {
  const state = { ...take.state };
  let missed = [...take.missed];
  const done = [...take.done];
  const ticked: string[] = [];
  let furthest = take.current;
  for (const v of verdicts) {
    const loc = locate(take, v.id);
    if (!loc || state[v.id] === "done" || !Number.isFinite(v.p)) continue;
    const ahead = loc.section > take.current;
    if (v.p >= (ahead ? NEXT_DONE_P : DONE_P)) {
      state[v.id] = "done";
      missed = missed.filter((m) => m !== v.id);
      done.push({ id: v.id, at, source: v.source ?? "" });
      ticked.push(v.id);
      if (ahead) furthest = Math.max(furthest, loc.section);
    } else if (v.p >= STARTED_P && loc.section === take.current && state[v.id] === "waiting") {
      state[v.id] = "started";
    }
  }
  let next: Take = { ...take, state, missed, done };
  if (furthest > take.current) next = goTo(next, furthest);
  if (ticked.length) next = maybeAdvance(next);
  return { take: next, ticked };
}

/**
 * The take on edited points (Edit points during a take): a point keeps its state when
 * its text is unchanged, the take stays in the section with the same title (or index).
 */
export function rebase(take: Take, sections: ScriptSection[]): Take {
  const byText = (id: string) => locate(take, id)?.text.trim().toLowerCase();
  const fresh = newTake(sections, { autoAdvance: take.autoAdvance });
  const idOf = new Map<string, string>();
  for (const s of sections) for (const p of s.points) if (!idOf.has(p.text.trim().toLowerCase())) idOf.set(p.text.trim().toLowerCase(), p.id);
  const map = (id: string) => idOf.get(byText(id) ?? "\u0000");
  const state = { ...fresh.state };
  for (const [id, st] of Object.entries(take.state)) {
    const to = map(id);
    if (to) state[to] = st;
  }
  const title = take.sections[take.current]?.title;
  const byTitle = sections.findIndex((s) => s.title && s.title === title);
  const current = byTitle >= 0 ? byTitle : Math.min(take.current, Math.max(0, sections.length - 1));
  const done = take.done.flatMap((d) => (map(d.id) ? [{ ...d, id: map(d.id)! }] : []));
  const missed = take.missed.flatMap((id) => (map(id) ? [map(id)!] : []));
  return { ...fresh, state, current, done, missed };
}

export interface TakeReport {
  total: number;
  said: { id: string; text: string; at: number; source: string }[];
  /** Passed over and never said. */
  missed: { id: string; text: string }[];
  /** After where he stopped. */
  notReached: { id: string; text: string }[];
  /** Points said after a later one. */
  outOfOrder: number;
}

export function report(take: Take): TakeReport {
  const order = new Map<string, number>();
  let n = 0;
  for (const s of take.sections) for (const p of s.points) order.set(p.id, n++);
  const said = take.done.map((d) => ({
    ...d,
    text: locate(take, d.id)?.text ?? "",
  }));
  let max = -1;
  let outOfOrder = 0;
  for (const d of take.done) {
    const i = order.get(d.id) ?? 0;
    if (i < max) outOfOrder++;
    max = Math.max(max, i);
  }
  const open = (id: string) => take.state[id] !== "done";
  const missed = take.missed.filter(open).map((id) => ({ id, text: locate(take, id)?.text ?? "" }));
  const notReached: { id: string; text: string }[] = [];
  take.sections.forEach((s, si) => {
    if (si < take.current) return;
    for (const p of s.points) if (open(p.id) && !take.missed.includes(p.id)) notReached.push({ id: p.id, text: p.text });
  });
  // Open points of earlier sections that never reached the Missed row (he went back).
  take.sections.forEach((s, si) => {
    if (si >= take.current) return;
    for (const p of s.points) if (open(p.id) && !take.missed.includes(p.id)) missed.push({ id: p.id, text: p.text });
  });
  return { total: order.size, said, missed, notReached, outOfOrder };
}

const clock = (ms: number) => {
  const s = Math.max(0, Math.round(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};

export interface TakeInfo {
  startedAt: Date;
  durationMs: number;
  transcript: string;
  jevCalls: number;
  medianLatencyMs: number | null;
  locale: string;
  memo?: string;
}

/** The take as Markdown for the script's note (newest take on top of the log). */
export function reportMarkdown(take: Take, info: TakeInfo): string {
  const r = report(take);
  const when = info.startedAt.toLocaleString(info.locale, {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
  const lines = [`### Take ${when} · ${clock(info.durationMs)}`, ""];
  const stats = [`Said ${r.said.length} of ${r.total}`];
  if (r.outOfOrder) stats.push(`${r.outOfOrder} out of order`);
  if (info.medianLatencyMs != null) stats.push(`ticks in ${(info.medianLatencyMs / 1000).toFixed(1)} s`);
  stats.push(`Jev calls ${info.jevCalls}`);
  lines.push(stats.join(" · "), "");
  if (r.said.length) {
    lines.push("**Said, in order**");
    r.said.forEach((d, i) => lines.push(`${i + 1}. ${d.text} (${clock(d.at)}${d.source === "manual" ? ", ticked by hand" : ""})`));
    lines.push("");
  }
  if (r.missed.length) {
    lines.push("**Missed**");
    for (const m of r.missed) lines.push(`- ${m.text}`);
    lines.push("");
  }
  if (r.notReached.length) {
    lines.push("**Not reached**");
    for (const m of r.notReached) lines.push(`- ${m.text}`);
    lines.push("");
  }
  if (info.memo) lines.push(`Recording: ${info.memo}`, "");
  if (info.transcript.trim()) lines.push("**Transcript**", "", info.transcript.trim(), "");
  return lines.join("\n").trim();
}
