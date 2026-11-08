import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { toApiError } from "../../api/client";
import { notesApi, plainExcerpt, type NoteSummary } from "../../api/notes";
import { t } from "../../i18n";

/** A Keep note slug: the same shape the server accepts. */
export const SLUG_RE = /^[a-z0-9](?:[a-z0-9]|-(?=[a-z0-9])){0,63}$/;

/**
 * Pick one of the owner's Keep notes: a search box over /v1/notes and the matches as radio rows.
 * A typed slug that is not among the matches can be used as it is.
 */
export function NotePicker(props: { value: string; onChange: (slug: string) => void; exclude?: string[] }) {
  const [query, setQuery] = createSignal("");
  const [notes, setNotes] = createSignal<NoteSummary[] | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  createEffect(() => {
    const q = query();
    const timer = setTimeout(
      async () => {
        try {
          setError(null);
          setNotes(await notesApi.list(q));
        } catch (e) {
          setError(toApiError(e).message);
        }
      },
      q ? 250 : 0,
    );
    onCleanup(() => clearTimeout(timer));
  });

  const shown = () => (notes() ?? []).filter((n) => !props.exclude?.includes(n.slug)).slice(0, 30);
  const typedSlug = () => {
    const q = query().trim().toLowerCase();
    return SLUG_RE.test(q) && !shown().some((n) => n.slug === q) ? q : null;
  };

  return (
    <div class="nb-picker">
      <input
        class="pg-input"
        type="search"
        placeholder={t("notebooks.pickNoteSearch")}
        aria-label={t("notebooks.pickNoteSearch")}
        value={query()}
        onInput={(e) => setQuery(e.currentTarget.value)}
      />
      <Show when={error()}>
        <p class="pg-notice" role="alert">{error()}</p>
      </Show>
      <ul class="nb-picker-list" role="radiogroup" aria-label={t("notebooks.pickNoteList")}>
        <For each={shown()}>
          {(n) => (
            <li>
              <label class="nb-picker-row" data-selected={props.value === n.slug ? "" : undefined}>
                <input type="radio" name="nb-note" checked={props.value === n.slug} onChange={() => props.onChange(n.slug)} />
                <span class="nb-picker-text">
                  <span class="nb-picker-title">{n.title || n.slug}</span>
                  <span class="nb-picker-sub">{plainExcerpt(n.excerpt, n.title).slice(0, 90) || n.slug}</span>
                </span>
              </label>
            </li>
          )}
        </For>
        <Show when={typedSlug()}>
          {(slug) => (
            <li>
              <label class="nb-picker-row" data-selected={props.value === slug() ? "" : undefined}>
                <input type="radio" name="nb-note" checked={props.value === slug()} onChange={() => props.onChange(slug())} />
                <span class="nb-picker-text">
                  <span class="nb-picker-title">{t("notebooks.useSlug", { slug: slug() })}</span>
                </span>
              </label>
            </li>
          )}
        </Show>
      </ul>
      <Show when={notes() !== null && !shown().length && !typedSlug()}>
        <p class="pg-muted">{t("notebooks.pickNoteNone")}</p>
      </Show>
    </div>
  );
}
