import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { createStore } from "solid-js/store";
import { catalogName, itemKey, LANGS, OTHER_SECTION, type CatalogItem, type Lang } from "../lib/shoppingCatalog";
import { LANG_LABEL } from "../lib/shoppingI18n";
import { THEME_ICON, type ShopItem } from "../lib/shoppingOps";
import { readJSON, writeJSON } from "../lib/storage";
import {
  addCatalogItem,
  clearRecent,
  dismissShoppingError,
  dismissUndo,
  initShopping,
  itemIcon,
  itemSection,
  onOnline,
  refreshing,
  refreshShopping,
  removeItems,
  rowKeyId,
  setGrouped,
  setLang,
  shop,
  syncing,
  t,
  tapItem,
  undoLast,
} from "../state/shopping";
import "../styles/shopping.css";
import { DetailsSheet } from "./shopping/DetailsSheet";
import { withFlip } from "./shopping/flip";
import { ListChooser } from "./shopping/ListChooser";
import { SearchSheet } from "./shopping/SearchSheet";
import { onTileKeyDown, Tile } from "./shopping/Tile";

const EXPANDED_KEY = "secretary.shopping.expanded";
const RECENT_SECTION = "__recent";
/** App bar: 148 px expanded, collapses to the 56 px toolbar (Bring's CollapsingToolbarLayout). */
const APPBAR_COLLAPSE_PX = 148 - 56;

interface Group {
  id: string;
  name: string;
  icon: string;
  items: ShopItem[];
}

function loadExpanded(): Record<string, boolean> {
  const raw = readJSON("local", EXPANDED_KEY);
  const out: Record<string, boolean> = { [RECENT_SECTION]: true };
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    for (const [k, v] of Object.entries(raw)) if (typeof v === "boolean") out[k] = v;
  }
  return out;
}

/** Header of a collapsible home section (Bring's `view_home_collapsible_section`). */
function SectionHeader(props: {
  id: string;
  title: string;
  open: boolean;
  onToggle: () => void;
  onMore?: () => void;
  moreOpen?: boolean;
}) {
  return (
    <div class="bring-section-head">
      <button
        type="button"
        class="bring-section-toggle"
        aria-expanded={props.open}
        aria-controls={`bring-sec-${props.id}`}
        onClick={() => props.onToggle()}
      >
        <span class="bring-section-chevron" classList={{ "bring-section-chevron--open": props.open }} aria-hidden="true" />
        <span class="bring-section-title">{props.title}</span>
      </button>
      <Show when={props.onMore}>
        <button
          type="button"
          class="bring-more"
          aria-label={t("more", { name: props.title })}
          aria-haspopup="menu"
          aria-expanded={!!props.moreOpen}
          onClick={() => props.onMore?.()}
        >
          <span aria-hidden="true">•••</span>
        </button>
      </Show>
    </div>
  );
}

