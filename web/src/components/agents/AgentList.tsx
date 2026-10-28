import { A } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { agentsApi, type AgentListItem, type SearchResult } from "../../api/agents";
import { toApiError } from "../../api/client";
import { t } from "../../i18n";
import { agentHref } from "../../lib/agentsRoute";
import { agentTitle, baseName } from "../../lib/agentsTimeline";
import { agentFeatures, agentList, agentsError, agentsLoaded, agentsState, detect, loadArchived } from "../../state/agents";
import { StatusRing, ago, statusLabel } from "./status";

function matches(a: AgentListItem, q: string): boolean {
  const hay = `${a.title ?? ""} ${a.provider} ${a.model ?? ""} ${a.cwd} ${a.shortId}`.toLowerCase();
  return q
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((w) => hay.includes(w));
}

function Row(props: { a: AgentListItem; current: boolean; snippet?: string }) {
  const pending = () => Object.values(agentsState.permissions).filter((p) => p.agentId === props.a.id && p.status === "pending").length;
  return (
    <li>
      <A href={agentHref(props.a.id)} class="ag-row-link" data-current={props.current ? "yes" : "no"} data-bucket={props.a.bucket} aria-current={props.current ? "page" : undefined}>
        <StatusRing bucket={props.a.bucket} status={props.a.status} />
        <span class="ag-row-link__text">
          <span class="ag-row-link__title">{agentTitle(props.a)}</span>
          <span class="ag-row-link__meta">
            {props.a.provider}
            {` · ${baseName(props.a.cwd)}`}
            <Show when={props.a.updatedAt}>{` · ${ago(props.a.updatedAt)}`}</Show>
          </span>
          <Show when={props.snippet}>
            <span class="ag-row-link__snippet">{props.snippet}</span>
          </Show>
        </span>
        <span class="ag-row-link__state" data-bucket={props.a.bucket}>
          {pending() > 0 ? `${statusLabel(props.a)} (${pending()})` : statusLabel(props.a)}
        </span>
      </A>
    </li>
  );
}

/** The agents, grouped by what needs the owner first; archived ones behind a switch; search when the route exists. */
export function AgentList(props: { current?: string }) {
  const [showArchived, setShowArchived] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [remote, setRemote] = createSignal<SearchResult[] | null>(null);
  const [searchError, setSearchError] = createSignal<string | null>(null);
  const items = createMemo(() => agentList(showArchived()));
  const filtered = createMemo(() => {
    const q = query().trim();
    return q ? items().filter((a) => matches(a, q)) : items();
  });

  createEffect(on(showArchived, (on) => on && void loadArchived().catch(() => undefined)));

  // Search: typo-tolerant and full text on the server (phase 6); hidden where the route does not exist.
  onMount(() => void detect("search", () => agentsApi.search("a", 1)).catch(() => undefined));
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(
    on(query, (q) => {
      clearTimeout(timer);
      setSearchError(null);
      const text = q.trim();
      if (text.length < 2 || agentFeatures.search === false) return setRemote(null);
      timer = setTimeout(async () => {
        try {
          const r = await detect("search", () => agentsApi.search(text, 20));
          if (query().trim() === text) setRemote(r ?? null);
        } catch (e) {
          setSearchError(toApiError(e).message);
        }
      }, 250);
    }),
  );
  onCleanup(() => clearTimeout(timer));

  return (
    <div class="ag-list">
      <Show when={agentFeatures.search !== false}>
        <div class="ag-search">
          <input
            type="search"
            value={query()}
            placeholder={t("agents.searchPlaceholder")}
            aria-label={t("agents.searchLabel")}
            autocomplete="off"
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
        </div>
      </Show>
      <label class="ag-check ag-list__archived">
        <input type="checkbox" checked={showArchived()} onChange={(e) => setShowArchived(e.currentTarget.checked)} /> {t("agents.showArchived")}
      </label>
      <Show when={agentsError()}>
        <p class="ag-note" role="alert">{agentsError()}</p>
      </Show>
      <Show when={searchError()}>
        <p class="ag-note" role="alert">{searchError()}</p>
      </Show>
      <Show
        when={remote() !== null}
        fallback={
          <>
            <Show when={agentsLoaded() && filtered().length === 0 && !agentsError()}>
              <p class="ag-muted ag-empty" role="status">{query().trim() ? t("agents.noMatches") : showArchived() ? t("agents.noArchived") : t("agents.noAgents")}</p>
            </Show>
            <Show when={!agentsLoaded()}>
              <p class="ag-muted ag-empty" role="status">{t("agents.loading")}</p>
            </Show>
            <ul class="ag-rows" aria-label={t("agents.listLabel")}>
              <For each={filtered()}>{(a) => <Row a={a} current={props.current === a.id} />}</For>
            </ul>
          </>
        }
      >
        <ul class="ag-rows" aria-label={t("agents.searchResults")}>
          <For each={remote()!} fallback={<li class="ag-muted ag-empty">{t("agents.noMatches")}</li>}>
            {(r) => {
              const known = (): AgentListItem =>
                agentsState.items[r.agentId] ?? {
                  id: r.agentId,
                  shortId: r.agentId.slice(0, 7),
                  title: r.title,
                  provider: r.provider,
                  status: r.status,
                  cwd: r.cwd,
                  createdAt: r.updatedAt,
                  updatedAt: r.updatedAt,
                  archivedAt: r.archivedAt ?? null,
                  bucket: r.status === "running" ? "running" : r.status === "error" ? "failed" : "done",
                };
              return <Row a={known()} current={props.current === r.agentId} snippet={r.matches[0]?.snippet} />;
            }}
          </For>
        </ul>
      </Show>
    </div>
  );
}
