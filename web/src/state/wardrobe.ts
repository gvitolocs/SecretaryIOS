import { createMemo, createRoot, createSignal } from "solid-js";
import { avatarApi } from "../api/avatar";
import { toApiError } from "../api/client";
import type { TimelineItem } from "../api/timeline";
import { t } from "../i18n";
import { readJSON, writeJSON } from "../lib/storage";
import {
  applyPatch,
  DEFAULT_SETTINGS,
  isOutfitId,
  lookHair,
  lookHairColor,
  lookOutfit,
  wornLook as wornLookOf,
  parseSettings,
  parseHairColors,
  parseHairstyles,
  parseWardrobe,
  resolveHair,
  resolveHairColor,
  resolveOutfit,
  sectionFor,
  type AvatarPatch,
  type AvatarSettings,
  type HairColor,
  type Hairstyle,
  type Outfit,
  type Section,
} from "../lib/wardrobe";
import { onTimelineItem } from "./chat";

// Lucina's wardrobe: what she wears and when. The server keeps the choice
// (GET/PUT /v1/avatar); localStorage only caches it so her first frame is
// already in the right outfit. The stage follows `currentOutfit()`.

export const CACHE_KEY = "secretary.avatar.v2";
/** A live `avatar_outfit` timeline item older than this is history, not news. */
export const LIVE_ITEM_MS = 10 * 60_000;
/** Coming back to the tab refreshes at most this often. */
export const REFRESH_MIN_MS = 30_000;

type Status = "idle" | "loading" | "ready" | "error";

interface Cached {
  settings: AvatarSettings;
  outfits: Outfit[];
  hairstyles: Hairstyle[];
  colors: HairColor[];
  accessories?: unknown[];
  parts?: unknown[];
}

function readCache(): Cached | null {
  const raw = readJSON("local", CACHE_KEY) as Partial<Cached> | null;
  if (!raw || typeof raw !== "object") return null;
  return {
    settings: parseSettings(raw.settings),
    outfits: parseWardrobe(raw.outfits),
    hairstyles: parseHairstyles(raw.hairstyles),
    colors: parseHairColors(raw.colors),
    accessories: Array.isArray(raw.accessories) ? raw.accessories : [],
    parts: Array.isArray(raw.parts) ? raw.parts : [],
  };
}

const store = createRoot(() => {
  const cached = readCache();
  const [settings, setSettings] = createSignal<AvatarSettings>(cached?.settings ?? DEFAULT_SETTINGS);
  const [outfits, setOutfits] = createSignal<Outfit[]>(cached?.outfits ?? []);
  const [hairstyles, setHairstyles] = createSignal<Hairstyle[]>(cached?.hairstyles ?? []);
  const [colors, setColors] = createSignal<HairColor[]>(cached?.colors ?? []);
  const [accessories, setAccessories] = createSignal<unknown[]>(cached?.accessories ?? []);
  const [parts, setParts] = createSignal<unknown[]>(cached?.parts ?? []);
  const [editorOpen, setEditorOpen] = createSignal(false);
  const [status, setStatus] = createSignal<Status>("idle");
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [section, setSection] = createSignal<Section>("default");
  const [open, setOpen] = createSignal(false);
  const [changing, setChanging] = createSignal(false);
  const worn = createMemo(() => wornLookOf(settings()));
  // The look she wears overrides the outfit, hairstyle and colour it sets; the rest follows the wardrobe.
  const current = createMemo(() => {
    const id = lookOutfit(worn(), outfits());
    return (id ? outfits().find((o) => o.id === id) : undefined) ?? resolveOutfit(settings(), section(), outfits());
  });
  const hair = createMemo(() => lookHair(worn(), hairstyles()) ?? resolveHair(settings(), section(), hairstyles()));
  const hairColor = createMemo(() => lookHairColor(worn()) ?? resolveHairColor(settings(), colors()));
  return {
    cachedAtStart: !!cached,
    settings,
    setSettings,
    outfits,
    setOutfits,
    hairstyles,
    setHairstyles,
    colors,
    setColors,
    accessories,
    setAccessories,
    parts,
    setParts,
    editorOpen,
    setEditorOpen,
    worn,
    hair,
    hairColor,
    status,
    setStatus,
    saving,
    setSaving,
    error,
    setError,
    section,
    setSection,
    open,
    setOpen,
    changing,
    setChanging,
    current,
  };
});

