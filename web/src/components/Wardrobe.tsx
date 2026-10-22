import { createEffect, createMemo, createSignal, For, on, onCleanup, Show, type JSX } from "solid-js";
import { locale, t, type MessageKey } from "../i18n";
import {
  categoriesPresent,
  categoryOf,
  CATEGORY_LABELS,
  outfitName,
  thumbnailUrl,
  type Category,
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
  currentOutfit,
  outfitChanging,
  updateAvatar,
  wardrobeOpen,
  wardrobeOutfits,
  wearOutfit,
} from "../state/wardrobe";
import {
  BowIcon,
  CheckIcon,
  ChefHatIcon,
  CloseIcon,
  DressIcon,
  DumbbellIcon,
  GridIcon,
  HangerIcon,
  ListIcon,
  PinIcon,
  ShirtIcon,
  SparkleIcon,
} from "./Icons";

// The Wardrobe, after Custom Cast's costume screen (SceneEdit): Lucina stays
// on stage as the live preview while a panel shows round category tabs over a
// grid of outfit tiles with a name ribbon; the one she wears is framed. Tap a
// tile and she changes on the spot. Our own Nebula/Pearl look; no assets from
// that app.

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
function OutfitArt(props: { outfit: Outfit; size?: "sm" }) {
  const [broken, setBroken] = createSignal(false);
  const src = () => thumbnailUrl(props.outfit);
  const Glyph = () => (ID_ICONS[props.outfit.id] ?? CATEGORY_ICONS[categoryOf(props.outfit)])();
  return (
    <span class="wtile-art" classList={{ "wtile-art--sm": props.size === "sm" }} data-cat={categoryOf(props.outfit)}>
      <Show when={src() && !broken()} fallback={<span class="wtile-glyph">{Glyph()}</span>}>
        <img src={src()!} alt="" loading="lazy" decoding="async" draggable={false} onError={() => setBroken(true)} />
      </Show>
    </span>
  );
}

