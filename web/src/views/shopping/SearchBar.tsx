import { createMemo, createSignal, For, Show } from "solid-js";
import { catalogName, exactMatch, fold, itemKey, searchCatalog, type CatalogItem } from "../../lib/shoppingCatalog";
import { SPEC_CHIPS } from "../../lib/shoppingI18n";
import type { ShopItem } from "../../lib/shoppingOps";
import { joinSpec, parseQuery } from "../../lib/shoppingQuery";
import { addCatalogItem, addText, catalogIndex, itemIcon, itemLang, rowKeyId, shop, t, tapItem } from "../../state/shopping";
import { Tile } from "./Tile";

/** The search field's state, shared by the field (in the header) and the suggestion grid (in the board). */
export function createShopSearch() {
  const [query, setQuery] = createSignal("");
  const [chips, setChips] = createSignal<string[]>([]);
  let input: HTMLInputElement | undefined;
  const parsed = createMemo(() => parseQuery(query()));
  const base = {
    query,
    setQuery,
    chips,
    toggleChip: (c: string) => setChips((cs) => (cs.includes(c) ? cs.filter((x) => x !== c) : [...cs, c])),
    /** The item part of the query ("pane" in "pane x2"). */
    text: () => parsed().text,
    /** Typed spec plus the chips that are on. */
    spec: createMemo(() => joinSpec(parsed().spec, chips())),
    active: () => query().trim() !== "",
    clear: () => {
      setQuery("");
      setChips([]);
    },
    setInput: (el: HTMLInputElement) => (input = el),
    focus: () => input?.focus(),
  };
  return { ...base, matches: useMatches(base) };
}

export type ShopSearch = ReturnType<typeof createShopSearch>;

/** Matches for the query: the list's own free-text rows (history), the catalog, and whether to offer a custom tile. */
function useMatches(search: { text: () => string }) {
  const recentRank = createMemo(() => {
    const recent = shop.items.filter((i) => i.status === "recent").sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
    return new Map(recent.map((r, i) => [r.catalogId ?? "", 100 - i]));
  });
  const catalog = createMemo<CatalogItem[]>(() => {
    const q = search.text();
    if (!q) return [];
    return searchCatalog(catalogIndex(), q, { limit: 40, boost: (id) => recentRank().get(id) ?? 0 });
  });
  /** Rows the owner typed themselves (no catalog entry) whose name contains the query; to-buy first. */
  const history = createMemo<ShopItem[]>(() => {
    const q = fold(search.text());
    if (!q) return [];
    return shop.items
      .filter((i) => !i.catalogId && fold(i.name).includes(q))
      .sort((a, b) => Number(fold(b.name).startsWith(q)) - Number(fold(a.name).startsWith(q)) || a.name.localeCompare(b.name))
      .slice(0, 12);
  });
  const exact = createMemo(() => (search.text() ? exactMatch(catalogIndex(), search.text()) : null));
  const exactRow = createMemo(() => history().find((i) => fold(i.name) === fold(search.text())) ?? null);
  const showCustom = createMemo(() => search.text() !== "" && !exact() && !exactRow());
  return { catalog, history, exact, exactRow, showCustom };
}