export const avatarSettings = store.settings;
export const wardrobeOutfits = store.outfits;
export const wardrobeHairstyles = store.hairstyles;
export const wardrobeColors = store.colors;
export const avatarStatus = store.status;
export const avatarSaving = store.saving;
export const avatarError = store.error;
export const avatarSection = store.section;
export const wardrobeOpen = store.open;
/** The stage is loading or playing an outfit change. */
export const outfitChanging = store.changing;
export const setOutfitChanging = store.setChanging;
/** The outfit she should wear right now (null: no wardrobe installed). */
export const currentOutfit = store.current;
/** The hairstyle she should wear now (`natural`: her own hair). */
export const currentHair = store.hair;
/** The hair colour id she should wear now (null: natural). */
export const currentHairColor = store.hairColor;
/** The look she wears now (null: the model as authored). */
export const currentLook = store.worn;
export const wardrobeAccessories = store.accessories;
export const wardrobeParts = store.parts;
/** The "Edit Lucina" screen is open. */
export const editorOpen = store.editorOpen;

export function openEditor(): void {
  store.setOpen(false);
  store.setEditorOpen(true);
  void refreshAvatar();
}

export function closeEditor(): void {
  store.setEditorOpen(false);
}

/** Saves a look (a new one without `id`) and wears it. Resolves to its id, or null on failure. */
export async function saveLook(name: string, look: Record<string, unknown>, id?: string): Promise<string | null> {
  store.setError(null);
  store.setSaving(true);
  try {
    const saved = await avatarApi.put({ saveLook: id ? { id, name, look } : { name, look } });
    const added = id ?? saved.looks.find((l) => !store.settings().looks.some((o) => o.id === l.id))?.id ?? saved.looks.at(-1)?.id ?? null;
    const worn = added ? await avatarApi.put({ current: added }) : saved;
    store.setSettings(worn);
    persist();
    return added;
  } catch (e) {
    const err = toApiError(e);
    store.setError(err.isNetwork ? t("wardrobe.errOffline") : t("wardrobe.errSave"));
    return null;
  } finally {
    store.setSaving(false);
  }
}

/** Wear a saved look (null: none, back to the model as authored). */
export function wearLook(id: string | null): Promise<boolean> {
  return updateAvatar({ current: id });
}

export function deleteLook(id: string): Promise<boolean> {
  return updateAvatar({ deleteLook: id });
}

function persist(): void {
  writeJSON("local", CACHE_KEY, {
    settings: store.settings(),
    outfits: store.outfits(),
    hairstyles: store.hairstyles(),
    colors: store.colors(),
    accessories: store.accessories(),
    parts: store.parts(),
  });
}

export function setAvatarPath(pathname: string): void {
  store.setSection(sectionFor(pathname));
}

export function openWardrobe(): void {
  store.setError(null);
  store.setOpen(true);
  void refreshAvatar();
}

export function closeWardrobe(): void {
  store.setOpen(false);
}

let inflight: Promise<void> | null = null;
let lastLoad = 0;

/** Fetch the settings and the wardrobe. Failures keep what we had. */
export function loadAvatar(): Promise<void> {
  if (inflight) return inflight;
  store.setStatus(store.status() === "ready" ? "ready" : "loading");
  inflight = (async () => {
    const [s, w] = await Promise.allSettled([avatarApi.get(), avatarApi.wardrobe()]);
    // A write that started meanwhile wins over this read.
    if (s.status === "fulfilled" && !store.saving()) store.setSettings(s.value);
    if (w.status === "fulfilled") {
      store.setOutfits(w.value.outfits);
      store.setHairstyles(w.value.hairstyles);
      store.setColors(w.value.colors);
      store.setAccessories(w.value.accessories);
      store.setParts(w.value.parts);
    }
    lastLoad = Date.now();
    if (s.status === "fulfilled" || w.status === "fulfilled") {
      store.setStatus("ready");
      persist();
    } else if (store.status() !== "ready") store.setStatus("error");
  })().finally(() => {
    inflight = null;
  });
  return inflight;
}

