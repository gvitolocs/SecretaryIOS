// Lucina's wardrobe: outfits, hairstyles and hair colours from
// GET /v1/avatar/wardrobe and the settings from GET /v1/avatar
// (docs/server-api.md, "Avatar wardrobe"). Everything from the network is
// untrusted and validated here. This module must not import the stage package
// at runtime (the stage chunk stays lazy).

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

/** A hairstyle part (`natural`, her own hair, has no glb). */
export interface Hairstyle {
  id: string;
  name?: Record<string, string> | string;
  glb?: string;
  thumbnail?: string;
  tags?: string[];
  hideTiara?: boolean;
  hideMaterials?: string[];
}

export interface HairColor {
  id: string;
  name?: Record<string, string> | string;
  /** `#rrggbb`. */
  hex: string;
}

export interface WardrobeFile {
  outfits: Outfit[];
  hairstyles: Hairstyle[];
  colors: HairColor[];
  /** Editor accessories and outfit parts: passed to the stage, which validates them. */
  accessories: unknown[];
  parts: unknown[];
}

export type Section = "gym" | "shopping" | "default";
export const SECTIONS: readonly Section[] = ["gym", "shopping", "default"];

/** Her own hair. */
export const NATURAL_HAIR = "natural";

export interface AvatarRule {
  outfit: string;
  hair: string;
}

export type AvatarRules = Record<Section, AvatarRule>;

export interface AvatarSettings {
  /** Pinned outfit (worn everywhere), or null. */
  outfit: string | null;
  /** Pinned hairstyle (`natural` = her own hair), or null. */
  hair: string | null;
  /** Hair colour id, or null for the natural colour. */
  hairColor: string | null;
  /** With nothing pinned, follow the app section. */
  auto: boolean;
  rules: AvatarRules;
  updatedMs: number;
  updatedBy: string;
  /** Saved looks of the character editor. */
  looks: SavedLook[];
  /** The look she wears (an id in `looks`), or null. */
  current: string | null;
}

/** One saved look (`look` is the stage's Look JSON; the stage normalizes it). */
export interface SavedLook {
  id: string;
  name: string;
  look: Record<string, unknown>;
  updatedMs: number;
}

export interface AvatarPatch {
  outfit?: string | null;
  hair?: string | null;
  hairColor?: string | null;
  auto?: boolean;
  rules?: Partial<Record<Section, Partial<AvatarRule>>>;
  /** Pick a saved look by id or name; null: none. */
  current?: string | null;
  saveLook?: { id?: string; name: string; look: Record<string, unknown> };
  deleteLook?: string;
}

export const ASSET_BASE = "/v1/assets/";

export const DEFAULT_RULES: AvatarRules = {
  gym: { outfit: "gym", hair: "ponytail" },
  shopping: { outfit: "chef", hair: NATURAL_HAIR },
  default: { outfit: "uniform", hair: NATURAL_HAIR },
};

export const DEFAULT_SETTINGS: AvatarSettings = {
  outfit: null,
  hair: null,
  hairColor: null,
  auto: true,
  rules: DEFAULT_RULES,
  updatedMs: 0,
  updatedBy: "",
  looks: [],
  current: null,
};

const ID_RE = /^[a-z0-9][a-z0-9_-]{0,39}$/;
const ASSET_RE = /^[a-z0-9][a-z0-9._-]{0,127}$/;
const HEX_RE = /^#[0-9a-f]{6}$/i;

export function isOutfitId(v: unknown): v is string {
  return typeof v === "string" && ID_RE.test(v);
}

function assetName(v: unknown, exts: string[]): v is string {
  return typeof v === "string" && ASSET_RE.test(v) && !v.includes("..") && exts.some((e) => v.endsWith(e));
}

function names(raw: unknown): Record<string, string> | string | undefined {
  if (typeof raw === "string" && raw.trim()) return raw.trim().slice(0, 80);
  if (!raw || typeof raw !== "object") return undefined;
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    if (/^[a-z]{2}$/.test(k) && typeof v === "string" && v.trim()) out[k] = v.trim().slice(0, 80);
  }
  return Object.keys(out).length ? out : undefined;
}

