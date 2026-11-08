import { createEffect, createMemo, createSignal, For, Match, on, onCleanup, Show, Switch, type JSX } from "solid-js";
import { locale, t, type MessageKey } from "../i18n";
import {
  categoriesPresent,
  categoryOf,
  CATEGORY_LABELS,
  hairChoices,
  NATURAL_HAIR,
  outfitName,
  thumbnailUrl,
  type Category,
  type Hairstyle,
  type Outfit,
  type Section,
} from "../lib/wardrobe";
import {
  avatarError,
  avatarSaving,
  avatarSection,
  avatarSettings,
  avatarStatus,
  closeWardrobe,
  openEditor,
  currentHair,
  currentHairColor,
  currentOutfit,
  outfitChanging,
  setHairColor,
  updateAvatar,
  wardrobeColors,
  wardrobeHairstyles,
  wardrobeOpen,
  wardrobeOutfits,
  wearHair,
  wearOutfit,
} from "../state/wardrobe";
import {
  BowIcon,
  CheckIcon,
  ChefHatIcon,
  CloseIcon,
  DressIcon,
  DropIcon,
  DumbbellIcon,
  EditIcon,
  GridIcon,
  HairIcon,
  HangerIcon,
  ListIcon,
  PinIcon,
  ShirtIcon,
  SparkleIcon,
} from "./Icons";

// The Wardrobe, after Custom Cast's costume screen (SceneEdit): Lucina stays
// on stage as the live preview while a panel shows round tabs (Outfit, Hair,
// Colour) over a grid of tiles with a name ribbon; the one she wears is
// framed. Tap a tile and she changes on the spot. Our own Nebula/Pearl look;
// no assets from that app.

type Kind = "outfit" | "hair" | "color";

const KINDS: { id: Kind; label: MessageKey; icon: () => JSX.Element }[] = [
  { id: "outfit", label: "wardrobe.kindOutfit", icon: HangerIcon },
  { id: "hair", label: "wardrobe.kindHair", icon: HairIcon },
  { id: "color", label: "wardrobe.kindColor", icon: DropIcon },
];

const CATEGORY_ICONS: Record<Category | "all", () => JSX.Element> = {
  all: GridIcon,
  uniform: DressIcon,
  casual: ShirtIcon,
  sport: DumbbellIcon,
  work: ChefHatIcon,
  formal: BowIcon,
  fantasy: SparkleIcon,
  other: HangerIcon,
};

/** Glyphs for well-known outfit ids (tiles without a thumbnail); others use their category. */
const ID_ICONS: Record<string, () => JSX.Element> = {
  uniform: DressIcon,
  gym: DumbbellIcon,
  chef: ChefHatIcon,
  maid: BowIcon,
  casual: ShirtIcon,
};

const SECTIONS: { id: Section; label: MessageKey; icon: () => JSX.Element }[] = [
  { id: "gym", label: "wardrobe.ruleGym", icon: DumbbellIcon },
  { id: "shopping", label: "wardrobe.ruleShopping", icon: ListIcon },
  { id: "default", label: "wardrobe.ruleDefault", icon: SparkleIcon },
];

/** The tile picture: the thumbnail, or a glyph on a holo gradient when there is none (or it fails). */
function Art(props: { item: { id: string; thumbnail?: string }; cat: string; glyph: () => JSX.Element; size?: "sm" }) {
  const [broken, setBroken] = createSignal(false);
  const src = () => thumbnailUrl(props.item);
  return (
    <span class="wtile-art" classList={{ "wtile-art--sm": props.size === "sm" }} data-cat={props.cat}>
      <Show when={src() && !broken()} fallback={<span class="wtile-glyph">{props.glyph()}</span>}>
        <img src={src()!} alt="" loading="lazy" decoding="async" draggable={false} onError={() => setBroken(true)} />
      </Show>
    </span>
  );
}

