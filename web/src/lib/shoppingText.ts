/**
 * Free text to shopping-list items (docs/design/shopping-labels.md), the twin of the Rust
 * crate `crates/shopping-text` and of iOS `ShoppingText.swift`; all three pass
 * `tests/fixtures/shopping-split-cases.json`.
 *
 * The owner's rule: an item is a label, and a sentence never becomes one item. Command words
 * ("add", "to the shopping list", "aggiungi alla lista della spesa", "tilføj … til
 * indkøbslisten") go, the text splits on commas, semicolons, line breaks and conjunctions,
 * and a leading quantity is read ("4 baguettes", "500 g burro").
 */

export const MAX_PARTS = 50;
export const MAX_QUANTITY = 9999;

export interface ItemPart {
  name: string;
  quantity: number | null;
  unit: string | null;
}

const VERBS = new Set([
  "add", "please", "put", "buy", "aggiungi", "aggiungere", "metti", "compra", "comprare", "prendi",
  "tilfoej", "koeb", "saet", "husk",
]);
const COURTESY = new Set(["please", "grazie", "tak"]);
const PREPS = new Set(["to", "on", "onto", "in", "into", "for", "alla", "alle", "nella", "sulla", "sulle", "per", "a", "til", "paa", "i"]);
const DETS = new Set(["the", "my", "la", "mia", "min"]);
/** List names: [words, may stand without a preposition]. */
const LISTS: Array<[string[], boolean]> = [
  [["shopping", "list"], true],
  [["grocery", "list"], true],
  [["list"], false],
  [["shopping"], false],
  [["lista", "della", "spesa"], true],
  [["lista", "spesa"], true],
  [["lista"], false],
  [["spesa"], false],
  [["indkoebslisten"], true],
  [["indkoebsliste"], true],
  [["listen"], false],
  [["indkoebssedlen"], false],
];
const CONJUNCTIONS = new Set(["e", "ed", "and", "og", "&", "+"]);
const UNITS = new Set(["g", "gr", "hg", "kg", "l", "lt", "dl", "cl", "ml", "pz", "pcs", "pc", "stk"]);
const UNIT_LINKS = new Set(["di", "d", "of"]);

const WORD_TRIM = new Set([":", ",", ".", ";", "!", "?", '"', "'", "“", "”", "«", "»"]);
const PART_TRIM = new Set([".", ";", ":", "!", "?", '"', "'", "“", "”", "«", "»"]);

const SPECIAL: Record<string, string> = { æ: "ae", ø: "oe", å: "aa", œ: "oe", ß: "ss", ð: "d", þ: "th", ł: "l" };

function foldChars(text: string): string {
  return Array.from(text.toLowerCase())
    .map((c) => SPECIAL[c] ?? c.normalize("NFD").replace(/\p{M}+/gu, ""))
    .join("");
}

function trimBy(text: string, drop: (c: string) => boolean): string {
  const chars = Array.from(text);
  let lo = 0;
  let hi = chars.length;
  while (lo < hi && drop(chars[lo]!)) lo++;
  while (hi > lo && drop(chars[hi - 1]!)) hi--;
  return chars.slice(lo, hi).join("");
}

/** A word as the command tables compare it: surrounding punctuation trimmed, then folded. */
export function foldWord(word: string): string {
  return foldChars(trimBy(word, (c) => WORD_TRIM.has(c)));
}

function startsWith(words: string[], at: number, seq: string[]): boolean {
  if (words.length - at < seq.length) return false;
  return seq.every((s, i) => words[at + i] === s);
}

/** Lengths of every list phrase (`[PREP] [DET] LIST`) at the start of `f`. */
function listPhrases(f: string[]): number[] {
  const out: number[] = [];
  const hasPrep = f.length > 0 && PREPS.has(f[0]!);
  for (const prep of [false, true]) {
    if (prep && !hasPrep) continue;
    const p = prep ? 1 : 0;
    for (const det of [false, true]) {
      const k = p + (det ? 1 : 0);
      if (det && !(f.length > p && DETS.has(f[p]!))) continue;
      for (const [seq, alone] of LISTS) {
        if ((prep || alone) && f.length >= k && startsWith(f, k, seq)) out.push(k + seq.length);
      }
    }
  }
  return out;
}

function leadLen(f: string[]): number {
  const verb = f.length > 0 && VERBS.has(f[0]!) ? 1 : 0;
  return listPhrases(f).reduce((a, b) => Math.max(a, b), verb);
}

function trailLen(f: string[]): number {
  let best = f.length > 0 && COURTESY.has(f[f.length - 1]!) ? 1 : 0;
  for (let i = 0; i < f.length; i++) {
    if (listPhrases(f.slice(i)).includes(f.length - i)) {
      best = Math.max(best, f.length - i);
      break;
    }
  }
  return best;
}

