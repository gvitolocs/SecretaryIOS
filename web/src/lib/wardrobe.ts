// Lucina's wardrobe: the outfit list from GET /v1/avatar/wardrobe and the
// settings from GET /v1/avatar (docs/server-api.md, "Avatar wardrobe").
// Everything from the network is untrusted and validated here. This module
// must not import the stage package at runtime (the stage chunk stays lazy).

import type { MessageKey } from "../i18n";

export interface Outfit {
  id: string;
  /** Per language (`it`, `en`, `da`) or one string. */
  name?: Record<string, string> | string;
  glb: string;
  thumbnail?: string;
  tags?: string[];
  manifest?: string;
}

export type Section = "gym" | "shopping" | "default";

export interface AvatarRules {
  gym: string;
  shopping: string;
  default: string;
}

export interface AvatarSettings {
  /** Pinned outfit (worn everywhere), or null. */
  outfit: string | null;
  /** With no pin, follow the app section. */
  auto: boolean;
  rules: AvatarRules;
  updatedMs: number;
  updatedBy: string;
}

export interface AvatarPatch {
  outfit?: string | null;
  auto?: boolean;
  rules?: Partial<AvatarRules>;
}

export const ASSET_BASE = "/v1/assets/";

export const DEFAULT_SETTINGS: AvatarSettings = {
  outfit: null,
  auto: true,
  rules: { gym: "gym", shopping: "chef", default: "uniform" },
  updatedMs: 0,
  updatedBy: "",
};

const ID_RE = /^[a-z0-9][a-z0-9_-]{0,39}$/;
const ASSET_RE = /^[a-z0-9][a-z0-9._-]{0,127}$/;

export function isOutfitId(v: unknown): v is string {
  return typeof v === "string" && ID_RE.test(v);
}

function assetName(v: unknown, exts: string[]): v is string {
  return typeof v === "string" && ASSET_RE.test(v) && !v.includes("..") && exts.some((e) => v.endsWith(e));
}

/** Valid entries only (unique ids, order kept). */
export function parseWardrobe(raw: unknown): Outfit[] {
  if (!Array.isArray(raw)) return [];
  const out: Outfit[] = [];
  const seen = new Set<string>();
  for (const r of raw) {
    if (!r || typeof r !== "object") continue;
    const o = r as Record<string, unknown>;
    if (!isOutfitId(o.id) || seen.has(o.id) || !assetName(o.glb, [".glb"])) continue;
    const e: Outfit = { id: o.id, glb: o.glb };
    if (typeof o.name === "string" && o.name.trim()) e.name = o.name.trim().slice(0, 80);
    else if (o.name && typeof o.name === "object") {
      const names: Record<string, string> = {};
      for (const [k, v] of Object.entries(o.name as Record<string, unknown>)) {
        if (/^[a-z]{2}$/.test(k) && typeof v === "string" && v.trim()) names[k] = v.trim().slice(0, 80);
      }
      if (Object.keys(names).length) e.name = names;
    }
    if (assetName(o.thumbnail, [".png", ".webp"])) e.thumbnail = o.thumbnail;
    if (assetName(o.manifest, [".json"])) e.manifest = o.manifest;
    if (Array.isArray(o.tags)) {
      const tags = o.tags.filter((t): t is string => typeof t === "string" && t.length > 0 && t.length <= 32).slice(0, 16);
      if (tags.length) e.tags = tags;
    }
    seen.add(e.id);
    out.push(e);
  }
  return out;
}

/** Settings from the server, with defaults for anything missing or malformed. */
export function parseSettings(raw: unknown): AvatarSettings {
  const r = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const rules = (r.rules && typeof r.rules === "object" ? r.rules : {}) as Record<string, unknown>;
  const rule = (k: Section) => (isOutfitId(rules[k]) ? (rules[k] as string) : DEFAULT_SETTINGS.rules[k]);
  return {
    outfit: isOutfitId(r.outfit) ? r.outfit : null,
    auto: typeof r.auto === "boolean" ? r.auto : DEFAULT_SETTINGS.auto,
    rules: { gym: rule("gym"), shopping: rule("shopping"), default: rule("default") },
    updatedMs: typeof r.updatedMs === "number" && Number.isFinite(r.updatedMs) ? r.updatedMs : 0,
    updatedBy: typeof r.updatedBy === "string" ? r.updatedBy.slice(0, 16) : "",
  };
}