/** Reload unless it was done very recently. */
export function refreshAvatar(force = false): Promise<void> {
  if (!force && inflight) return inflight;
  if (!force && Date.now() - lastLoad < REFRESH_MIN_MS && store.status() === "ready") return Promise.resolve();
  return loadAvatar();
}

/** Resolves when the first load ends, or after `timeoutMs` (the stage should not wait forever). */
export function avatarReady(timeoutMs: number): Promise<void> {
  if (store.cachedAtStart || store.status() === "ready" || store.status() === "error") return Promise.resolve();
  const load = inflight ?? loadAvatar();
  return Promise.race([load, new Promise<void>((r) => setTimeout(r, timeoutMs))]);
}

let writeSeq = 0;

/** Optimistic: the stage changes at once; a refused write puts it back. */
export async function updateAvatar(patch: AvatarPatch): Promise<boolean> {
  const before = store.settings();
  const seq = ++writeSeq;
  store.setSettings(applyPatch(before, patch));
  store.setError(null);
  store.setSaving(true);
  try {
    const saved = await avatarApi.put(patch);
    if (seq === writeSeq) store.setSettings(saved);
    persist();
    return true;
  } catch (e) {
    if (seq === writeSeq) {
      store.setSettings(before);
      const err = toApiError(e);
      store.setError(err.code.startsWith("unknown_") ? t("wardrobe.errUnknown") : err.isNetwork ? t("wardrobe.errOffline") : t("wardrobe.errSave"));
    }
    return false;
  } finally {
    if (seq === writeSeq) store.setSaving(false);
  }
}

/** Put on `id` and keep it (a pin). */
export function wearOutfit(id: string): Promise<boolean> {
  return updateAvatar({ outfit: id });
}

/** Style her hair `id` and keep it (a pin; `natural` is her own hair). */
export function wearHair(id: string): Promise<boolean> {
  return updateAvatar({ hair: id });
}

/** Tint her hair (null: natural colour). */
export function setHairColor(id: string | null): Promise<boolean> {
  return updateAvatar({ hairColor: id });
}

/** Lucina changed outfit (timeline item): show it now, then read the truth. */
export function handleTimelineItem(item: TimelineItem, now = Date.now()): boolean {
  // Lucina put on a saved look ("mettiti il look serata"): read the truth.
  if (item.meta?.kind === "avatar_look" && item.role === "lucina") {
    if (now - item.ts > LIVE_ITEM_MS) return false;
    void refreshAvatar(true);
    return true;
  }
  if (item.meta?.kind !== "avatar_outfit" || item.role !== "lucina") return false;
  if (now - item.ts > LIVE_ITEM_MS) return false;
  const { outfit, hair } = item.meta;
  if (!store.saving()) {
    const patch: AvatarPatch = {};
    if (isOutfitId(outfit)) patch.outfit = outfit;
    if (isOutfitId(hair)) patch.hair = hair;
    // Neither: back to automatic.
    store.setSettings(applyPatch(store.settings(), Object.keys(patch).length ? patch : { outfit: null, hair: null, auto: true }));
  }
  void refreshAvatar(true);
  return true;
}

/**
 * Keep the wardrobe in sync while the shell is shown: load now, follow
 * Lucina's changes from the timeline stream, refresh when the tab comes back.
 */
export function startAvatarSync(): () => void {
  void loadAvatar();
  const off = onTimelineItem((item) => void handleTimelineItem(item));
  const onVisible = () => {
    if (document.visibilityState === "visible") void refreshAvatar();
  };
  document.addEventListener("visibilitychange", onVisible);
  return () => {
    off();
    document.removeEventListener("visibilitychange", onVisible);
  };
}

/** Test helper: back to the start state (re-reading the cache). */
export function resetWardrobeState(): void {
  const cached = readCache();
  store.setSettings(cached?.settings ?? DEFAULT_SETTINGS);
  store.setOutfits(cached?.outfits ?? []);
  store.setHairstyles(cached?.hairstyles ?? []);
  store.setColors(cached?.colors ?? []);
  store.setAccessories(cached?.accessories ?? []);
  store.setParts(cached?.parts ?? []);
  store.setEditorOpen(false);
  store.setStatus("idle");
  store.setSaving(false);
  store.setError(null);
  store.setSection("default");
  store.setOpen(false);
  store.setChanging(false);
  inflight = null;
  lastLoad = 0;
  writeSeq++;
}