function OutfitArt(props: { outfit: Outfit; size?: "sm" }) {
  return (
    <Art
      item={props.outfit}
      cat={categoryOf(props.outfit)}
      glyph={() => (ID_ICONS[props.outfit.id] ?? CATEGORY_ICONS[categoryOf(props.outfit)])()}
      size={props.size}
    />
  );
}

function HairArt(props: { hair: Hairstyle; size?: "sm" }) {
  return <Art item={props.hair} cat={props.hair.id === NATURAL_HAIR ? "natural" : "hair"} glyph={HairIcon} size={props.size} />;
}

/**
 * The COM3D2 fitting room (beta, crates/secretary-avatar-com3d2 on nezopt behind the p1 gateway):
 * Lucina on the COM3D2 body with part-by-part outfits, body sliders and Rust physics. A per-device
 * switch, off by default; her look in the app does not change.
 */
export const COM3D2_URL = "/v1/com3d2/";
export const COM3D2_KEY = "secretary.wardrobe.com3d2";

function readCom3d2(): boolean {
  try {
    return localStorage.getItem(COM3D2_KEY) === "1";
  } catch {
    return false;
  }
}

export default function Wardrobe() {
  const [kind, setKind] = createSignal<Kind>("outfit");
  const [com3d2, setCom3d2State] = createSignal(readCom3d2());
  const setCom3d2 = (on: boolean) => {
    setCom3d2State(on);
    try {
      if (on) localStorage.setItem(COM3D2_KEY, "1");
      else localStorage.removeItem(COM3D2_KEY);
    } catch {
      // Storage blocked: the switch still works until the page reloads.
    }
  };
  const [cat, setCat] = createSignal<Category | "all">("all");
  let panel: HTMLElement | undefined;
  let closeBtn: HTMLButtonElement | undefined;
  let opener: HTMLElement | null = null;

  const outfits = wardrobeOutfits;
  const hairs = createMemo(() => hairChoices(wardrobeHairstyles()));
  const cats = createMemo(() => categoriesPresent(outfits()));
  const shown = createMemo(() => (cat() === "all" ? outfits() : outfits().filter((o) => categoryOf(o) === cat())));
  const nameOf = (o: { id: string; name?: Record<string, string> | string }) =>
    o.id === NATURAL_HAIR && !o.name ? t("wardrobe.hairNatural") : outfitName(o, locale());
  const settings = avatarSettings;
  const pinned = () => settings().outfit !== null || settings().hair !== null;
  const onNow = () => currentOutfit();
  const hairNow = () => hairs().find((h) => h.id === currentHair()) ?? hairs()[0]!;
  const colorNow = () => wardrobeColors().find((c) => c.id === currentHairColor()) ?? null;
  /** Sections whose rule names this outfit / hairstyle (small badges on the tile). */
  const outfitRules = (id: string) => SECTIONS.filter((s) => settings().rules[s.id].outfit === id);
  const hairRules = (id: string) => SECTIONS.filter((s) => settings().rules[s.id].hair === id);

  createEffect(
    on(wardrobeOpen, (open) => {
      if (open) {
        opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        queueMicrotask(() => closeBtn?.focus());
      } else if (opener) {
        opener.focus?.();
        opener = null;
      }
    }),
  );
  // A category that no longer has outfits falls back to "all".
  createEffect(() => {
    if (!cats().includes(cat())) setCat("all");
  });

  // Category chips scroll sideways on a phone: fade the edge that has more chips behind it.
  const [chipsMore, setChipsMore] = createSignal({ start: false, end: false });
  const measureChips = (el: HTMLElement) => {
    const max = el.scrollWidth - el.clientWidth;
    setChipsMore({ start: el.scrollLeft > 2, end: max > 2 && el.scrollLeft < max - 2 });
  };
  const trackChips = (el: HTMLDivElement) => {
    queueMicrotask(() => measureChips(el));
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => measureChips(el));
    ro.observe(el);
    onCleanup(() => ro.disconnect());
  };

  const onDocKey = (e: KeyboardEvent) => wardrobeOpen() && e.key === "Escape" && closeWardrobe();
  document.addEventListener("keydown", onDocKey);
  onCleanup(() => document.removeEventListener("keydown", onDocKey));

  /** Arrow keys move between the Outfit / Hair / Colour tabs (WAI-ARIA tabs pattern). */
  function tabKeys(e: KeyboardEvent) {
    const list = KINDS.map((k) => k.id);
    const i = list.indexOf(kind());
    const next = e.key === "ArrowRight" ? i + 1 : e.key === "ArrowLeft" ? i - 1 : e.key === "Home" ? 0 : e.key === "End" ? list.length - 1 : null;
    if (next === null) return;
    e.preventDefault();
    const k = list[(next + list.length) % list.length]!;
    setKind(k);
    panel?.querySelector<HTMLElement>(`[data-kind-tab="${k}"]`)?.focus();
  }

  const badges = (list: typeof SECTIONS) => (
    <span class="wtile-badges" aria-hidden="true">
      <For each={list}>{(s) => <span class="wtile-badge" title={t(s.label)}>{s.icon()}</span>}</For>
    </span>
  );

  return (
    <Show when={wardrobeOpen()}>
      <div class="wardrobe">
        <div class="wardrobe-scrim" onClick={closeWardrobe} aria-hidden="true" />
        <section
          ref={panel}
          class="wardrobe-panel"
          classList={{ "wardrobe-panel--wide": com3d2() }}
          role="dialog"
          aria-modal="true"
          aria-labelledby="wardrobe-title"
        >
          <span class="wardrobe-grab" aria-hidden="true" />
          <header class="wardrobe-head">
            <div>
              <p class="wardrobe-eyebrow">{t("wardrobe.eyebrow")}</p>
              <h2 id="wardrobe-title" class="wardrobe-title">
                {t("wardrobe.title")}
              </h2>
            </div>
            <button type="button" class="wchip wchip--on wardrobe-edit" onClick={openEditor}>
              <EditIcon /> {t("avatarEditor.open")}
            </button>
            <button ref={closeBtn} type="button" class="icon-btn wardrobe-close" onClick={closeWardrobe} aria-label={t("wardrobe.close")}>
              <CloseIcon />
            </button>
          </header>

          {/* The frame owns the radius, hairline and clip; only this body scrolls (page system, Sheet). */}
          <div class="wardrobe-body">
            {/* What she wears now. */}
            <div class="wardrobe-now" classList={{ "wardrobe-now--changing": outfitChanging() }}>
              <Show when={onNow()} fallback={<span class="wtile-art wtile-art--sm"><span class="wtile-glyph"><HangerIcon /></span></span>}>
                {(o) => <OutfitArt outfit={o()} size="sm" />}
              </Show>
              <div class="wardrobe-now-text" role="status" aria-live="polite">
                <span class="wardrobe-now-label">{outfitChanging() ? t("wardrobe.changing") : t("wardrobe.wearing")}</span>
                <span class="wardrobe-now-name">
                  {onNow() ? nameOf(onNow()!) : "—"}
                  <span class="wardrobe-now-hair">
                    {" · "}
                    {nameOf(hairNow())}
                    <Show when={colorNow()}>
                      {(c) => <span class="wswatch-dot" style={{ "--swatch": c().hex }} title={nameOf(c())} />}
                    </Show>
                  </span>
                </span>
              </div>
              <Show when={pinned()}>
                <button
                  type="button"
                  class="wardrobe-pin"
                  onClick={() => void updateAvatar({ outfit: null, hair: null })}
                  disabled={avatarSaving()}
                  title={t("wardrobe.unpinTitle")}
                >
                  <PinIcon />
                  <span>{t("wardrobe.pinned")}</span>
                  <span class="wardrobe-pin-x" aria-hidden="true">
                    ×
                  </span>
                  <span class="sr-only">{t("wardrobe.unpin")}</span>
                </button>
              </Show>
            </div>

            {/* Auto by section: a "switch" row, as in Custom Cast's settings. */}
            <div class="wardrobe-switch">
              <div class="wardrobe-switch-text">
                <span class="wardrobe-switch-label" id="wardrobe-auto-label">
                  {t("wardrobe.auto")}
                </span>
                <span class="muted small" id="wardrobe-auto-hint">
                  {pinned() ? t("wardrobe.pinnedHint") : settings().auto ? t("wardrobe.autoOn") : t("wardrobe.autoOff")}
                </span>
              </div>
              <button
                type="button"
                role="switch"
                class="wswitch"
                classList={{ "wswitch--on": settings().auto }}
                aria-checked={settings().auto ? "true" : "false"}
                aria-labelledby="wardrobe-auto-label"
                aria-describedby="wardrobe-auto-hint"
                onClick={() => void updateAvatar({ auto: !settings().auto })}
              >
                <span class="wswitch-knob" />
              </button>
            </div>

            {/* The COM3D2 body (beta): the fitting room replaces the tabs below while it is on. */}
            <div class="wardrobe-switch">
              <div class="wardrobe-switch-text">
                <span class="wardrobe-switch-label" id="wardrobe-com3d2-label">
                  {t("wardrobe.com3d2")}
                </span>
                <span class="muted small" id="wardrobe-com3d2-hint">
                  {com3d2() ? t("wardrobe.com3d2On") : t("wardrobe.com3d2Off")}
                </span>
              </div>
              <button
                type="button"
                role="switch"
                class="wswitch"
                classList={{ "wswitch--on": com3d2() }}
                aria-checked={com3d2() ? "true" : "false"}
                aria-labelledby="wardrobe-com3d2-label"
                aria-describedby="wardrobe-com3d2-hint"
                onClick={() => setCom3d2(!com3d2())}
              >
                <span class="wswitch-knob" />
              </button>
            </div>

            <Show
              when={!com3d2()}
              fallback={<iframe class="wardrobe-com3d2" src={COM3D2_URL} title={t("wardrobe.com3d2Frame")} />}
            >
            {/* Round tabs: an icon over a short label (Outfit, Hair, Colour). */}
            <div class="wardrobe-cats" role="tablist" aria-label={t("wardrobe.title")} onKeyDown={tabKeys}>
              <For each={KINDS}>
                {(k) => (
                  <button
                    type="button"
                    role="tab"
                    id={`wardrobe-tab-${k.id}`}
                    class="wcat"
                    classList={{ "wcat--on": kind() === k.id }}
                    data-kind-tab={k.id}
                    aria-selected={kind() === k.id ? "true" : "false"}
                    aria-controls="wardrobe-grid"
                    tabindex={kind() === k.id ? 0 : -1}
                    onClick={() => setKind(k.id)}
                  >
                    <span class="wcat-ring">{k.icon()}</span>
                    <span class="wcat-label">{t(k.label)}</span>
                  </button>
                )}
              </For>
            </div>

            <div id="wardrobe-grid" role="tabpanel" aria-labelledby={`wardrobe-tab-${kind()}`} class="wardrobe-pane">
              <Switch>
                <Match when={kind() === "outfit"}>
                  <Show
                    when={outfits().length}
                    fallback={
                      <div class="wardrobe-empty" role="status">
                        <Show
                          when={avatarStatus() === "loading"}
                          fallback={
                            <>
                              <span class="wardrobe-empty-icon" aria-hidden="true">
                                <HangerIcon />
                              </span>
                              <p class="empty-title">{t("wardrobe.empty")}</p>
                              <p class="muted small">{t("wardrobe.emptySub")}</p>
                            </>
                          }
                        >
                          <span class="holo-loader" aria-hidden="true" />
                          <p class="muted small">{t("wardrobe.loading")}</p>
                        </Show>
                      </div>
                    }
                  >
                    <Show when={cats().length > 2}>
                      <div
                        ref={trackChips}
                        class="wchips"
                        classList={{ "wchips--more-start": chipsMore().start, "wchips--more-end": chipsMore().end }}
                        role="radiogroup"
                        aria-label={t("wardrobe.categories")}
                        onScroll={(e) => measureChips(e.currentTarget)}
                      >
                        <For each={cats()}>
                          {(c) => (
                            <button
                              type="button"
                              role="radio"
                              class="wchip"
                              classList={{ "wchip--on": cat() === c }}
                              aria-checked={cat() === c ? "true" : "false"}
                              onClick={() => setCat(c)}
                            >
                              {CATEGORY_ICONS[c]()}
                              <span>{t(CATEGORY_LABELS[c])}</span>
                            </button>
                          )}
                        </For>
                      </div>
                    </Show>
                    <div class="wardrobe-grid" aria-label={t("wardrobe.outfits")} role="group">
                      <For each={shown()}>
                        {(o, i) => {
                          const on = () => onNow()?.id === o.id;
                          return (
                            <button
                              type="button"
                              class="wtile"
                              classList={{ "wtile--on": on(), "wtile--pinned": settings().outfit === o.id }}
                              style={{ "--i": String(i()) }}
                              aria-pressed={on() ? "true" : "false"}
                              aria-label={t("wardrobe.wear", { name: nameOf(o) })}
                              title={nameOf(o)}
                              onClick={() => void wearOutfit(o.id)}
                            >
                              <OutfitArt outfit={o} />
                              <span class="wtile-ribbon">{nameOf(o)}</span>
                              {badges(outfitRules(o.id))}
                              <Show when={on()}>
                                <span class="wtile-check" aria-hidden="true">
                                  <CheckIcon />
                                </span>
                              </Show>
                              <Show when={settings().outfit === o.id}>
                                <span class="wtile-pin" aria-hidden="true">
                                  <PinIcon />
                                </span>
                              </Show>
                            </button>
                          );
                        }}
                      </For>
                    </div>
                  </Show>
                </Match>

                <Match when={kind() === "hair"}>
                  <div class="wardrobe-grid" aria-label={t("wardrobe.hairstyles")} role="group">
                    <For each={hairs()}>
                      {(h, i) => {
                        const on = () => currentHair() === h.id;
                        return (
                          <button
                            type="button"
                            class="wtile"
                            classList={{ "wtile--on": on(), "wtile--pinned": settings().hair === h.id }}
                            style={{ "--i": String(i()) }}
                            aria-pressed={on() ? "true" : "false"}
                            aria-label={t("wardrobe.wearHair", { name: nameOf(h) })}
                            title={nameOf(h)}
                            onClick={() => void wearHair(h.id)}
                          >
                            <HairArt hair={h} />
                            <span class="wtile-ribbon">{nameOf(h)}</span>
                            {badges(hairRules(h.id))}
                            <Show when={on()}>
                              <span class="wtile-check" aria-hidden="true">
                                <CheckIcon />
                              </span>
                            </Show>
                            <Show when={settings().hair === h.id}>
                              <span class="wtile-pin" aria-hidden="true">
                                <PinIcon />
                              </span>
                            </Show>
                          </button>
                        );
                      }}
                    </For>
                  </div>
                  <Show when={hairs().length === 1}>
                    <p class="muted small wardrobe-hint">{t("wardrobe.hairEmpty")}</p>
                  </Show>
                </Match>

                <Match when={kind() === "color"}>
                  <div class="wswatches" role="radiogroup" aria-label={t("wardrobe.kindColor")}>
                    <button
                      type="button"
                      role="radio"
                      class="wswatch wswatch--natural"
                      classList={{ "wswatch--on": currentHairColor() === null }}
                      aria-checked={currentHairColor() === null ? "true" : "false"}
                      onClick={() => void setHairColor(null)}
                    >
                      <span class="wswatch-chip" aria-hidden="true" />
                      <span class="wswatch-name">{t("wardrobe.colorNatural")}</span>
                    </button>
                    <For each={wardrobeColors()}>
                      {(c) => (
                        <button
                          type="button"
                          role="radio"
                          class="wswatch"
                          classList={{ "wswatch--on": currentHairColor() === c.id }}
                          aria-checked={currentHairColor() === c.id ? "true" : "false"}
                          style={{ "--swatch": c.hex }}
                          onClick={() => void setHairColor(c.id)}
                        >
                          <span class="wswatch-chip" aria-hidden="true" />
                          <span class="wswatch-name">{nameOf(c)}</span>
                        </button>
                      )}
                    </For>
                  </div>
                  <Show when={!wardrobeColors().length}>
                    <p class="muted small wardrobe-hint">{t("wardrobe.colorEmpty")}</p>
                  </Show>
                </Match>
              </Switch>
            </div>

            {/* Per-section rules: an outfit row and a hair row per section. */}
            <Show when={outfits().length}>
              <section class="wardrobe-rules" aria-labelledby="wardrobe-rules-title">
                <h3 id="wardrobe-rules-title" class="wardrobe-subtitle">
                  {t("wardrobe.rules")}
                </h3>
                <For each={SECTIONS}>
                  {(s) => (
                    <div class="wrule" classList={{ "wrule--here": avatarSection() === s.id, "wrule--off": !settings().auto && s.id !== "default" }}>
                      <span class="wrule-label" id={`wrule-${s.id}`}>
                        <span class="wrule-icon" aria-hidden="true">
                          {s.icon()}
                        </span>
                        {t(s.label)}
                      </span>
                      <div class="wrule-picks" role="radiogroup" aria-labelledby={`wrule-${s.id}`}>
                        <For each={outfits()}>
                          {(o) => {
                            const sel = () => settings().rules[s.id].outfit === o.id;
                            return (
                              <button
                                type="button"
                                role="radio"
                                class="wpick"
                                classList={{ "wpick--on": sel() }}
                                aria-checked={sel() ? "true" : "false"}
                                aria-label={t("wardrobe.ruleFor", { section: t(s.label), name: nameOf(o) })}
                                title={nameOf(o)}
                                onClick={() => void updateAvatar({ rules: { [s.id]: { outfit: o.id } } })}
                              >
                                <OutfitArt outfit={o} size="sm" />
                              </button>
                            );
                          }}
                        </For>
                      </div>
                      <Show when={hairs().length > 1}>
                        <div class="wrule-picks wrule-picks--hair" role="radiogroup" aria-label={t("wardrobe.ruleHairGroup", { section: t(s.label) })}>
                          <For each={hairs()}>
                            {(h) => {
                              const sel = () => settings().rules[s.id].hair === h.id;
                              return (
                                <button
                                  type="button"
                                  role="radio"
                                  class="wpick wpick--hair"
                                  classList={{ "wpick--on": sel() }}
                                  aria-checked={sel() ? "true" : "false"}
                                  aria-label={t("wardrobe.ruleHairFor", { section: t(s.label), name: nameOf(h) })}
                                  title={nameOf(h)}
                                  onClick={() => void updateAvatar({ rules: { [s.id]: { hair: h.id } } })}
                                >
                                  <HairArt hair={h} size="sm" />
                                </button>
                              );
                            }}
                          </For>
                        </div>
                      </Show>
                    </div>
                  )}
                </For>
              </section>
            </Show>
            </Show>

            <Show when={avatarError()}>
              {(msg) => (
                <p class="form-error" role="alert">
                  {msg()}
                </p>
              )}
            </Show>
            <p class="wardrobe-hint muted small">{t("wardrobe.askHint")}</p>
          </div>
        </section>
      </div>
    </Show>
  );
}
