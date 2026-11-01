// Lucina Prompter scripts (docs/prompter.md). A script is a Keep note with the label
// "prompter": its Markdown is the talking points, and the take log sits under a line.
//
//   ## Intro                    ← a section (any heading level)
//   - Lucina Prompter on screen ← a point (-, *, •, 1. or a plain line)
//
//   ---                         ← everything below is the take log, never points
//   ### Take 28 Oct 23:40 · 3:12
//
// Ids are positional (`s<section>p<point>`), the same rule lucina-memo's replay uses.

export interface ScriptPoint {
  id: string;
  text: string;
}

export interface ScriptSection {
  title: string;
  points: ScriptPoint[];
}

export const PROMPTER_LABEL = "prompter";
/** The line that separates the points from the take log. */
export const TAKES_RULE = "---";

const HEADING = /^#{1,6}\s+(.*)$/;
const BULLET = /^(?:[-*•+]|\d+[.)])\s+(.*)$/;
const CHECKBOX = /^\[[ xX]\]\s+/;

/** Split a note into its script part and its take log. */
export function splitNote(content: string): { script: string; takes: string } {
  const lines = content.split(/\r?\n/);
  const i = lines.findIndex((l) => l.trim() === TAKES_RULE);
  if (i < 0) return { script: content.replace(/\s+$/, ""), takes: "" };
  return {
    script: lines.slice(0, i).join("\n").replace(/\s+$/, ""),
    takes: lines
      .slice(i + 1)
      .join("\n")
      .replace(/^\s+/, ""),
  };
}

export function parseScript(content: string): ScriptSection[] {
  const out: ScriptSection[] = [];
  for (const raw of splitNote(content).script.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    const h = HEADING.exec(line);
    if (h) {
      out.push({ title: h[1]!.trim(), points: [] });
      continue;
    }
    const b = BULLET.exec(line);
    const text = (b ? b[1]! : line).replace(CHECKBOX, "").trim();
    if (!text) continue;
    if (!out.length) out.push({ title: "", points: [] });
    const s = out[out.length - 1]!;
    s.points.push({ id: "", text });
  }
  const kept = out.filter((s) => s.points.length > 0 || s.title);
  return renumber(kept);
}

/** Give every point its positional id again (after an edit). */
export function renumber(sections: ScriptSection[]): ScriptSection[] {
  return sections.map((s, i) => ({
    title: s.title,
    points: s.points.map((p, j) => ({ id: `s${i}p${j}`, text: p.text })),
  }));
}

export function serializeScript(sections: ScriptSection[]): string {
  return sections.map((s) => [s.title ? `## ${s.title}` : "", ...s.points.map((p) => `- ${p.text}`)].filter(Boolean).join("\n")).join("\n\n");
}

/** The note's content with a new script and the take log kept. */
export function withScript(content: string, sections: ScriptSection[]): string {
  const { takes } = splitNote(content);
  const script = serializeScript(sections);
  return takes ? `${script}\n\n${TAKES_RULE}\n\n${takes}` : script;
}

/** The note's content with one more take at the top of the log. */
export function withTake(content: string, takeMarkdown: string): string {
  const { script, takes } = splitNote(content);
  return `${script}\n\n${TAKES_RULE}\n\n${takeMarkdown.trim()}${takes ? `\n\n${takes}` : ""}`;
}

/** The note with its newest take replaced (Lucina's summary added), or null when another take came first. */
export function replaceTopTake(content: string, oldTake: string, newTake: string): string | null {
  const { script, takes } = splitNote(content);
  if (!takes.startsWith(oldTake.trim())) return null;
  return `${script}\n\n${TAKES_RULE}\n\n${newTake.trim()}${takes.slice(oldTake.trim().length)}`;
}

export function pointCount(sections: ScriptSection[]): number {
  return sections.reduce((n, s) => n + s.points.length, 0);
}

/** Capitalised only because they start a sentence: not worth steering whisper with. */
const STARTERS = new Set(
  (
    "a an and as at but by each every for from how if in it its my no now of on or our so that the then these this " +
    "those to we what when where which while who why with you your thanks thank " +
    "il lo la le gli un una di da che come questo questa quando perché e ma se poi grazie " +
    "det den de en et og jeg vi du hvordan hvad når hvis men så tak"
  ).split(" "),
);

/** Names and jargon that steer whisper's spelling: capitalised words, acronyms, camelCase. */
export function vocabulary(sections: ScriptSection[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  const add = (w: string) => {
    const k = w.toLowerCase();
    if (w.length < 2 || seen.has(k)) return;
    seen.add(k);
    out.push(w);
  };
  for (const text of sections.flatMap((s) => [s.title, ...s.points.map((p) => p.text)])) {
    const words = text.split(/[^\p{L}\p{N}'-]+/u).filter(Boolean);
    for (const w of words) {
      if (/^\p{Lu}{2,}$/u.test(w) || /\p{Ll}\p{Lu}/u.test(w) || (/^\p{Lu}/u.test(w) && !STARTERS.has(w.toLowerCase()))) add(w);
    }
  }
  return out.slice(0, 40);
}