export function applyPatch(s: AvatarSettings, p: AvatarPatch): AvatarSettings {
  return {
    ...s,
    ...(p.outfit !== undefined ? { outfit: p.outfit } : {}),
    ...(p.auto !== undefined ? { auto: p.auto } : {}),
    rules: { ...s.rules, ...(p.rules ?? {}) },
  };
}

/** The app section a path belongs to. */
export function sectionFor(pathname: string): Section {
  if (pathname === "/gym" || pathname.startsWith("/gym/")) return "gym";
  if (pathname === "/shopping" || pathname.startsWith("/shopping/")) return "shopping";
  return "default";
}

/** Wanted outfit id: the pin, else the section's rule (auto) or the default rule. */
export function wantedOutfit(s: AvatarSettings, section: Section): string {
  return s.outfit ?? (s.auto ? s.rules[section] : s.rules.default);
}

/**
 * The outfit to put on, among those installed: the wanted one, else the
 * default rule, else the first outfit; null when the wardrobe is empty.
 */
export function resolveOutfit(s: AvatarSettings, section: Section, outfits: readonly Outfit[]): Outfit | null {
  const find = (id: string) => outfits.find((o) => o.id === id);
  return find(wantedOutfit(s, section)) ?? find(s.rules.default) ?? outfits[0] ?? null;
}

export function outfitName(o: Outfit, lang: string): string {
  if (typeof o.name === "string") return o.name;
  const n = o.name ?? {};
  return n[lang] ?? n.en ?? Object.values(n)[0] ?? o.id;
}

export function thumbnailUrl(o: Outfit): string | null {
  return o.thumbnail ? ASSET_BASE + o.thumbnail : null;
}

// ---- Categories (the Wardrobe's tabs) ---------------------------------------

export const CATEGORIES = ["uniform", "casual", "sport", "work", "formal", "fantasy", "other"] as const;
export type Category = (typeof CATEGORIES)[number];

export const CATEGORY_LABELS: Record<Category | "all", MessageKey> = {
  all: "wardrobe.catAll",
  uniform: "wardrobe.catUniform",
  casual: "wardrobe.catCasual",
  sport: "wardrobe.catSport",
  work: "wardrobe.catWork",
  formal: "wardrobe.catFormal",
  fantasy: "wardrobe.catFantasy",
  other: "wardrobe.catOther",
};

/** Tag synonyms → category, so the wardrobe file can be loose. */
const TAG_CATEGORY: Record<string, Category> = {
  uniform: "uniform",
  default: "uniform",
  armor: "uniform",
  casual: "casual",
  everyday: "casual",
  home: "casual",
  sport: "sport",
  gym: "sport",
  fitness: "sport",
  training: "sport",
  work: "work",
  chef: "work",
  maid: "work",
  kitchen: "work",
  formal: "formal",
  dress: "formal",
  party: "formal",
  fantasy: "fantasy",
  costume: "fantasy",
  cosplay: "fantasy",
};

export function categoryOf(o: Outfit): Category {
  for (const tag of o.tags ?? []) {
    const c = TAG_CATEGORY[tag.toLowerCase()];
    if (c) return c;
  }
  return TAG_CATEGORY[o.id] ?? "other";
}

/** Tabs to show: "all" plus each category that has outfits, in a fixed order. */
export function categoriesPresent(outfits: readonly Outfit[]): (Category | "all")[] {
  const present = new Set(outfits.map(categoryOf));
  return ["all", ...CATEGORIES.filter((c) => present.has(c))];
}
