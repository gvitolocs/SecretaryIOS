import { createEffect, createSignal, For, on, Show } from "solid-js";
import { setItemCategory, type GalleryCategory, type ItemCategory } from "../../../api/gallery";
import { t } from "../../../i18n";
import { corrections, kindKey, moveKey, notKey, type Correction } from "../../../lib/galleryCategories";
import { CategoryIcon } from "./CategoryIcons";
import "../../../styles/gallery-categories.css";

const label = (c: Correction): string =>
  c.kind === "not" ? t(notKey(c.from)) : c.kind === "move" ? t(moveKey(c.to)) : t("galleryCategories.backToAutomatic");

/**
 * The info panel's category: what the server decided (or the owner chose) and a small picker to correct it: "Not a receipt",
 * "Move to Documents", "Back to automatic". The owner's choice is final on the server: no later decision replaces it.
 */
export function CategoryValue(props: { itemId: string; category: ItemCategory; onChanged?: (category: ItemCategory | null) => void }) {
  const [current, setCurrent] = createSignal<ItemCategory | null>(props.category);
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);

  // Another photo, or the server's answer changed under us (the details were asked for again): start from it.
  createEffect(
    on(
      () => [props.itemId, props.category] as const,
      ([, category]) => {
        setCurrent(category);
        setOpen(false);
        setFailed(false);
      },
      { defer: true },
    ),
  );

  const choose = async (c: Correction) => {
    if (busy()) return;
    const id = props.itemId;
    const target: GalleryCategory | null = c.kind === "auto" ? null : c.to;
    setBusy(true);
    setFailed(false);
    try {
      const now = await setItemCategory(id, target);
      if (id !== props.itemId) return;
      setCurrent(now);
      setOpen(false);
      props.onChanged?.(now);
    } catch {
      if (id === props.itemId) setFailed(true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show when={current()}>
      {(cat) => (
        <>
          <span class="cat-row">
            <span class="cat-row__icon">
              <CategoryIcon category={cat().category} />
            </span>
            <span>{t(kindKey(cat().category))}</span>
            <Show when={cat().decidedBy === "owner" || cat().category !== "photo"}>
              <span class="gv-sub">{cat().decidedBy === "owner" ? t("galleryCategories.byYou") : t("galleryCategories.automatic")}</span>
            </Show>
            <button type="button" class="gv-small-btn" aria-expanded={open()} aria-label={t("galleryCategories.changeLabel")} onClick={() => setOpen(!open())}>
              {t("galleryCategories.change")}
            </button>
          </span>
          <Show when={open()}>
            <ul class="cat-menu" aria-label={t("galleryCategories.changeLabel")}>
              <For each={corrections(cat())}>
                {(c) => (
                  <li>
                    <button type="button" class="cat-menu__item" disabled={busy()} onClick={() => void choose(c)}>
                      {label(c)}
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
          <Show when={failed()}>
            <p class="cat-error" role="alert">
              {t("galleryCategories.changeFailed")}
            </p>
          </Show>
        </>
      )}
    </Show>
  );
}
