import { describe, expect, it } from "vitest";
import { parseScript, replaceTopTake, serializeScript, splitNote, vocabulary, withScript, withTake } from "./prompterScript";
import {
  applyVerdicts,
  candidates,
  currentPoint,
  jumpTo,
  newTake,
  rebase,
  nextSection,
  prevSection,
  report,
  reportMarkdown,
  resetSection,
  toggle,
  type Take,
} from "./prompterTake";

const SCRIPT = `## Intro
- Lucina Prompter puts talking points on the screen
- Each point is crossed out while I talk
- It costs almost nothing per take

## How it works
- Whisper runs on my own server
- Jev picks the point I just covered

## Wrap up
- Thanks for watching`;

const take = (opts = {}) => newTake(parseScript(SCRIPT), opts);
const state = (t: Take, id: string) => t.state[id];
const hear = (t: Take, v: [string, number][], at = 1000) =>
  applyVerdicts(
    t,
    v.map(([id, p]) => ({ id, p, source: "jev" })),
    at,
  );

describe("prompter script", () => {
  it("parses sections, bullets, numbers and loose lines with positional ids", () => {
    const s = parseScript("Opening line\n# Part one\n1. First\n* [ ] Second\n\n### Part two\n• Third\n---\n### Take x\n- not a point");
    expect(s.map((x) => x.title)).toEqual(["", "Part one", "Part two"]);
    expect(s[0]!.points).toEqual([{ id: "s0p0", text: "Opening line" }]);
    expect(s[1]!.points.map((p) => p.text)).toEqual(["First", "Second"]);
    expect(s[2]!.points).toEqual([{ id: "s2p0", text: "Third" }]);
  });

  it("keeps the take log when the script changes, and puts new takes on top", () => {
    const note = withTake(SCRIPT, "### Take A");
    expect(splitNote(note).takes).toBe("### Take A");
    const two = withTake(note, "### Take B");
    expect(splitNote(two).takes).toBe("### Take B\n\n### Take A");
    const edited = withScript(two, parseScript("## New\n- Only point"));
    expect(splitNote(edited)).toEqual({
      script: "## New\n- Only point",
      takes: "### Take B\n\n### Take A",
    });
    expect(serializeScript(parseScript(SCRIPT))).toBe(SCRIPT);
    expect(splitNote(replaceTopTake(two, "### Take B", "### Take B\n\nsummary")!).takes).toBe("### Take B\n\nsummary\n\n### Take A");
    expect(replaceTopTake(two, "### Take A", "x")).toBeNull();
  });

  it("picks names for whisper's vocabulary", () => {
    expect(vocabulary(parseScript(SCRIPT))).toEqual(["Intro", "Lucina", "Prompter", "Whisper", "Jev", "Wrap"]);
    expect(vocabulary(parseScript("- the GPU and macOS"))).toEqual(["GPU", "macOS"]);
  });
});

