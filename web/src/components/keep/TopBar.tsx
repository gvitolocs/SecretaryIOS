import { Show } from "solid-js";
import { t } from "../../i18n";
import { colorName } from "./Pickers";
import { typeLabel } from "./SearchPage";
import { swatchFill, type KeepTheme } from "../../lib/keepColors";
import { searchActive, type SearchState } from "../../lib/keepFilter";
import { BackIcon, CloseIcon, GridViewIcon, KeepLogo, LabelIcon, ListViewIcon, MenuIcon, RefreshIcon, SearchIcon, SettingsIcon } from "./icons";

export function TopBar(props: {
  search: SearchState;
  searchOpen: boolean;
  theme: KeepTheme;
  labelName: (tag: string) => string;
  listMode: boolean;
  syncing: boolean;
  onMenu: () => void;
  onHome: () => void;
  onSearchFocus: () => void;
  onSearch: (p: Partial<SearchState>) => void;
  onSearchExit: () => void;
  onRefresh: () => void;
  onToggleList: () => void;
  onSettings: () => void;
}) {
  let input: HTMLInputElement | undefined;
  return (
    <header class="kp-top">
      <button type="button" class="kp-icon-btn kp-icon-btn--lg kp-top-menu" aria-label={t("keep.menu")} title={t("keep.menu")} onClick={props.onMenu}>
        <MenuIcon />
      </button>
      <a
        class="kp-brand"
        href="/keep"
        onClick={(e) => {
          e.preventDefault();
          props.onHome();
        }}
      >
        <KeepLogo />
        <span class="kp-brand-name">{t("keep.title")}</span>
      </a>
      <form class="kp-search" classList={{ "kp-search--open": props.searchOpen }} role="search" onSubmit={(e) => e.preventDefault()}>
        <button
          type="button"
          class="kp-icon-btn kp-icon-btn--lg"
          aria-label={props.searchOpen ? t("keep.searchBack") : t("keep.search")}
          title={props.searchOpen ? t("keep.searchBack") : t("keep.search")}
          onClick={() => (props.searchOpen ? props.onSearchExit() : input?.focus())}
        >
          {props.searchOpen ? <BackIcon /> : <SearchIcon />}
        </button>
        <Show when={props.search.type}>
          {(type) => (
            <span class="kp-search-chip" data-chip="type">
              {typeLabel(type())}
              <button type="button" aria-label={t("keep.searchClear")} onClick={() => props.onSearch({ type: undefined })}>
                <CloseIcon />
              </button>
            </span>
          )}
        </Show>
        <Show when={props.search.label}>
          {(tag) => (
            <span class="kp-search-chip" data-chip="label">
              <LabelIcon />
              {props.labelName(tag())}
              <button type="button" aria-label={t("keep.searchClear")} onClick={() => props.onSearch({ label: undefined })}>
                <CloseIcon />
              </button>
            </span>
          )}
        </Show>
        <Show when={props.search.color}>
          {(c) => (
            <span class="kp-search-chip" data-chip="color">
              <span class="kp-search-dot" style={{ "background-color": swatchFill(c(), props.theme) }} />
              {colorName(c())}
              <button type="button" aria-label={t("keep.searchClear")} onClick={() => props.onSearch({ color: undefined })}>
                <CloseIcon />
              </button>
            </span>
          )}
        </Show>
        <input
          ref={input}
          id="kp-search"
          class="kp-search-input"
          type="search"
          autocomplete="off"
          placeholder={t("keep.search")}
          aria-label={t("keep.search")}
          value={props.search.q}
          onFocus={props.onSearchFocus}
          onInput={(e) => props.onSearch({ q: e.currentTarget.value })}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              props.onSearchExit();
              input?.blur();
            } else if (e.key === "Backspace" && !props.search.q) {
              const s = props.search;
              if (s.color) props.onSearch({ color: undefined });
              else if (s.label) props.onSearch({ label: undefined });
              else if (s.type) props.onSearch({ type: undefined });
            }
          }}
        />
        <Show when={searchActive(props.search)}>
          <button
            type="button"
            class="kp-icon-btn kp-icon-btn--lg"
            aria-label={t("keep.searchClear")}
            title={t("keep.searchClear")}
            onClick={() => {
              props.onSearch({ q: "", type: undefined, label: undefined, color: undefined });
              input?.focus();
            }}
          >
            <CloseIcon />
          </button>
        </Show>
      </form>
      <div class="kp-top-actions">
        <button type="button" class="kp-icon-btn kp-icon-btn--lg kp-top-refresh" classList={{ "kp-spin": props.syncing }} aria-label={t("keep.refresh")} title={t("keep.refresh")} onClick={props.onRefresh}>
          <RefreshIcon />
        </button>
        <button type="button" class="kp-icon-btn kp-icon-btn--lg" data-tool="layout" aria-label={props.listMode ? t("keep.gridView") : t("keep.listView")} title={props.listMode ? t("keep.gridView") : t("keep.listView")} onClick={props.onToggleList}>
          {props.listMode ? <GridViewIcon /> : <ListViewIcon />}
        </button>
        <button type="button" class="kp-icon-btn kp-icon-btn--lg kp-top-settings" aria-label={t("keep.settings")} title={t("keep.settings")} onClick={props.onSettings}>
          <SettingsIcon />
        </button>
      </div>
    </header>
  );
}