/** The words left once leading and trailing command words are gone: [lo, hi), never empty. */
export function stripCommands(folded: string[]): [number, number] {
  let lo = 0;
  let hi = folded.length;
  for (;;) {
    let changed = false;
    let n = leadLen(folded.slice(lo, hi));
    if (n > 0 && lo + n < hi) {
      lo += n;
      changed = true;
    }
    n = trailLen(folded.slice(lo, hi));
    if (n > 0 && lo + n < hi) {
      hi -= n;
      changed = true;
    }
    if (!changed) return [lo, hi];
  }
}

/** A segment's words split at conjunctions that have a word on both sides. */
export function splitConjunctions(words: string[]): string[][] {
  const groups: string[][] = [];
  let cur: string[] = [];
  words.forEach((w, i) => {
    if (CONJUNCTIONS.has(foldWord(w)) && cur.length > 0 && i + 1 < words.length) {
      groups.push(cur);
      cur = [];
    } else {
      cur.push(w);
    }
  });
  if (cur.length > 0) groups.push(cur);
  return groups;
}

function number(s: string): number | null {
  if (!/^[0-9]{1,4}$/.test(s)) return null;
  const n = Number(s);
  return n >= 1 && n <= MAX_QUANTITY ? n : null;
}

function timesPrefix(s: string): string | null {
  return s.startsWith("x") || s.startsWith("×") ? s.slice(1) : null;
}

function timesSuffix(s: string): string | null {
  return s.endsWith("x") || s.endsWith("×") ? s.slice(0, -1) : null;
}

/** [quantity, unit, name words] of an item's words, or null. */
export function parseQuantity(words: string[]): [number, string | null, string[]] | null {
  const first = words[0];
  if (first === undefined) return null;
  let lead: [number, number] | null = null;
  const n0 = number(first);
  if (n0 !== null) {
    const times = words[1] === "x" || words[1] === "×";
    lead = [n0, times ? 2 : 1];
  } else {
    const suffix = timesSuffix(first);
    const n1 = suffix !== null ? number(suffix) : null;
    if (n1 !== null) lead = [n1, 1];
    else {
      const prefix = timesPrefix(first);
      const n2 = prefix !== null ? number(prefix) : null;
      if (n2 !== null) lead = [n2, 1];
    }
  }
  if (lead) {
    const [n, start] = lead;
    let next = start;
    let unit: string | null = null;
    const u = words[next] !== undefined ? foldWord(words[next]!) : null;
    if (u !== null && UNITS.has(u)) {
      unit = u;
      next++;
      if (words[next] !== undefined && UNIT_LINKS.has(foldWord(words[next]!))) next++;
    }
    return next < words.length ? [n, unit, words.slice(next)] : null;
  }
  if (words.length >= 2) {
    const prefix = timesPrefix(words[words.length - 1]!);
    const n = prefix !== null ? number(prefix) : null;
    if (n !== null) return [n, null, words.slice(0, -1)];
  }
  return null;
}

function part(text: string, isKnown: (s: string) => boolean): ItemPart | null {
  const t = trimBy(text, (c) => /\s/u.test(c) || PART_TRIM.has(c));
  if (!t) return null;
  if (isKnown(t)) return { name: t, quantity: null, unit: null };
  const words = t.split(/\s+/u).filter(Boolean);
  const q = parseQuantity(words);
  return q ? { name: q[2].join(" "), quantity: q[0], unit: q[1] } : { name: t, quantity: null, unit: null };
}

/**
 * The items of a free text. `isKnown(s)`: `s` is exactly a catalog name or alias, so it
 * stays whole ("sale e pepe").
 */
export function splitItems(text: string, isKnown: (s: string) => boolean): ItemPart[] {
  const words = text.replace(/[\n\r]/g, ",").split(/\s+/u).filter(Boolean);
  if (words.length === 0) return [];
  const whole = words.join(" ");
  if (isKnown(whole)) {
    const p = part(whole, isKnown);
    return p ? [p] : [];
  }
  const [lo, hi] = stripCommands(words.map(foldWord));
  const rest = words.slice(lo, hi).join(" ");
  const out: ItemPart[] = [];
  for (const segment of rest.split(/[,;]/)) {
    const seg = segment.trim();
    if (!seg) continue;
    const groups = isKnown(seg) ? [seg] : splitConjunctions(seg.split(/\s+/u).filter(Boolean)).map((g) => g.join(" "));
    for (const g of groups) {
      if (out.length === MAX_PARTS) return out;
      const p = part(g, isKnown);
      if (p) out.push(p);
    }
  }
  return out;
}