/** Search field with an add button; quick-spec chips show while typing. */
export function SearchField(props: { search: ShopSearch; act: (fn: () => void) => void; resultsId: string; wide?: boolean }) {
  const s = props.search;
  const m = s.matches;

  /** Enter: the exact catalog item or the owner's own item, the only suggestion, else a new custom item. */
  function addTyped() {
    const text = s.text();
    if (!text) {
      s.focus();
      return;
    }
    const spec = s.spec() || undefined;
    const only = m.catalog().length === 1 && m.history().length === 0 ? m.catalog()[0] : undefined;
    const cat = m.exact() ?? (m.exactRow() ? undefined : only);
    const name = m.exactRow()?.name ?? text;
    // Clear first, so the list is back on screen and the new tile pops into it.
    s.clear();
    props.act(() => void (cat ? addCatalogItem(cat, spec) : addText(name, spec)));
    s.focus();
  }

  return (
    <div class="shop-search">
      <form
        class="shop-search-row"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          addTyped();
        }}
      >
        <label class="sr-only" for="bring-search-field">
          {t("searchLabel")}
        </label>
        <span class="shop-search-glass" aria-hidden="true" />
        <input
          ref={s.setInput}
          id="bring-search-field"
          class={props.wide ? "pg-input" : "shop-search-field"}
          type="search"
          placeholder={t("searchPlaceholder")}
          value={s.query()}
          onInput={(e) => s.setQuery(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && s.active()) {
              e.preventDefault();
              s.clear();
            } else if (e.key === "ArrowDown") {
              const first = document.getElementById(props.resultsId)?.querySelector<HTMLElement>("[data-tile]");
              if (first) {
                e.preventDefault();
                first.focus();
              }
            }
          }}
          enterkeyhint="done"
          autocomplete="off"
          autocapitalize="sentences"
          maxlength="200"
          aria-controls={props.resultsId}
        />
        <Show when={s.active()}>
          <button
            type="button"
            class="shop-search-clear"
            aria-label={t("clearSearch")}
            onClick={() => {
              s.clear();
              s.focus();
            }}
          >
            <span aria-hidden="true">×</span>
          </button>
        </Show>
        <button type="submit" class={props.wide ? "pg-icon-btn" : "shop-search-add"} aria-label={t("addTyped")}>
          <Show when={props.wide} fallback={<span aria-hidden="true" />}>
            <svg class="pg-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 5v14M5 12h14" />
            </svg>
          </Show>
        </button>
      </form>
      <Show when={s.active()}>
        <div class="shop-quick" role="group" aria-label={t("quickSpec")}>
          <For each={SPEC_CHIPS[itemLang()]}>
            {(c) => (
              <button
                type="button"
                class="shop-chip"
                classList={{ "shop-chip--on": s.chips().includes(c) }}
                aria-pressed={s.chips().includes(c)}
                onClick={() => s.toggleChip(c)}
              >
                {c}
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

/** Suggestions as tiles while typing: a custom tile, the owner's own items, then the catalog. */
export function SearchResults(props: { search: ShopSearch; act: (fn: () => void) => void; id: string }) {
  const s = props.search;
  const m = s.matches;
  const rowByKey = createMemo(() => new Map(shop.items.map((i) => [itemKey(rowKeyId(i), i.name), i])));
  const rowFor = (cat: CatalogItem): ShopItem | undefined => rowByKey().get(itemKey(cat.id, catalogName(cat, itemLang())));

  /** Runs an add with the values read now, after clearing the search (so the list shows again and the tile pops in). */
  function done(fn: (text: string, spec: string | undefined) => void) {
    const text = s.text();
    const spec = s.spec() || undefined;
    s.clear();
    props.act(() => fn(text, spec));
    s.focus();
  }

  const nothing = () => !m.showCustom() && m.history().length === 0 && m.catalog().length === 0;

  return (
    <section class="shop-results" id={props.id} aria-label={t("suggestions")}>
      <h2 class="shop-heading">{t("suggestions")}</h2>
      <div
        class="shop-grid"
        role="list"
        aria-label={t("suggestions")}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            s.clear();
            s.focus();
          }
        }}
      >
        <Show when={m.showCustom()}>
          <Tile
            flipId={`custom:${s.text()}`}
            kind="custom"
            icon={null}
            name={s.text()}
            spec={s.spec()}
            label={t("addCustom", { name: s.spec() ? `${s.text()} (${s.spec()})` : s.text() })}
            onTap={() => done((text, spec) => void addText(text, spec))}
          />
        </Show>
        <For each={m.history()}>
          {(row) => {
            const onList = () => row.status === "needed";
            return (
              <Tile
                flipId={`hist:${row.id}`}
                kind={onList() ? "need" : "recent"}
                icon={itemIcon(row)}
                catalogId={row.catalogId}
                iconPicked={row.iconPicked}
                section={row.section}
                name={row.name}
                spec={s.spec() || row.spec}
                label={onList() ? t("toBuyItem", { name: row.name }) : t("recentItem", { name: row.name })}
                onTap={() => done((_, spec) => (onList() && !spec ? tapItem(row.id) : void addText(row.name, spec)))}
              />
            );
          }}
        </For>
        <For each={m.catalog()}>
          {(cat) => {
            const row = () => rowFor(cat);
            const name = () => catalogName(cat, itemLang());
            const onList = () => row()?.status === "needed";
            return (
              <Tile
                flipId={`cat:${cat.id}`}
                kind="catalog"
                icon={cat.icon}
                catalogId={cat.id}
                name={name()}
                spec={s.spec() || (onList() ? row()?.spec : undefined)}
                selected={onList()}
                label={onList() ? t("toBuyItem", { name: name() }) : t("catalogItem", { name: name() })}
                onTap={() => {
                  const r = row();
                  done((_, spec) => (r?.status === "needed" && !spec ? tapItem(r.id) : void addCatalogItem(cat, spec)));
                }}
              />
            );
          }}
        </For>
      </div>
      <Show when={nothing()}>
        <p class="bring-muted shop-pad">{t("noResults")}</p>
      </Show>
    </section>
  );
}