export default function ShoppingView() {
  const [detailsId, setDetailsId] = createSignal<string | null>(null);
  const [chooserOpen, setChooserOpen] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [recentMenu, setRecentMenu] = createSignal(false);
  const [collapsed, setCollapsed] = createSignal(false);
  const [editing, setEditing] = createSignal(false);
  const [selected, setSelected] = createStore<Record<string, boolean>>({});
  const [expanded, setExpanded] = createStore<Record<string, boolean>>(loadExpanded());
  let root: HTMLDivElement | undefined;
  let board: HTMLDivElement | undefined;
  let listsBtn: HTMLButtonElement | undefined;

  onMount(() => {
    initShopping();
    const onFocus = () => refreshShopping({ ifOlderThanMs: 2_000 });
    const onVisible = () => {
      if (document.visibilityState === "visible") onFocus();
    };
    window.addEventListener("focus", onFocus);
    window.addEventListener("online", onOnline);
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => {
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("online", onOnline);
      document.removeEventListener("visibilitychange", onVisible);
    });
  });

  const currentList = createMemo(() => shop.lists.find((l) => l.id === shop.currentListId));
  const listName = () => currentList()?.name ?? "";
  const needed = createMemo(() => shop.items.filter((i) => i.status === "needed"));
  const recent = createMemo(() =>
    shop.items.filter((i) => i.status === "recent").sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)),
  );
  const selectedIds = createMemo(() => recent().filter((i) => selected[i.id]).map((i) => i.id));

  /** To-buy tiles grouped by catalog section, in section order; unknown sections last. */
  const groups = createMemo<Group[]>(() => {
    const order = new Map(shop.catalog.sections.map((s, i) => [s.id, i]));
    const byId = new Map<string, Group>();
    for (const item of needed()) {
      let id = itemSection(item);
      if (!order.has(id)) id = OTHER_SECTION;
      let g = byId.get(id);
      if (!g) {
        const s = shop.catalog.sections.find((x) => x.id === id);
        g = { id, name: s?.name ?? t("otherSection"), icon: s?.icon ?? "📦", items: [] };
        byId.set(id, g);
      }
      g.items.push(item);
    }
    const rank = (id: string) => order.get(id) ?? Number.MAX_SAFE_INTEGER;
    return [...byId.values()].sort((a, b) => rank(a.id) - rank(b.id));
  });

  /** Catalog items per section, for the collapsible catalog below "Recently". */
  const catalogSections = createMemo(() => {
    const bySection = new Map<string, CatalogItem[]>();
    for (const item of shop.catalog.items) {
      const id = item.section ?? OTHER_SECTION;
      const list = bySection.get(id) ?? [];
      list.push(item);
      bySection.set(id, list);
    }
    return shop.catalog.sections.filter((s) => bySection.has(s.id)).map((s) => ({ ...s, items: bySection.get(s.id) ?? [] }));
  });
  const rowByKey = createMemo(() => new Map(shop.items.map((i) => [itemKey(rowKeyId(i), i.name), i])));

  const act = (fn: () => void) => withFlip(root, fn);

  function toggleSection(id: string) {
    setExpanded(id, !expanded[id]);
    writeJSON("local", EXPANDED_KEY, { ...expanded });
  }

  function clearSelection() {
    setSelected(Object.fromEntries(Object.keys(selected).map((k) => [k, false])));
  }

  function startEditing() {
    setRecentMenu(false);
    clearSelection();
    setEditing(true);
  }

  function stopEditing() {
    setEditing(false);
    clearSelection();
  }

  function itemLabel(item: ShopItem): string {
    const name = item.spec ? `${item.name} (${item.spec})` : item.name;
    if (editing() && item.status === "recent") return t(selected[item.id] ? "selectedRecent" : "selectRecent", { name });
    return t(item.status === "needed" ? "toBuyItem" : "recentItem", { name });
  }

  function specOf(item: ShopItem): string {
    const qty = item.quantity !== null ? `${item.quantity}${item.unit ? ` ${item.unit}` : ""}` : "";
    return [qty, item.spec].filter(Boolean).join(" · ");
  }

  const ItemTile = (p: { item: ShopItem }) => {
    const isEditing = () => editing() && p.item.status === "recent";
    return (
      <Tile
        flipId={p.item.id}
        kind={p.item.status === "needed" ? "need" : "recent"}
        icon={itemIcon(p.item)}
        name={p.item.name}
        spec={specOf(p.item)}
        label={itemLabel(p.item)}
        checked={isEditing() ? !!selected[p.item.id] : undefined}
        onTap={() => (isEditing() ? setSelected(p.item.id, !selected[p.item.id]) : act(() => tapItem(p.item.id)))}
        onDetails={() => setDetailsId(p.item.id)}
        detailsLabel={t("itemOptions", { name: p.item.name })}
      />
    );
  };

  return (
    <div class="bring" ref={root} lang={shop.lang}>
      <div class="bring-scroll" onScroll={(e) => setCollapsed(e.currentTarget.scrollTop > APPBAR_COLLAPSE_PX - 8)}>
        <header
          class={`bring-appbar bring-theme--${currentList()?.theme ?? "home"}`}
          classList={{ "bring-appbar--collapsed": collapsed() }}
        >
          <span class="bring-appbar-art" aria-hidden="true">
            {THEME_ICON[currentList()?.theme ?? "home"]}
          </span>
          <div class="bring-toolbar">
            <button
              ref={listsBtn}
              type="button"
              class="bring-back-pill"
              aria-label={t("switchList", { name: listName() })}
              aria-haspopup="dialog"
              onClick={() => setChooserOpen(true)}
            >
              <span class="bring-chevron-back" aria-hidden="true" />
              <span>{t("backToLists")}</span>
            </button>
            <span class="bring-toolbar-title" aria-hidden="true">
              {listName()}
            </span>
            <div class="bring-toolbar-end">
              <Show when={shop.pending > 0 || !shop.online}>
                <span class="bring-sync" classList={{ "bring-sync--offline": !shop.online }} role="status">
                  {shop.online ? t("syncing") : t("offline", { n: shop.pending })}
                </span>
              </Show>
              <button
                type="button"
                class="bring-round-btn"
                aria-label={t("settings")}
                aria-haspopup="menu"
                aria-expanded={menuOpen()}
                onClick={() => setMenuOpen((v) => !v)}
              >
                <span aria-hidden="true">⋯</span>
              </button>
            </div>
          </div>
          <h1 class="bring-title">
            <span class="bring-title-text">{listName()}</span>
            <span class="bring-badge bring-badge--title" role="status" aria-label={t("toBuyCount", { n: needed().length })}>
              {needed().length}
            </span>
          </h1>
          <Show when={menuOpen()}>
            <div class="bring-menu" role="menu" aria-label={t("settings")} onKeyDown={(e) => e.key === "Escape" && setMenuOpen(false)}>
              <label class="bring-menu-row">
                <input type="checkbox" checked={shop.grouped} onChange={(e) => setGrouped(e.currentTarget.checked)} />
                <span>{t("groupToggle")}</span>
              </label>
              <label class="bring-menu-row">
                <span>{t("language")}</span>
                <select class="bring-menu-select" value={shop.lang} onChange={(e) => setLang(e.currentTarget.value as Lang)}>
                  <For each={LANGS}>{(l) => <option value={l}>{LANG_LABEL[l]}</option>}</For>
                </select>
              </label>
              <button
                type="button"
                class="bring-menu-row"
                role="menuitem"
                disabled={refreshing() || syncing()}
                onClick={() => {
                  setMenuOpen(false);
                  refreshShopping();
                }}
              >
                {t("refresh")}
              </button>
            </div>
          </Show>
        </header>

        <Show when={shop.actionError}>
          {(msg) => (
            <div class="bring-notice" role="alert">
              <span>{msg()}</span>
              <button type="button" class="bring-notice-close" onClick={dismissShoppingError} aria-label={t("close")}>
                ×
              </button>
            </div>
          )}
        </Show>

        <div class="bring-board" ref={board} onKeyDown={(e) => onTileKeyDown(e, board)}>
          <Show
            when={shop.loaded}
            fallback={
              <Show
                when={shop.loadError}
                fallback={
                  <p class="bring-muted bring-center" role="status">
                    {t("loading")}
                  </p>
                }
              >
                {(msg) => (
                  <div class="bring-empty" role="alert">
                    <p class="bring-empty-title">{msg()}</p>
                    <button type="button" class="bring-btn bring-btn--secondary" onClick={() => refreshShopping()}>
                      {t("retry")}
                    </button>
                  </div>
                )}
              </Show>
            }
          >
            <Show when={shop.loadError}>
              {(msg) => (
                <p class="bring-muted bring-pad" role="status">
                  {msg()} {t("showingCached")}
                </p>
              )}
            </Show>

            {/* To buy */}
            <section class="bring-tobuy" aria-label={t("toBuy")}>
              <Show
                when={needed().length > 0}
                fallback={
                  <div class="bring-empty">
                    <span class="bring-empty-art" aria-hidden="true">
                      🧺
                    </span>
                    <p class="bring-empty-title">{t("emptyTitle")}</p>
                    <p class="bring-empty-hint">{t("emptyHint")}</p>
                  </div>
                }
              >
                <Show
                  when={shop.grouped}
                  fallback={
                    <div class="bring-grid" role="list" aria-label={t("toBuy")}>
                      <For each={needed()}>{(item) => <ItemTile item={item} />}</For>
                    </div>
                  }
                >
                  <For each={groups()}>
                    {(g) => (
                      <>
                        <h2 class="bring-group-title">{g.name}</h2>
                        <div class="bring-grid" role="list" aria-label={g.name}>
                          <For each={g.items}>{(item) => <ItemTile item={item} />}</For>
                        </div>
                      </>
                    )}
                  </For>
                </Show>
              </Show>
            </section>

            {/* Recently used */}
            <Show when={recent().length > 0}>
              <section class="bring-section">
                <SectionHeader
                  id={RECENT_SECTION}
                  title={t("recently")}
                  open={!!expanded[RECENT_SECTION] || editing()}
                  onToggle={() => toggleSection(RECENT_SECTION)}
                  onMore={() => setRecentMenu((v) => !v)}
                  moreOpen={recentMenu()}
                />
                <Show when={recentMenu()}>
                  <div class="bring-menu bring-menu--section" role="menu" aria-label={t("recently")}>
                    <button type="button" class="bring-menu-row" role="menuitem" onClick={startEditing}>
                      {t("editRecently")}
                    </button>
                    <button
                      type="button"
                      class="bring-menu-row"
                      role="menuitem"
                      onClick={() => {
                        setRecentMenu(false);
                        act(clearRecent);
                      }}
                    >
                      {t("clearRecently")}
                    </button>
                  </div>
                </Show>
                <Show when={editing()}>
                  <div class="bring-edit-bar">
                    <button type="button" class="bring-btn bring-btn--secondary bring-btn--small" onClick={stopEditing}>
                      {t("cancel")}
                    </button>
                    <button
                      type="button"
                      class="bring-btn bring-btn--secondary bring-btn--small"
                      disabled={selectedIds().length === 0}
                      onClick={() => {
                        const ids = selectedIds();
                        act(() => removeItems(ids));
                        stopEditing();
                      }}
                    >
                      {selectedIds().length ? t("removeN", { n: selectedIds().length }) : t("remove")}
                    </button>
                  </div>
                </Show>
                <Show when={expanded[RECENT_SECTION] || editing()}>
                  <div class="bring-grid" id={`bring-sec-${RECENT_SECTION}`} role="list" aria-label={t("recently")}>
                    <For each={recent()}>{(item) => <ItemTile item={item} />}</For>
                  </div>
                </Show>
              </section>
            </Show>

            {/* Catalog sections */}
            <For each={catalogSections()}>
              {(sec) => (
                <section class="bring-section">
                  <SectionHeader id={sec.id} title={sec.name} open={!!expanded[sec.id]} onToggle={() => toggleSection(sec.id)} />
                  <Show when={expanded[sec.id]}>
                    <div class="bring-grid" id={`bring-sec-${sec.id}`} role="list" aria-label={sec.name}>
                      <For each={sec.items}>
                        {(cat) => {
                          const name = () => catalogName(cat, shop.lang);
                          const row = () => rowByKey().get(itemKey(cat.id, name()));
                          const onList = () => row()?.status === "needed";
                          return (
                            <Tile
                              flipId={`catalog:${cat.id}`}
                              kind="catalog"
                              icon={cat.icon}
                              name={name()}
                              spec={onList() ? row()?.spec : undefined}
                              selected={onList()}
                              label={onList() ? t("toBuyItem", { name: name() }) : t("catalogItem", { name: name() })}
                              onTap={() => {
                                const r = row();
                                act(() => (r?.status === "needed" ? tapItem(r.id) : void addCatalogItem(cat)));
                              }}
                            />
                          );
                        }}
                      </For>
                    </div>
                  </Show>
                </section>
              )}
            </For>
          </Show>
        </div>
      </div>

      <SearchSheet act={act} />

      <div class="bring-snackbar-region" role="status" aria-live="polite">
        <Show when={shop.undo}>
          {(u) => (
            <div class="bring-snackbar">
              <span class="bring-snackbar-msg">{u().message}</span>
              <Show when={u().inverse}>
                <button type="button" class="bring-snackbar-action" onClick={() => act(undoLast)}>
                  {t("undo")}
                </button>
              </Show>
              <button type="button" class="bring-snackbar-close" aria-label={t("close")} onClick={dismissUndo}>
                ×
              </button>
            </div>
          )}
        </Show>
      </div>

      <DetailsSheet itemId={detailsId()} onClose={() => setDetailsId(null)} />
      <ListChooser
        open={chooserOpen()}
        onClose={() => {
          setChooserOpen(false);
          queueMicrotask(() => listsBtn?.focus());
        }}
      />
    </div>
  );
}