function tags(raw: unknown, max = 16, len = 32): string[] | undefined {
  if (!Array.isArray(raw)) return undefined;
  const t = raw.filter((x): x is string => typeof x === "string" && x.length > 0 && x.length <= len).slice(0, max);
  return t.length ? t : undefined;
}

function uniqueValid<T extends { id: string }>(raw: unknown, parse: (o: Record<string, unknown>) => T | null): T[] {
  if (!Array.isArray(raw)) return [];
  const out: T[] = [];
  const seen = new Set<string>();
  for (const r of raw) {
    if (!r || typeof r !== "object") continue;
    const e = parse(r as Record<string, unknown>);
    if (!e || seen.has(e.id)) continue;
    seen.add(e.id);
    out.push(e);
  }
  return out;
}

/** Valid outfits (unique ids, order kept); also takes the whole wardrobe file. */
export function parseWardrobe(raw: unknown): Outfit[] {
  if (raw && typeof raw === "object" && !Array.isArray(raw)) return parseWardrobe((raw as { outfits?: unknown }).outfits);
  return uniqueValid(raw, (o) => {
    if (!isOutfitId(o.id) || !assetName(o.glb, [".glb"])) return null;
    const e: Outfit = { id: o.id, glb: o.glb };
    const n = names(o.name);
    if (n) e.name = n;
    if (assetName(o.thumbnail, [".png", ".webp"])) e.thumbnail = o.thumbnail;
    if (assetName(o.manifest, [".json"])) e.manifest = o.manifest;
    const t = tags(o.tags);
    if (t) e.tags = t;
    return e;
  });
}

export function parseHairstyles(raw: unknown): Hairstyle[] {
  return uniqueValid(raw, (o) => {
    if (!isOutfitId(o.id)) return null;
    const natural = o.id === NATURAL_HAIR;
    if (!natural && !assetName(o.glb, [".glb"])) return null;
    const e: Hairstyle = { id: o.id };
    if (!natural) e.glb = o.glb as string;
    const n = names(o.name);
    if (n) e.name = n;
    if (assetName(o.thumbnail, [".png", ".webp"])) e.thumbnail = o.thumbnail;
    const t = tags(o.tags);
    if (t) e.tags = t;
    if (o.hideTiara === true) e.hideTiara = true;
    const hm = tags(o.hideMaterials, 32, 64);
    if (hm) e.hideMaterials = hm;
    return e;
  });
}

export function parseHairColors(raw: unknown): HairColor[] {
  return uniqueValid(raw, (o) => {
    if (!isOutfitId(o.id) || typeof o.hex !== "string" || !HEX_RE.test(o.hex)) return null;
    const c: HairColor = { id: o.id, hex: o.hex.toLowerCase() };
    const n = names(o.name);
    if (n) c.name = n;
    return c;
  });
}

/** `{outfits, hairstyles, colors}` (a bare array is the outfits). */
export function parseWardrobeFile(raw: unknown): WardrobeFile {
  const o = (raw && typeof raw === "object" && !Array.isArray(raw) ? raw : {}) as Record<string, unknown>;
  return {
    outfits: parseWardrobe(Array.isArray(raw) ? raw : o.outfits),
    hairstyles: parseHairstyles(o.hairstyles),
    colors: parseHairColors(o.colors),
    accessories: objects(o.accessories),
    parts: objects(o.parts),
  };
}

function parseRule(raw: unknown, fallback: AvatarRule): AvatarRule {
  if (isOutfitId(raw)) return { outfit: raw, hair: NATURAL_HAIR };
  const r = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  return {
    outfit: isOutfitId(r.outfit) ? r.outfit : fallback.outfit,
    hair: isOutfitId(r.hair) ? r.hair : fallback.hair,
  };
}

