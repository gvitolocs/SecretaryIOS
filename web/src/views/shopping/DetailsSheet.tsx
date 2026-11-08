import { type JSX, createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import { createStore } from "solid-js/store";
import { LANGS, OTHER_SECTION, type Lang } from "../../lib/shoppingCatalog";
import { LANG_LABEL, relativeTime, sourceName, SPEC_CHIPS } from "../../lib/shoppingI18n";
import { THEME_ICON } from "../../lib/shoppingOps";
import { deleteItem, editItem, itemIcon, itemSection, moveItem, shop, t } from "../../state/shopping";
import { Button, PrimaryAction } from "../../components/page";
import { ItemIcon, SectionIcon } from "./ItemIcon";
import { ShopSheet } from "./ShopSheet";

interface Form {
  name: string;
  /** A custom object's names (edited for every list). */
  names: Record<Lang, string>;
  icon: string;
  spec: string;
  quantity: string;
  unit: string;
  section: string;
}

type Panel = "rename" | "section" | "move" | null;

const QTY_RE = /^x\d+$/i;

/** Toggle a chip in a comma-separated spec; "xN" chips replace each other. */
export function toggleChip(spec: string, chip: string): string {
  const parts = spec
    .split(",")
    .map((p) => p.trim())
    .filter(Boolean);
  const has = parts.some((p) => p.toLowerCase() === chip.toLowerCase());
  let next = parts.filter((p) => p.toLowerCase() !== chip.toLowerCase());
  if (!has) {
    if (QTY_RE.test(chip)) next = next.filter((p) => !QTY_RE.test(p));
    next.push(chip);
  }
  return next.join(", ");
}

/**
 * Item details bottom sheet, in Bring's order: handle; name + Done; the
 * specification field and chips; the settings tiles (rename, section, move);
 * the last change; delete. Edits are kept when the sheet closes (Done, the
 * scrim, Escape), like the original.
 */
export function DetailsSheet(props: { wide?: boolean; itemId: string | null; onClose: () => void }) {
  const item = createMemo(() => (props.itemId ? shop.items.find((i) => i.id === props.itemId) : undefined));
  const [form, setForm] = createStore<Form>({
    name: "",
    names: { it: "", en: "", da: "" },
    icon: "",
    spec: "",
    quantity: "",
    unit: "",
    section: OTHER_SECTION,
  });
  const isCustom = () => item()?.kind === "custom" && !!item()?.objectId;
  const [panel, setPanel] = createSignal<Panel>(null);
  /** Desktop: the first click on Delete arms it, the second deletes. */
  const [armed, setArmed] = createSignal(false);
  let specInput: HTMLInputElement | undefined;
  let lastFocus: HTMLElement | null = null;

  // Reset the form whenever a different item opens (not on every remote edit).
  createEffect(
    on(
      () => props.itemId,
      (id) => {
        const it = id ? shop.items.find((i) => i.id === id) : undefined;
        if (!it) return;
        lastFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        setForm({
          name: it.name,
          names: { it: it.names.it ?? it.name, en: it.names.en ?? it.name, da: it.names.da ?? it.name },
          icon: it.icon ?? "",
          spec: it.spec,
          quantity: it.quantity === null ? "" : String(it.quantity),
          unit: it.unit ?? "",
          section: itemSection(it),
        });
        setPanel(null);
        setArmed(false);
        queueMicrotask(() => specInput?.focus());
      },
    ),
  );

  // The row vanished (deleted elsewhere, or moved): close without saving.
  createEffect(() => {
    if (props.itemId && !item()) finish(false);
  });

  function finish(save: boolean) {
    const it = item();
    if (save && it) {
      const q = form.quantity.trim().replace(",", ".");
      const n = q === "" ? null : Number(q);
      const custom = it.kind === "custom" && !!it.objectId;
      editItem(it.id, {
        ...(custom ? { names: { ...form.names }, icon: form.icon } : { name: form.name }),
        spec: form.spec,
        quantity: n !== null && Number.isFinite(n) && n > 0 ? n : null,
        unit: form.unit.trim() || null,
        section: form.section,
      });
    }
    props.onClose();
    const el = lastFocus;
    lastFocus = null;
    if (el?.isConnected) queueMicrotask(() => el.focus());
  }

  const sections = createMemo(() => {
    const list = shop.catalog.sections.map((s) => ({ id: s.id, icon: s.icon, name: s.name }));
    if (!list.some((s) => s.id === OTHER_SECTION)) list.push({ id: OTHER_SECTION, icon: "📦", name: t("otherSection") });
    return list;
  });
  const otherLists = createMemo(() => shop.lists.filter((l) => l.id !== shop.currentListId));
  const togglePanel = (p: Exclude<Panel, null>) => setPanel((cur) => (cur === p ? null : p));

  const Frame = (p: { children: JSX.Element }) => (
    <Show
      when={props.wide}
      fallback={
        <div class="bring-scrim" onClick={(e) => e.target === e.currentTarget && finish(true)}>
          <div
            class="bring-sheet bring-details"
            role="dialog"
            aria-modal="true"
            aria-labelledby="bring-details-title"
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.stopPropagation();
                finish(true);
              }
            }}
          >
            {p.children}
          </div>
        </div>
      }
    >
      <ShopSheet
        open
        title={form.name || item()?.name}
        onClose={() => finish(true)}
        footer={<PrimaryAction onClick={() => finish(true)}>{t("done")}</PrimaryAction>}
      >
        {p.children}
      </ShopSheet>
    </Show>
  );

  return (
    <Show when={item()}>
      {(it) => (
        <Frame>
          <Show when={!props.wide}>
            <div class="bring-sheet-grip" aria-hidden="true" />
            <header class="bring-details-head">
              <span class="bring-details-icon" aria-hidden="true">
                <ItemIcon icon={itemIcon(it())} catalogId={it().catalogId} name={it().name} section={it().section} iconPicked={it().iconPicked} />
              </span>
              <h2 id="bring-details-title" class="bring-details-name">
                {(isCustom() ? form.names[shop.lang] : form.name) || it().name}
              </h2>
              <button type="button" class="bring-text-btn" onClick={() => finish(true)}>
                {t("done")}
              </button>
            </header>
          </Show>
            <div class="bring-details-body">
              <h3 class="bring-details-title">{t("specTitle")}</h3>
              <label class="sr-only" for="bring-spec">
                {t("specTitle")}
              </label>
              <input
                ref={specInput}
                id="bring-spec"
                class={props.wide ? "pg-input" : "bring-field"}
                type="text"
                maxlength="200"
                placeholder={t("specPlaceholder")}
                value={form.spec}
                onInput={(e) => setForm("spec", e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    finish(true);
                  }
                }}
                enterkeyhint="done"
              />
              <div class="bring-chips" role="group" aria-label={t("specTitle")}>
                <For each={SPEC_CHIPS[shop.lang]}>
                  {(chip) => {
                    const active = () => form.spec.split(",").some((p) => p.trim().toLowerCase() === chip.toLowerCase());
                    return (
                      <button
                        type="button"
                        class={props.wide ? "pg-btn" : "bring-chip"}
                        classList={{ "bring-chip--on": active() }}
                        aria-pressed={active()}
                        onClick={() => setForm("spec", toggleChip(form.spec, chip))}
                      >
                        {chip}
                      </button>
                    );
                  }}
                </For>
              </div>
              <div class="bring-qty-row">
                <label class="bring-qty">
                  <span>{t("quantity")}</span>
                  <input
                    class={props.wide ? "pg-input" : "bring-field"}
                    type="text"
                    inputmode="decimal"
                    maxlength="8"
                    value={form.quantity}
                    onInput={(e) => setForm("quantity", e.currentTarget.value)}
                  />
                </label>
                <label class="bring-qty">
                  <span>{t("unit")}</span>
                  <input
                    class={props.wide ? "pg-input" : "bring-field"}
                    type="text"
                    maxlength="20"
                    placeholder="kg, g, l, pz"
                    value={form.unit}
                    onInput={(e) => setForm("unit", e.currentTarget.value)}
                  />
                </label>
              </div>

              <h3 class="bring-details-title">{t("settingsTitle")}</h3>
              <div class="bring-controls">
                <button
                  type="button"
                  class={props.wide ? "pg-btn" : "bring-control"}
                  aria-expanded={panel() === "rename"}
                  onClick={() => togglePanel("rename")}
                >
                  <span class="bring-control-icon" aria-hidden="true">
                    ✏️
                  </span>
                  <span>{isCustom() ? t("editNames") : t("rename")}</span>
                </button>
                <button
                  type="button"
                  class={props.wide ? "pg-btn" : "bring-control"}
                  aria-expanded={panel() === "section"}
                  onClick={() => togglePanel("section")}
                >
                  <span class="bring-control-icon" aria-hidden="true">
                    🗂️
                  </span>
                  <span>{t("changeSection")}</span>
                </button>
                <Show when={otherLists().length > 0}>
                  <button
                    type="button"
                    class={props.wide ? "pg-btn" : "bring-control"}
                    aria-expanded={panel() === "move"}
                    onClick={() => togglePanel("move")}
                  >
                    <span class="bring-control-icon" aria-hidden="true">
                      ↪️
                    </span>
                    <span>{t("moveItem")}</span>
                  </button>
                </Show>
              </div>

              <Show when={panel() === "rename" && isCustom()}>
                <div class="bring-panel bring-names">
                  <Show when={it().needsTranslation}>
                    <p class="bring-hint">{t("needsTranslation")}</p>
                  </Show>
                  <For each={LANGS}>
                    {(lang) => (
                      <label class="bring-qty">
                        <span>{LANG_LABEL[lang]}</span>
                        <input
                          class="bring-field"
                          type="text"
                          maxlength="100"
                          lang={lang}
                          value={form.names[lang]}
                          onInput={(e) => setForm("names", lang, e.currentTarget.value)}
                        />
                      </label>
                    )}
                  </For>
                  <label class="bring-qty">
                    <span>{t("itemIcon")}</span>
                    <input
                      class="bring-field"
                      type="text"
                      maxlength="16"
                      placeholder="🛒"
                      value={form.icon}
                      onInput={(e) => setForm("icon", e.currentTarget.value)}
                    />
                  </label>
                  <p class="bring-hint">{t("namesHint")}</p>
                </div>
              </Show>
              <Show when={panel() === "rename" && !isCustom()}>
                <div class="bring-panel">
                  <label class="sr-only" for="bring-rename">
                    {t("rename")}
                  </label>
                  <input
                    id="bring-rename"
                    class={props.wide ? "pg-input" : "bring-field"}
                    type="text"
                    maxlength="200"
                    value={form.name}
                    onInput={(e) => setForm("name", e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        setPanel(null);
                      }
                    }}
                  />
                </div>
              </Show>
              <Show when={panel() === "section"}>
                <div class="bring-panel" role="radiogroup" aria-label={t("chooseSection")}>
                  <For each={sections()}>
                    {(s) => (
                      <button
                        type="button"
                        role="radio"
                        class={props.wide ? "pg-btn" : "bring-option"}
                        aria-checked={form.section === s.id}
                        onClick={() => {
                          setForm("section", s.id);
                          setPanel(null);
                        }}
                      >
                        <span aria-hidden="true">
                          <SectionIcon id={s.id} icon={s.icon} />
                        </span>
                        <span>{s.name}</span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
              <Show when={panel() === "move"}>
                <div class="bring-panel" role="group" aria-label={t("chooseList")}>
                  <For each={otherLists()}>
                    {(l) => (
                      <button
                        type="button"
                        class={props.wide ? "pg-btn" : "bring-option"}
                        onClick={() => {
                          // Save pending edits first, so they travel with the item.
                          const id = it().id;
                          finish(true);
                          moveItem(id, l.id);
                        }}
                      >
                        <span aria-hidden="true">{THEME_ICON[l.theme]}</span>
                        <span>{l.name}</span>
                      </button>
                    )}
                  </For>
                </div>
              </Show>

              <Show when={it().updatedBy || it().updatedAt}>
                <h3 class="bring-details-title">{t("historyTitle")}</h3>
                <div class="bring-history">
                  <span class="bring-avatar" aria-hidden="true">
                    {sourceName(it().updatedBy || "?").slice(0, 1).toUpperCase()}
                  </span>
                  <span class="bring-history-text">
                    <span class="bring-history-who">{it().updatedBy ? t("via", { src: sourceName(it().updatedBy) }) : "—"}</span>
                    <span class="bring-history-when">{relativeTime(shop.lang, it().updatedAt)}</span>
                  </span>
                </div>
              </Show>

              <Show
                when={props.wide}
                fallback={
                  <button type="button" class="bring-btn bring-btn--destructive bring-btn--block" onClick={() => deleteItem(it().id)}>
                    {t("deleteItem")}
                  </button>
                }
              >
                <Show
                  when={armed()}
                  fallback={
                    <Button variant="ghost" onClick={() => setArmed(true)}>
                      {t("deleteItem")}
                    </Button>
                  }
                >
                  <div class="shopping-confirm" role="group" aria-label={t("deleteItem")}>
                    <Button variant="danger" onClick={() => deleteItem(it().id)}>
                      {t("deleteItem")}
                    </Button>
                    <Button variant="ghost" onClick={() => setArmed(false)}>
                      {t("cancel")}
                    </Button>
                  </div>
                </Show>
              </Show>
            </div>
        </Frame>
      )}
    </Show>
  );
}
