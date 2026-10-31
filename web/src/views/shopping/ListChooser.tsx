import { createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import { themeName } from "../../lib/shoppingI18n";
import { DEFAULT_LIST_ID, LIST_THEMES, THEME_ICON, type ListTheme, type ShopList } from "../../lib/shoppingOps";
import {
  createList,
  creatingList,
  deletingListId,
  removeList,
  savingListId,
  selectList,
  shop,
  t,
  updateList,
} from "../../state/shopping";
import { Button } from "../../components/page";
import { ShopSheet } from "./ShopSheet";

function ThemeSelect(props: { id: string; value: ListTheme; wide?: boolean; onChange: (t: ListTheme) => void }) {
  return (
    <select id={props.id} class={props.wide ? "pg-input" : "bring-field"} value={props.value} onChange={(e) => props.onChange(e.currentTarget.value as ListTheme)}>
      <For each={LIST_THEMES}>
        {(theme) => (
          <option value={theme}>
            {THEME_ICON[theme]} {themeName(shop.lang, theme)}
          </option>
        )}
      </For>
    </select>
  );
}

/** Rename / theme / delete form shown under a card after tapping its gear. */
function ListSettings(props: { list: ShopList; wide?: boolean; onDone: () => void }) {
  const [name, setName] = createSignal(props.list.name);
  const [theme, setTheme] = createSignal<ListTheme>(props.list.theme);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const fieldId = () => `bring-list-name-${props.list.id}`;
  const themeId = () => `bring-list-theme-${props.list.id}`;

  async function save(e: SubmitEvent) {
    e.preventDefault();
    const clean = name().trim();
    if (!clean) return;
    const patch: { name?: string; theme?: ListTheme } = {};
    if (clean !== props.list.name) patch.name = clean;
    if (theme() !== props.list.theme) patch.theme = theme();
    if (patch.name === undefined && patch.theme === undefined) return props.onDone();
    if (await updateList(props.list.id, patch)) props.onDone();
  }

  return (
    <form class="bring-list-settings" onSubmit={save} aria-label={t("listSettings", { name: props.list.name })}>
      <label class="bring-label" for={fieldId()}>
        {t("listName")}
      </label>
      <input id={fieldId()} class={props.wide ? "pg-input" : "bring-field"} type="text" maxlength="80" value={name()} onInput={(e) => setName(e.currentTarget.value)} />
      <label class="bring-label" for={themeId()}>
        {t("theme")}
      </label>
      <ThemeSelect id={themeId()} value={theme()} wide={props.wide} onChange={setTheme} />
      <Show
        when={props.wide}
        fallback={
          <button type="submit" class="bring-btn bring-btn--primary bring-btn--block" disabled={savingListId() === props.list.id || !name().trim()}>
            {t("save")}
          </button>
        }
      >
        <Button type="submit" disabled={savingListId() === props.list.id || !name().trim()}>
          {t("save")}
        </Button>
      </Show>
      <Show when={props.list.id !== DEFAULT_LIST_ID} fallback={<p class="bring-muted">{t("defaultListLocked")}</p>}>
        <Show
          when={confirmDelete()}
          fallback={
            <Show
              when={props.wide}
              fallback={
                <button type="button" class="bring-btn bring-btn--secondary bring-btn--block" onClick={() => setConfirmDelete(true)}>
                  {t("deleteList")}
                </button>
              }
            >
              <Button variant="ghost" onClick={() => setConfirmDelete(true)}>
                {t("deleteList")}
              </Button>
            </Show>
          }
        >
          <p class="bring-muted">{t("deleteListConfirm", { name: props.list.name })}</p>
          <Show
            when={props.wide}
            fallback={
              <button
                type="button"
                class="bring-btn bring-btn--destructive bring-btn--block"
                disabled={deletingListId() === props.list.id}
                onClick={() => void removeList(props.list.id)}
              >
                {t("deleteList")}
              </button>
            }
          >
            <Button variant="danger" disabled={deletingListId() === props.list.id} onClick={() => void removeList(props.list.id)}>
              {t("deleteList")}
            </Button>
          </Show>
        </Show>
      </Show>
    </form>
  );
}

/**
 * Full-screen list chooser (Bring's `fragment_bring_list_chooser`): an app bar
 * with a back button, one card per list (themed band, red count badge, big name,
 * gear for settings, a ring on the current list), then the "Create list" card.
 */
export function ListChooser(props: { wide?: boolean; open: boolean; onClose: () => void }) {
  const [settingsFor, setSettingsFor] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [newName, setNewName] = createSignal("");
  const [newTheme, setNewTheme] = createSignal<ListTheme>("grocery");
  let backBtn: HTMLButtonElement | undefined;

  const neededHere = createMemo(() => shop.items.filter((i) => i.status === "needed").length);
  /** Live count for the open list (optimistic), the server's for the others. */
  const countFor = (l: ShopList) => (l.id === shop.currentListId ? neededHere() : l.needed);

  createEffect(
    on(
      () => props.open,
      (open) => {
        if (!open) return;
        setSettingsFor(null);
        setCreating(false);
        setNewName("");
        queueMicrotask(() => backBtn?.focus());
      },
    ),
  );

  async function onCreate(e: SubmitEvent) {
    e.preventDefault();
    const name = newName().trim();
    if (!name) return;
    if (await createList(name, newTheme())) props.onClose();
  }

  const Cards = () => (
    <>
      <ul class="bring-cards" aria-label={t("lists")}>
        <For each={shop.lists}>
          {(list) => {
            const current = () => list.id === shop.currentListId;
            return (
              <li class="bring-card-item">
                <div class={`bring-card bring-theme--${list.theme}`} classList={{ "bring-card--current": current() }}>
                  <button
                    type="button"
                    class="bring-card-main"
                    aria-current={current() ? "true" : undefined}
                    aria-label={t("openList", { name: list.name, n: countFor(list) })}
                    onClick={() => {
                      selectList(list.id);
                      props.onClose();
                    }}
                  >
                    <span class="bring-card-art" aria-hidden="true">
                      {THEME_ICON[list.theme]}
                    </span>
                    <span class="bring-badge" aria-hidden="true">
                      {countFor(list)}
                    </span>
                    <span class="bring-card-name">{list.name}</span>
                    <Show when={current()}>
                      <span class="bring-card-current">{t("currentList")}</span>
                    </Show>
                  </button>
                  <button
                    type="button"
                    class="bring-gear"
                    aria-label={t("listSettings", { name: list.name })}
                    aria-expanded={settingsFor() === list.id}
                    onClick={() => setSettingsFor((id) => (id === list.id ? null : list.id))}
                  >
                    <span aria-hidden="true">⚙︎</span>
                  </button>
                </div>
                <Show when={settingsFor() === list.id}>
                  <ListSettings list={list} wide={props.wide} onDone={() => setSettingsFor(null)} />
                </Show>
              </li>
            );
          }}
        </For>
      </ul>

      <div class="bring-create">
        <Show
          when={creating()}
          fallback={
            <button type="button" class="bring-create-btn" onClick={() => setCreating(true)}>
              <span class="bring-create-plus" aria-hidden="true">
                +
              </span>
              <span>{t("createList")}</span>
            </button>
          }
        >
          <form class="bring-list-settings" onSubmit={onCreate} aria-label={t("createList")}>
            <label class="bring-label" for="bring-new-name">
              {t("listName")}
            </label>
            <input
              id="bring-new-name"
              class={props.wide ? "pg-input" : "bring-field"}
              type="text"
              maxlength="80"
              value={newName()}
              onInput={(e) => setNewName(e.currentTarget.value)}
              ref={(el) => queueMicrotask(() => el.focus())}
            />
            <label class="bring-label" for="bring-new-theme">
              {t("theme")}
            </label>
            <ThemeSelect id="bring-new-theme" value={newTheme()} wide={props.wide} onChange={setNewTheme} />
            <Show
              when={props.wide}
              fallback={
                <button type="submit" class="bring-btn bring-btn--primary bring-btn--block" disabled={creatingList() || !newName().trim()}>
                  {t("create")}
                </button>
              }
            >
              <Button type="submit" disabled={creatingList() || !newName().trim()}>
                {t("create")}
              </Button>
            </Show>
          </form>
        </Show>
      </div>
    </>
  );

  return (
    <Show when={props.open}>
      <Show
        when={props.wide}
        fallback={
          <div
            class="bring-chooser"
            role="dialog"
            aria-modal="true"
            aria-labelledby="bring-chooser-title"
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.stopPropagation();
                props.onClose();
              }
            }}
          >
            <header class="bring-chooser-bar">
              <button ref={backBtn} type="button" class="bring-back" onClick={() => props.onClose()} aria-label={t("close")}>
                <span class="bring-chevron-back" aria-hidden="true" />
              </button>
              <h2 id="bring-chooser-title" class="bring-chooser-title">
                {t("lists")}
              </h2>
            </header>
            <Cards />
          </div>
        }
      >
        <ShopSheet open title={t("lists")} onClose={props.onClose}>
          <Cards />
        </ShopSheet>
      </Show>
    </Show>
  );
}
