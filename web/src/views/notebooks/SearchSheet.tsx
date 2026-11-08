import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { toApiError } from "../../api/client";
import { chunkOrdinal, notebooksApi, refSourceId, type SearchHit } from "../../api/notebooks";
import { SearchIcon } from "../../components/Icons";
import { Sheet } from "../../components/page";
import { t, type MessageKey } from "../../i18n";

const KIND_LABEL: Record<SearchHit["kind"], MessageKey> = {
  source: "notebooks.hitSource",
  note: "notebooks.hitNote",
  insight: "notebooks.hitInsight",
};

/** Search within one notebook: passages of its sources, its notes and its insights. */
export function SearchSheet(props: { open: boolean; notebookId: string; onClose: () => void; onOpenSource: (sourceId: string, ordinal: number | null) => void }) {
  const [query, setQuery] = createSignal("");
  const [hits, setHits] = createSignal<SearchHit[] | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  createEffect(() => {
    if (props.open) {
      setQuery("");
      setHits(null);
      setError(null);
    }
  });

  createEffect(
    on(query, (q) => {
      const text = q.trim();
      if (text.length < 2) {
        setHits(null);
        return;
      }
      const timer = setTimeout(async () => {
        try {
          setError(null);
          const found = await notebooksApi.search(props.notebookId, text);
          if (query().trim() === text) setHits(found);
        } catch (e) {
          setError(toApiError(e).message);
        }
      }, 300);
      onCleanup(() => clearTimeout(timer));
    }),
  );

  function open(hit: SearchHit) {
    const sourceId = hit.source_id ?? (hit.kind === "source" ? refSourceId(hit.ref) : null);
    if (!sourceId) return;
    props.onOpenSource(sourceId, hit.kind === "source" ? chunkOrdinal(hit.ref) : null);
    props.onClose();
  }

  return (
    <Sheet
      open={props.open}
      onClose={props.onClose}
      title={t("notebooks.searchTitle")}
      class="nb-sheet"
      tools={
        <label class="nb-search-field">
          <SearchIcon />
          <input
            class="pg-input"
            type="search"
            placeholder={t("notebooks.searchPlaceholder")}
            aria-label={t("notebooks.searchPlaceholder")}
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
        </label>
      }
    >
      <Show when={error()}>
        <p class="pg-notice" role="alert">{error()}</p>
      </Show>
      <Show when={hits()} fallback={<p class="pg-muted">{t("notebooks.searchHint")}</p>}>
        {(list) => (
          <Show when={list().length} fallback={<p class="pg-muted">{t("notebooks.searchNone", { query: query().trim() })}</p>}>
            <ul class="nb-hits">
              <For each={list()}>
                {(hit) => (
                  <li class="nb-hit">
                    <Show
                      when={hit.kind === "note" && hit.slug}
                      fallback={
                        <button type="button" class="nb-hit-main" onClick={() => open(hit)}>
                          <span class="nb-hit-title">{hit.title}</span>
                          <span class="nb-hit-kind">{t(KIND_LABEL[hit.kind])}</span>
                          <span class="nb-hit-excerpt">{hit.excerpt}</span>
                        </button>
                      }
                    >
                      <a class="nb-hit-main" href={`/keep/note/${hit.slug}`}>
                        <span class="nb-hit-title">{hit.title}</span>
                        <span class="nb-hit-kind">{t(KIND_LABEL[hit.kind])}</span>
                        <span class="nb-hit-excerpt">{hit.excerpt}</span>
                      </a>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        )}
      </Show>
    </Sheet>
  );
}
