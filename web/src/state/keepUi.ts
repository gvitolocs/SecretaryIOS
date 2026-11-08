// Keep's UI state that outlives a render: the nav, list/grid, search, selection. Per browser.

import { createSignal } from "solid-js";
import { emptySearch, type SearchState } from "../lib/keepFilter";
import { readString, writeString } from "../lib/storage";

const LIST_KEY = "secretary.keep.list";
const NAV_KEY = "secretary.keep.navCollapsed";

const [listMode, setListModeSignal] = createSignal(readString("local", LIST_KEY) === "1");
const [navCollapsed, setNavCollapsedSignal] = createSignal(readString("local", NAV_KEY) === "1");
/** The phone's drawer. */
const [drawerOpen, setDrawerOpen] = createSignal(false);
const [search, setSearch] = createSignal<SearchState>(emptySearch());
/** The search field has focus (or filters are active): Keep shows its filter page. */
const [searchOpen, setSearchOpen] = createSignal(false);
const [selection, setSelection] = createSignal<ReadonlySet<string>>(new Set<string>());

export { listMode, navCollapsed, drawerOpen, setDrawerOpen, search, setSearch, searchOpen, setSearchOpen, selection };

export function setListMode(on: boolean): void {
  setListModeSignal(on);
  writeString("local", LIST_KEY, on ? "1" : "0");
}
export function setNavCollapsed(on: boolean): void {
  setNavCollapsedSignal(on);
  writeString("local", NAV_KEY, on ? "1" : "0");
}

export function toggleSelected(id: string): void {
  const next = new Set(selection());
  if (!next.delete(id)) next.add(id);
  setSelection(next);
}
export function clearSelection(): void {
  if (selection().size) setSelection(new Set<string>());
}
export function selectAll(ids: string[]): void {
  setSelection(new Set(ids));
}
