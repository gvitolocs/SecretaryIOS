// Shopping catalog (docs/shopping-v2.md, "Catalog"): parsing, text folding and
// local search. Everything here is pure so the ranking can be unit tested.

export type Lang = "it" | "en" | "da";
export const LANGS: readonly Lang[] = ["it", "en", "da"];

export interface CatalogSection {
  id: string;
  name: string;
  icon: string;
  order: number;
}

export interface CatalogItem {
  id: string;
  name: string;
  names: Partial<Record<string, string>>;
  aliases: string[];
  section: string | null;
  icon: string;
}

export interface Catalog {
  version: number;
  lang: string;
  sections: CatalogSection[];
  items: CatalogItem[];
}

export const EMPTY_CATALOG: Catalog = { version: 0, lang: "it", sections: [], items: [] };

/** Section id used for items without a (known) section. */
export const OTHER_SECTION = "other";
export const DEFAULT_ICON = "🛒";

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim() ? v.trim() : null;
}

/** Defensive parse of `GET /v1/shopping/catalog`. Unknown fields are ignored. */
export function parseCatalog(data: unknown): Catalog {
  if (!isRecord(data)) return EMPTY_CATALOG;
  const sections: CatalogSection[] = [];
  if (Array.isArray(data.sections)) {
    data.sections.forEach((raw, i) => {
      if (!isRecord(raw)) return;
      const id = str(raw.id);
      if (!id) return;
      sections.push({
        id,
        name: str(raw.name) ?? id,
        icon: str(raw.icon) ?? "",
        order: typeof raw.order === "number" && Number.isFinite(raw.order) ? raw.order : i + 1,
      });
    });
  }
  sections.sort((a, b) => a.order - b.order);
  const items: CatalogItem[] = [];
  if (Array.isArray(data.items)) {
    for (const raw of data.items) {
      if (!isRecord(raw)) continue;
      const id = str(raw.id);
      const name = str(raw.name);
      if (!id || !name) continue;
      const names: Record<string, string> = {};
      if (isRecord(raw.names)) {
        for (const [k, v] of Object.entries(raw.names)) {
          const s = str(v);
          if (s) names[k] = s;
        }
      }
      items.push({
        id,
        name,
        names,
        aliases: Array.isArray(raw.aliases) ? raw.aliases.map(str).filter((a): a is string => a !== null) : [],
        section: str(raw.section),
        icon: str(raw.icon) ?? DEFAULT_ICON,
      });
    }
  }
  return {
    version: typeof data.version === "number" ? data.version : 1,
    lang: str(data.lang) ?? "it",
    sections,
    items,
  };
}

// Special letters folded before diacritics are stripped (docs/catalog.md, "Search normalisation").
const SPECIAL: Record<string, string> = { æ: "ae", ø: "oe", å: "aa", œ: "oe", ß: "ss", ð: "d", þ: "th", ł: "l" };

/** The catalog's normalise(): lowercase, fold special letters, strip diacritics, runs of non-letters/digits -> one space. */
export function fold(text: string): string {
  return text
    .toLowerCase()
    .replace(/[æøåœßðþł]/g, (c) => SPECIAL[c] ?? c)
    .normalize("NFD")
    .replace(/\p{M}+/gu, "")
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .trim();
}

/** Stable identity of an active row: the catalog id, else the folded name. */
export function itemKey(catalogId: string | null | undefined, name: string): string {
  return catalogId ? `c:${catalogId}` : `n:${fold(name)}`;
}

/** Display name of a catalog item in the UI language. */
export function catalogName(item: CatalogItem, lang: string): string {
  return item.names[lang] ?? item.name;
}

interface Indexed {
  item: CatalogItem;
  /** Folded display names: the primary one first. */
  names: string[];
  aliases: string[];
}

export interface SearchIndex {
  lang: string;
  entries: Indexed[];
  byId: Map<string, CatalogItem>;
}

export function buildIndex(catalog: Catalog, lang: string): SearchIndex {
  const entries = catalog.items.map((item) => {
    const primary = fold(catalogName(item, lang));
    const others = Object.values(item.names)
      .filter((n): n is string => typeof n === "string")
      .map(fold)
      .concat(fold(item.name))
      .filter((n) => n && n !== primary);
    return {
      item,
      names: [primary],
      // Names in other languages count as aliases (someone types "milk" in Italian mode).
      aliases: [...new Set([...item.aliases.map(fold), ...others])].filter(Boolean),
    };
  });
  return { lang, entries, byId: new Map(catalog.items.map((i) => [i.id, i])) };
}

function words(text: string): string[] {
  return text.split(" ").filter(Boolean);
}

/** Lower is better; null = no match. */
function tierFor(text: string, q: string, qWords: string[]): number | null {
  if (text === q) return 0;
  if (text.startsWith(q)) return 1;
  const w = words(text);
  if (w.some((x) => x.startsWith(q))) return 2;
  // Every query word starts some word of the text ("lat ave" -> "latte d avena").
  if (qWords.length > 1 && qWords.every((qw) => w.some((x) => x.startsWith(qw)))) return 3;
  if (text.includes(q)) return 6;
  return null;
}

export interface SearchOptions {
  limit?: number;
  /** Extra score per catalog id (recent/frequent items); higher ranks first within a tier. */
  boost?: (catalogId: string) => number;
}

/**
 * Rank catalog items for a query: exact, prefix and word-start matches on the
 * name first, then the same on aliases, then plain substrings. Ties go to the
 * boost (recently used), then the shorter name, then alphabetical order.
 */
export function searchCatalog(index: SearchIndex, query: string, opts: SearchOptions = {}): CatalogItem[] {
  const q = fold(query);
  if (!q) return [];
  const qWords = words(q);
  const limit = opts.limit ?? 24;
  const hits: Array<{ item: CatalogItem; tier: number; boost: number; len: number; name: string }> = [];
  for (const e of index.entries) {
    let best: number | null = null;
    for (const n of e.names) {
      const t = tierFor(n, q, qWords);
      if (t !== null && (best === null || t < best)) best = t;
    }
    for (const a of e.aliases) {
      const t = tierFor(a, q, qWords);
      if (t === null) continue;
      // Alias matches rank after name matches of the same kind; substrings stay last.
      const shifted = t === 6 ? 7 : t + 3;
      if (best === null || shifted < best) best = shifted;
    }
    if (best === null) continue;
    const name = e.names[0] ?? "";
    hits.push({ item: e.item, tier: best, boost: opts.boost?.(e.item.id) ?? 0, len: name.length, name });
  }
  hits.sort((a, b) => a.tier - b.tier || b.boost - a.boost || a.len - b.len || a.name.localeCompare(b.name));
  return hits.slice(0, limit).map((h) => h.item);
}

/** A catalog item whose folded name or alias equals the text exactly, if any. */
export function exactMatch(index: SearchIndex, text: string): CatalogItem | null {
  const q = fold(text);
  if (!q) return null;
  for (const e of index.entries) if (e.names.includes(q)) return e.item;
  for (const e of index.entries) if (e.aliases.includes(q)) return e.item;
  return null;
}