/** Settings from the server, with defaults for anything missing or malformed. */
export function parseSettings(raw: unknown): AvatarSettings {
  const r = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const rules = (r.rules && typeof r.rules === "object" ? r.rules : {}) as Record<string, unknown>;
  return {
    outfit: isOutfitId(r.outfit) ? r.outfit : null,
    hair: isOutfitId(r.hair) ? r.hair : null,
    hairColor: isOutfitId(r.hairColor) ? r.hairColor : null,
    auto: typeof r.auto === "boolean" ? r.auto : DEFAULT_SETTINGS.auto,
    rules: {
      gym: parseRule(rules.gym, DEFAULT_RULES.gym),
      shopping: parseRule(rules.shopping, DEFAULT_RULES.shopping),
      default: parseRule(rules.default, DEFAULT_RULES.default),
    },
    updatedMs: typeof r.updatedMs === "number" && Number.isFinite(r.updatedMs) ? r.updatedMs : 0,
    updatedBy: typeof r.updatedBy === "string" ? r.updatedBy.slice(0, 16) : "",
    ...parseLooks(r.looks, r.current),
  };
}

export function applyPatch(s: AvatarSettings, p: AvatarPatch): AvatarSettings {
  const rules = { ...s.rules };
  for (const sec of SECTIONS) {
    const rp = p.rules?.[sec];
    if (rp) rules[sec] = { ...rules[sec], ...rp };
  }
  return {
    ...s,
    ...(p.outfit !== undefined ? { outfit: p.outfit } : {}),
    ...(p.hair !== undefined ? { hair: p.hair } : {}),
    ...(p.hairColor !== undefined ? { hairColor: p.hairColor } : {}),
    ...(p.auto !== undefined ? { auto: p.auto } : {}),
    rules,
    ...lookPatch(s, p),
  };
}

/** The app section a path belongs to. */
export function sectionFor(pathname: string): Section {
  if (pathname === "/gym" || pathname.startsWith("/gym/")) return "gym";
  if (pathname === "/shopping" || pathname.startsWith("/shopping/")) return "shopping";
  return "default";
}

/** The rule in force: the section's with auto, else the default one. */
export function ruleFor(s: AvatarSettings, section: Section): AvatarRule {
  return s.auto ? s.rules[section] : s.rules.default;
}

/** Wanted outfit id: the pin, else the rule's. */
export function wantedOutfit(s: AvatarSettings, section: Section): string {
  return s.outfit ?? ruleFor(s, section).outfit;
}

/** Wanted hairstyle id: the pin, else the rule's. */
export function wantedHair(s: AvatarSettings, section: Section): string {
  return s.hair ?? ruleFor(s, section).hair;
}

/**
 * The outfit to put on, among those installed: the wanted one, else the
 * default rule's, else the first outfit; null when the wardrobe is empty.
 */
export function resolveOutfit(s: AvatarSettings, section: Section, outfits: readonly Outfit[]): Outfit | null {
  const find = (id: string) => outfits.find((o) => o.id === id);
  return find(wantedOutfit(s, section)) ?? find(s.rules.default.outfit) ?? outfits[0] ?? null;
}

/** The hairstyle to show: the wanted one when installed, else her own hair. */
export function resolveHair(s: AvatarSettings, section: Section, hairstyles: readonly Hairstyle[]): string {
  const want = wantedHair(s, section);
  return want !== NATURAL_HAIR && hairstyles.some((h) => h.id === want && h.glb) ? want : NATURAL_HAIR;
}

/** The hair colour to show: the chosen one when installed, else null (natural). */
export function resolveHairColor(s: AvatarSettings, colors: readonly HairColor[]): string | null {
  return s.hairColor && colors.some((c) => c.id === s.hairColor) ? s.hairColor : null;
}

/** Hairstyles to offer: `natural` first (always), then the installed ones. */
export function hairChoices(hairstyles: readonly Hairstyle[]): Hairstyle[] {
  const natural = hairstyles.find((h) => h.id === NATURAL_HAIR) ?? { id: NATURAL_HAIR };
  return [natural, ...hairstyles.filter((h) => h.id !== NATURAL_HAIR)];
}

export function outfitName(o: { id: string; name?: Record<string, string> | string }, lang: string): string {
  if (typeof o.name === "string") return o.name;
  const n = o.name ?? {};
  return n[lang] ?? n.en ?? Object.values(n)[0] ?? o.id;
}