export default function Wardrobe() {
  const [cat, setCat] = createSignal<Category | "all">("all");
  let panel: HTMLElement | undefined;
  let closeBtn: HTMLButtonElement | undefined;
  let opener: HTMLElement | null = null;

  const outfits = wardrobeOutfits;
  const cats = createMemo(() => categoriesPresent(outfits()));
  const shown = createMemo(() => (cat() === "all" ? outfits() : outfits().filter((o) => categoryOf(o) === cat())));
  const nameOf = (o: Outfit) => outfitName(o, locale());
  const byId = (id: string | null) => (id ? outfits().find((o) => o.id === id) : undefined);
  const pinned = () => avatarSettings().outfit;
  const onNow = () => currentOutfit();
  /** Sections whose rule names `id` (shown as small badges on the tile). */
  const rulesFor = (id: string) => SECTIONS.filter((s) => avatarSettings().rules[s.id] === id);

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

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      closeWardrobe();
    }
  }
  const onDocKey = (e: KeyboardEvent) => wardrobeOpen() && e.key === "Escape" && closeWardrobe();
  document.addEventListener("keydown", onDocKey);
  onCleanup(() => document.removeEventListener("keydown", onDocKey));

  /** Arrow keys move between the category tabs (WAI-ARIA tabs pattern). */
  function tabKeys(e: KeyboardEvent) {
    const list = cats();
    const i = list.indexOf(cat());
    const next = e.key === "ArrowRight" ? i + 1 : e.key === "ArrowLeft" ? i - 1 : e.key === "Home" ? 0 : e.key === "End" ? list.length - 1 : null;
    if (next === null) return;
    e.preventDefault();
    const c = list[(next + list.length) % list.length]!;
    setCat(c);
    panel?.querySelector<HTMLElement>(`[data-cat-tab="${c}"]`)?.focus();
  }

  return (
    <Show when={wardrobeOpen()}>
      <div class="wardrobe" onKeyDown={onKey}>
        <div class="wardrobe-scrim" onClick={closeWardrobe} aria-hidden="true" />
        <section ref={panel} class="wardrobe-panel" role="dialog" aria-modal="true" aria-labelledby="wardrobe-title">
          <span class="wardrobe-grab" aria-hidden="true" />
          <header class="wardrobe-head">
            <div>
              <p class="wardrobe-eyebrow">{t("wardrobe.eyebrow")}</p>
              <h2 id="wardrobe-title" class="wardrobe-title">
                {t("wardrobe.title")}
              </h2>
            </div>
            <button ref={closeBtn} type="button" class="icon-btn wardrobe-close" onClick={closeWardrobe} aria-label={t("wardrobe.close")}>
              <CloseIcon />
            </button>
          </header>

          {/* What she wears now. */}
          <div class="wardrobe-now" classList={{ "wardrobe-now--changing": outfitChanging() }}>
            <Show when={onNow()} fallback={<span class="wtile-art wtile-art--sm"><span class="wtile-glyph"><HangerIcon /></span></span>}>
              {(o) => <OutfitArt outfit={o()} size="sm" />}
            </Show>
            <div class="wardrobe-now-text" role="status" aria-live="polite">
              <span class="wardrobe-now-label">{outfitChanging() ? t("wardrobe.changing") : t("wardrobe.wearing")}</span>
              <span class="wardrobe-now-name">{onNow() ? nameOf(onNow()!) : "—"}</span>
            </div>
            <Show when={byId(pinned())}>
              <button
                type="button"
                class="wardrobe-pin"
                onClick={() => void updateAvatar({ outfit: null })}
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
                {pinned() ? t("wardrobe.pinnedHint") : avatarSettings().auto ? t("wardrobe.autoOn") : t("wardrobe.autoOff")}
              </span>
            </div>
            <button
              type="button"
              role="switch"
              class="wswitch"
              classList={{ "wswitch--on": avatarSettings().auto }}
              aria-checked={avatarSettings().auto ? "true" : "false"}
              aria-labelledby="wardrobe-auto-label"
              aria-describedby="wardrobe-auto-hint"
              onClick={() => void updateAvatar({ auto: !avatarSettings().auto })}
            >
              <span class="wswitch-knob" />
            </button>
          </div>

          <Show
            when={outfits().length}
            fallback={
              <div class="wardrobe-empty" role="status">
                <Show when={avatarStatus() === "loading"} fallback={
                  <>
                    <span class="wardrobe-empty-icon" aria-hidden="true"><HangerIcon /></span>
                    <p class="empty-title">{t("wardrobe.empty")}</p>
                    <p class="muted small">{t("wardrobe.emptySub")}</p>
                  </>
                }>
                  <span class="holo-loader" aria-hidden="true" />
                  <p class="muted small">{t("wardrobe.loading")}</p>
                </Show>
              </div>
            }
          >
            {/* Round category tabs: an icon over a short label. */}
            <div class="wardrobe-cats" role="tablist" aria-label={t("wardrobe.categories")} onKeyDown={tabKeys}>
              <For each={cats()}>
                {(c) => (
                  <button
                    type="button"
                    role="tab"
                    class="wcat"
                    classList={{ "wcat--on": cat() === c }}
                    data-cat-tab={c}
                    aria-selected={cat() === c ? "true" : "false"}
                    aria-controls="wardrobe-grid"
                    tabindex={cat() === c ? 0 : -1}
                    onClick={() => setCat(c)}
                  >
                    <span class="wcat-ring">{CATEGORY_ICONS[c]()}</span>
                    <span class="wcat-label">{t(CATEGORY_LABELS[c])}</span>
                  </button>
                )}
              </For>
            </div>

            {/* The tiles. */}
            <div id="wardrobe-grid" class="wardrobe-grid" role="tabpanel" aria-label={t("wardrobe.outfits")}>
              <For each={shown()}>
                {(o, i) => {
                  const on = () => onNow()?.id === o.id;
                  return (
                    <button
                      type="button"
                      class="wtile"
                      classList={{ "wtile--on": on(), "wtile--pinned": pinned() === o.id }}
                      style={{ "--i": String(i()) }}
                      aria-pressed={on() ? "true" : "false"}
                      aria-label={t("wardrobe.wear", { name: nameOf(o) })}
                      onClick={() => void wearOutfit(o.id)}
                    >
                      <OutfitArt outfit={o} />
                      <span class="wtile-ribbon">{nameOf(o)}</span>
                      <span class="wtile-badges" aria-hidden="true">
                        <For each={rulesFor(o.id)}>{(s) => <span class="wtile-badge" title={t(s.label)}>{s.icon()}</span>}</For>
                      </span>
                      <Show when={on()}>
                        <span class="wtile-check" aria-hidden="true">
                          <CheckIcon />
                        </span>
                      </Show>
                      <Show when={pinned() === o.id}>
                        <span class="wtile-pin" aria-hidden="true">
                          <PinIcon />
                        </span>
                      </Show>
                    </button>
                  );
                }}
              </For>
            </div>

            {/* Per-section rules: a row of mini tiles per section. */}
            <section class="wardrobe-rules" aria-labelledby="wardrobe-rules-title">
              <h3 id="wardrobe-rules-title" class="wardrobe-subtitle">
                {t("wardrobe.rules")}
              </h3>
              <For each={SECTIONS}>
                {(s) => (
                  <div class="wrule" classList={{ "wrule--here": avatarSection() === s.id, "wrule--off": !avatarSettings().auto && s.id !== "default" }}>
                    <span class="wrule-label" id={`wrule-${s.id}`}>
                      <span class="wrule-icon" aria-hidden="true">{s.icon()}</span>
                      {t(s.label)}
                    </span>
                    <div class="wrule-picks" role="radiogroup" aria-labelledby={`wrule-${s.id}`}>
                      <For each={outfits()}>
                        {(o) => {
                          const sel = () => avatarSettings().rules[s.id] === o.id;
                          return (
                            <button
                              type="button"
                              role="radio"
                              class="wpick"
                              classList={{ "wpick--on": sel() }}
                              aria-checked={sel() ? "true" : "false"}
                              aria-label={t("wardrobe.ruleFor", { section: t(s.label), name: nameOf(o) })}
                              title={nameOf(o)}
                              onClick={() => void updateAvatar({ rules: { [s.id]: o.id } })}
                            >
                              <OutfitArt outfit={o} size="sm" />
                            </button>
                          );
                        }}
                      </For>
                    </div>
                  </div>
                )}
              </For>
            </section>
          </Show>

          <Show when={avatarError()}>
            {(msg) => (
              <p class="form-error" role="alert">
                {msg()}
              </p>
            )}
          </Show>
          <p class="wardrobe-hint muted small">{t("wardrobe.askHint")}</p>
        </section>
      </div>
    </Show>
  );
}