describe("prompter take", () => {
  it("starts at the first point and crosses points out from 0.75", () => {
    let t = take();
    expect(currentPoint(t)).toBe("s0p0");
    ({ take: t } = hear(t, [["s0p0", 0.74]]));
    expect(state(t, "s0p0")).toBe("started");
    ({ take: t } = hear(t, [["s0p0", 0.8]]));
    expect(state(t, "s0p0")).toBe("done");
    expect(currentPoint(t)).toBe("s0p1");
    expect(t.done.map((d) => d.id)).toEqual(["s0p0"]);
  });

  it("accepts points in any order within a section", () => {
    let t = take();
    ({ take: t } = hear(t, [["s0p2", 0.9]]));
    ({ take: t } = hear(t, [["s0p0", 0.9]]));
    expect(t.current).toBe(0);
    expect(t.missed).toEqual([]);
    expect(currentPoint(t)).toBe("s0p1");
    expect(report(t).outOfOrder).toBe(1);
  });

  it("ignores repeats of a done point", () => {
    let t = take();
    ({ take: t } = hear(t, [["s0p0", 0.9]], 1000));
    const { take: again, ticked } = hear(
      t,
      [
        ["s0p0", 0.99],
        ["s0p0", 0.2],
      ],
      5000,
    );
    expect(ticked).toEqual([]);
    expect(again.done).toEqual([{ id: "s0p0", at: 1000, source: "jev" }]);
    expect(state(again, "s0p0")).toBe("done");
  });

  it("moves on when a next-section point is clearly made, sending open points to Missed", () => {
    let t = take();
    ({ take: t } = hear(t, [["s0p0", 0.9]]));
    ({ take: t } = hear(t, [["s1p0", 0.8]]));
    expect(t.current).toBe(0);
    expect(state(t, "s1p0")).toBe("waiting");
    ({ take: t } = hear(t, [["s1p0", 0.9]]));
    expect(t.current).toBe(1);
    expect(t.missed).toEqual(["s0p1", "s0p2"]);
    expect(candidates(t).map((c) => `${c.kind}:${c.id}`)).toEqual(["current:s1p1", "missed:s0p1", "missed:s0p2", "next:s2p0"]);
  });

  it("ticks a missed point said later and clears it from the row", () => {
    let t = take();
    t = nextSection(t);
    expect(t.missed).toEqual(["s0p0", "s0p1", "s0p2"]);
    ({ take: t } = hear(t, [["s0p1", 0.8]]));
    expect(t.missed).toEqual(["s0p0", "s0p2"]);
    expect(t.current).toBe(1);
  });

  it("jumps back from the Missed row and forward again", () => {
    let t = nextSection(take());
    t = jumpTo(t, "s0p1");
    expect(t.current).toBe(0);
    expect(candidates(t).filter((c) => c.kind === "missed")).toEqual([]);
    t = nextSection(t);
    expect(t.current).toBe(1);
    expect(t.missed).toEqual(["s0p0", "s0p1", "s0p2"]);
    t = prevSection(prevSection(t));
    expect(t.current).toBe(0);
  });

  it("resets a section: back to waiting, out of Missed and of the order", () => {
    let t = take();
    ({ take: t } = hear(t, [
      ["s0p0", 0.9],
      ["s0p1", 0.5],
    ]));
    t = nextSection(t);
    t = jumpTo(t, "s0p0");
    t = resetSection(t);
    expect(["s0p0", "s0p1", "s0p2"].map((id) => state(t, id))).toEqual(["waiting", "waiting", "waiting"]);
    expect(t.missed).toEqual([]);
    expect(t.done).toEqual([]);
  });

  it("moves on by itself when the section is done, unless told not to", () => {
    let t = take();
    ({ take: t } = hear(t, [
      ["s0p0", 0.9],
      ["s0p1", 0.9],
      ["s0p2", 0.9],
    ]));
    expect(t.current).toBe(1);
    expect(t.missed).toEqual([]);
    let manual = take({ autoAdvance: false });
    ({ take: manual } = hear(manual, [
      ["s0p0", 0.9],
      ["s0p1", 0.9],
      ["s0p2", 0.9],
    ]));
    expect(manual.current).toBe(0);
  });

  it("toggles a point by hand", () => {
    let t = toggle(take(), "s0p1", 2000);
    expect(state(t, "s0p1")).toBe("done");
    expect(t.done).toEqual([{ id: "s0p1", at: 2000, source: "manual" }]);
    t = toggle(t, "s0p1", 3000);
    expect(state(t, "s0p1")).toBe("waiting");
    expect(t.done).toEqual([]);
  });

  it("keeps the state of unchanged points when the points are edited mid-take", () => {
    let t = take();
    ({ take: t } = hear(t, [
      ["s0p0", 0.9],
      ["s1p0", 0.9],
    ]));
    expect(t.current).toBe(1);
    const edited = parseScript(SCRIPT.replace("- Jev picks the point I just covered", "- A brand new point\n- Jev picks the point I just covered"));
    const r = rebase(t, edited);
    expect(r.current).toBe(1);
    expect(r.state["s0p0"]).toBe("done");
    expect(r.state["s1p1"]).toBe("waiting");
    expect(r.state["s1p0"]).toBe("done");
    expect(r.missed).toEqual(["s0p1", "s0p2"]);
    expect(r.done.map((d) => d.id)).toEqual(["s0p0", "s1p0"]);
  });

  it("drops unknown ids and broken probabilities", () => {
    const t = take();
    const { take: same, ticked } = applyVerdicts(
      t,
      [
        { id: "zz", p: 1 },
        { id: "s0p0", p: Number.NaN },
      ],
      0,
    );
    expect(ticked).toEqual([]);
    expect(same.state).toEqual(t.state);
  });

  it("reports said, missed and not reached, and writes the take for the note", () => {
    let t = take();
    ({ take: t } = hear(t, [["s0p0", 0.9]], 4000));
    ({ take: t } = hear(t, [["s1p0", 0.95]], 15000));
    const r = report(t);
    expect(r.said.map((s) => s.id)).toEqual(["s0p0", "s1p0"]);
    expect(r.missed.map((s) => s.id)).toEqual(["s0p1", "s0p2"]);
    expect(r.notReached.map((s) => s.id)).toEqual(["s1p1", "s2p0"]);
    const md = reportMarkdown(t, {
      startedAt: new Date("2026-10-28T22:40:00Z"),
      durationMs: 72_000,
      transcript: "Hi, so this is Lucina Prompter.",
      jevCalls: 7,
      medianLatencyMs: 820,
      locale: "en-GB",
    });
    expect(md).toMatch(/^### Take .* · 1:12/);
    expect(md).toContain("Said 2 of 6 · ticks in 0.8 s · Jev calls 7");
    expect(md).toContain("1. Lucina Prompter puts talking points on the screen (0:04)");
    expect(md).toContain("**Missed**\n- Each point is crossed out while I talk");
    expect(md).toContain("**Not reached**\n- Jev picks the point I just covered\n- Thanks for watching");
    expect(md).toContain("**Transcript**\n\nHi, so this is Lucina Prompter.");
  });
});
