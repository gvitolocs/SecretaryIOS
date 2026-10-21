import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { PlusIcon, RefreshIcon } from "../components/Icons";
import { readString, writeString } from "../lib/storage";
import {
  addItem,
  dismissShoppingError,
  refreshing,
  refreshShopping,
  shop,
  toggleItem,
  type ShoppingRow,
} from "../state/shopping";

const BOUGHT_OPEN_KEY = "secretary.boughtOpen";

function ItemRow(props: { item: ShoppingRow }) {
  const inputId = () => `item-${props.item.id}`;
  return (
    <li class="item" classList={{ "item--checked": props.item.checked, "item--pending": props.item.pending }}>
      <input
        id={inputId()}
        class="item-check"
        type="checkbox"
        checked={props.item.checked}
        disabled={props.item.temp || !props.item.hasId}
        onChange={(e) => {
          // Keep the DOM in step with the store when a tap is ignored.
          e.currentTarget.checked = props.item.checked;
          void toggleItem(props.item.id);
        }}
      />
      <label class="item-label" for={inputId()}>
        <span class="item-name">{props.item.name}</span>
        <Show when={props.item.quantity}>{(q) => <span class="item-qty">{q()}</span>}</Show>
      </label>
    </li>
  );
}

export default function ShoppingView() {
  const [draft, setDraft] = createSignal("");
  const [boughtOpen, setBoughtOpen] = createSignal(readString("local", BOUGHT_OPEN_KEY) === "1");
  const open = createMemo(() => shop.items.filter((i) => !i.checked));
  const bought = createMemo(() => shop.items.filter((i) => i.checked));

  onMount(() => {
    void refreshShopping();
    const onFocus = () => void refreshShopping({ ifOlderThanMs: 2_000 });
    const onVisible = () => {
      if (document.visibilityState === "visible") onFocus();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisible);
    });
  });

  function onAdd(e: SubmitEvent) {
    e.preventDefault();
    const name = draft().trim();
    if (!name) return;
    setDraft("");
    void addItem(name).then((ok) => {
      // Give the text back if it failed and the field is still empty.
      if (!ok && !draft()) setDraft(name);
    });
  }

  return (
    <div class="shopping">
      <div class="shopping-inner">
        <form class="add-form" onSubmit={onAdd}>
          <label class="sr-only" for="add-item">
            Add an item
          </label>
          <input
            id="add-item"
            class="text-input"
            type="text"
            placeholder="Add an item"
            value={draft()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            enterkeyhint="done"
            autocomplete="off"
            maxlength="200"
          />
          <button class="icon-btn icon-btn--accent" type="submit" aria-label="Add item" disabled={!draft().trim()}>
            <PlusIcon />
          </button>
          <button
            class="icon-btn"
            classList={{ "is-spinning": refreshing() }}
            type="button"
            onClick={() => void refreshShopping()}
            aria-label="Refresh list"
            title="Refresh list"
            disabled={refreshing()}
          >
            <RefreshIcon />
          </button>
        </form>

        <Show when={shop.actionError}>
          {(msg) => (
            <div class="notice" role="alert">
              <span>{msg()}</span>
              <button type="button" class="banner-close" onClick={dismissShoppingError} aria-label="Dismiss">
                ×
              </button>
            </div>
          )}
        </Show>

        <Show
          when={shop.loaded}
          fallback={
            <Show
              when={shop.loadError}
              fallback={
                <p class="muted center" role="status">
                  Loading your list…
                </p>
              }
            >
              {(msg) => (
                <div class="empty" role="alert">
                  <p class="empty-title">{msg()}</p>
                  <button type="button" class="btn" onClick={() => void refreshShopping()}>
                    Try again
                  </button>
                </div>
              )}
            </Show>
          }
        >
          <Show when={shop.loadError}>
            {(msg) => (
              <p class="muted small" role="status">
                {msg()} Showing the last copy.
              </p>
            )}
          </Show>
          <Show
            when={shop.items.length > 0}
            fallback={
              <div class="empty">
                <p class="empty-title">Your list is empty.</p>
                <p class="empty-sub">Add something above, or ask Lucina to.</p>
              </div>
            }
          >
            <Show when={open().length > 0} fallback={<p class="muted center">Everything's bought. Nicely done.</p>}>
              <ul class="items" aria-label="To buy">
                <For each={open()}>{(item) => <ItemRow item={item} />}</For>
              </ul>
            </Show>
            <Show when={bought().length > 0}>
              <details
                class="bought"
                open={boughtOpen()}
                onToggle={(e) => {
                  const isOpen = e.currentTarget.open;
                  setBoughtOpen(isOpen);
                  writeString("local", BOUGHT_OPEN_KEY, isOpen ? "1" : "0");
                }}
              >
                <summary>Bought ({bought().length})</summary>
                <ul class="items" aria-label="Bought">
                  <For each={bought()}>{(item) => <ItemRow item={item} />}</For>
                </ul>
              </details>
            </Show>
          </Show>
        </Show>
      </div>
    </div>
  );
}