export function thumbnailUrl(o: { thumbnail?: string }): string | null {
  return o.thumbnail ? ASSET_BASE + o.thumbnail : null;
}

// ---- Categories (the Wardrobe's outfit filter) -----------------------------

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

/** Filters to show: "all" plus each category that has outfits, in a fixed order. */
export function categoriesPresent(outfits: readonly Outfit[]): (Category | "all")[] {
  const present = new Set(outfits.map(categoryOf));
  return ["all", ...CATEGORIES.filter((c) => present.has(c))];
}

// ---- Saved looks (the character editor) ------------------------------------

/** Plain objects of an untrusted list (at most 200). */
function objects(raw: unknown): unknown[] {
  return Array.isArray(raw) ? raw.filter((x) => x && typeof x === "object" && !Array.isArray(x)).slice(0, 200) : [];
}

/** At most 20 looks with valid ids and names; `current` only when it names one of them. */
export function parseLooks(raw: unknown, current: unknown): { looks: SavedLook[]; current: string | null } {
  const looks = uniqueValid(raw, (o) => {
    if (!isOutfitId(o.id) || typeof o.name !== "string" || !o.name.trim()) return null;
    if (!o.look || typeof o.look !== "object" || Array.isArray(o.look)) return null;
    return {
      id: o.id,
      name: o.name.trim().slice(0, 40),
      look: o.look as Record<string, unknown>,
      updatedMs: typeof o.updatedMs === "number" && Number.isFinite(o.updatedMs) ? o.updatedMs : 0,
    };
  }).slice(0, 20);
  return { looks, current: typeof current === "string" && looks.some((l) => l.id === current) ? current : null };
}

/** The optimistic effect of the look fields of a patch (the server's answer replaces it). */
function lookPatch(s: AvatarSettings, p: AvatarPatch): { looks: SavedLook[]; current: string | null } {
  let looks = s.looks;
  let current = s.current;
  if (p.deleteLook) {
    looks = looks.filter((l) => l.id !== p.deleteLook);
    if (current === p.deleteLook) current = null;
  }
  if (p.saveLook?.id) {
    const saved: SavedLook = { id: p.saveLook.id, name: p.saveLook.name, look: p.saveLook.look, updatedMs: Date.now() };
    looks = looks.some((l) => l.id === saved.id) ? looks.map((l) => (l.id === saved.id ? saved : l)) : [...looks, saved];
  }
  if (p.current !== undefined) current = p.current === null ? null : (looks.find((l) => l.id === p.current)?.id ?? current);
  return { looks, current };
}

/** The look she wears now, or null (the model as authored). */
export function wornLook(s: AvatarSettings): Record<string, unknown> | null {
  return s.current ? (s.looks.find((l) => l.id === s.current)?.look ?? null) : null;
}

const obj = (v: unknown): Record<string, unknown> => (v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : {});

/** A look's outfit override: `base` when it picks parts, else its outfit id, else null. */
export function lookOutfit(look: Record<string, unknown> | null, outfits: readonly Outfit[]): string | null {
  if (!look) return null;
  const o = obj(look.outfit);
  const parts = obj(o.parts);
  if (Object.values(parts).some(isOutfitId) && outfits.some((x) => x.id === "base")) return "base";
  return isOutfitId(o.id) && outfits.some((x) => x.id === o.id) ? o.id : null;
}

/** A look's hairstyle override (installed ones only), else null. */
export function lookHair(look: Record<string, unknown> | null, hairstyles: readonly Hairstyle[]): string | null {
  const h = obj(look?.hair).style;
  return isOutfitId(h) && (h === NATURAL_HAIR || hairstyles.some((x) => x.id === h && x.glb)) ? h : null;
}

/** A look's hair colour (`#rrggbb`), else null. */
export function lookHairColor(look: Record<string, unknown> | null): string | null {
  const c = obj(look?.hair).color;
  return typeof c === "string" && HEX_RE.test(c) ? c.toLowerCase() : null;
}
