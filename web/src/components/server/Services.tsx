import { A } from "@solidjs/router";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { allComponents, filterServices, isUnhealthy, type ServiceGroup } from "../../api/systemOverview";
import { t } from "../../i18n";
import { Card, EmptyState } from "../page";
import { componentHref, type Links } from "./links";
import { Chip, Dot, levelLabel, StateWord } from "./parts";

// The Services tab: the catalog tree (group → project → components) with a search field. Groups and
// projects are collapsible, unhealthy ones open; while searching everything that matches is open.

export function ServicesTab(props: { services: ServiceGroup[]; links: Links }) {
  const [query, setQuery] = createSignal("");
  const [showRepos, setShowRepos] = createSignal(false);
  const repoCount = () => allComponents(props.services).filter((c) => c.kind === "repo").length;
  const groups = createMemo(() => filterServices(props.services.map((g) => ({ ...g,
    projects: g.projects.map((p) => ({ ...p, components: p.components.filter((c) => showRepos() || c.kind !== "repo") })).filter((p) => p.components.length),
  })).filter((g) => g.projects.length), query()));
  const searching = () => query().trim().length > 0;
  let input: HTMLInputElement | undefined;

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
      const el = e.target as HTMLElement | null;
      if (el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName))) return;
      if (document.querySelector(".pg-sheet")) return;
      e.preventDefault();
      input?.focus();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });

  return (
    <div class="sh-services">
      <div class="sh-search">
        <input
          ref={input}
          type="search"
          class="pg-input"
          placeholder={t("serverHealth.search")}
          aria-label={t("serverHealth.search")}
          title={t("serverHealth.searchHint")}
          aria-keyshortcuts="/"
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
        <button type="button" class="pg-btn" aria-pressed={showRepos()} onClick={() => setShowRepos(!showRepos())}>
          {t("serverHealth.showRepos", { count: repoCount() })}
        </button>
      </div>
      <Show
        when={groups().length}
        fallback={
          <Card>
            <EmptyState text={searching() ? t("serverHealth.noMatch", { query: query().trim() }) : t("serverHealth.noServices")} />
          </Card>
        }
      >
        <For each={groups()}>
          {(g) => (
            <details class="pg-card sh-group" open={searching() || isUnhealthy(g.state)}>
              <summary class="sh-group-head">
                <Dot state={g.state} />
                <span class="sh-group-name">{g.name}</span>
                <StateWord state={g.state} />
                <span class="pg-muted sh-group-count">{t("serverHealth.serviceCount", { count: g.projects.reduce((n, p) => n + p.components.length, 0) })}</span>
              </summary>
              <For each={g.projects}>
                {(p) => (
                  <details class="sh-project" open={searching() || isUnhealthy(p.state)}>
                    <summary class="sh-project-head">
                      <Dot state={p.state} />
                      <span class="sh-project-name">{p.id}</span>
                      <StateWord state={p.state} />
                      <span class="pg-muted sh-group-count">{t("serverHealth.serviceCount", { count: p.components.length })}</span>
                    </summary>
                    <ul class="sh-comps" aria-label={p.id}>
                      <For each={p.components}>
                        {(c) => (
                          <li>
                            <A href={componentHref(props.links, c.id)} class="sh-comp" data-state={c.state}>
                              <Dot state={c.state} />
                              <span class="sh-comp-main">
                                <span class="sh-comp-name">{c.name}</span>
                                <Show when={isUnhealthy(c.state)}><StateWord state={c.state} /></Show>
                              </span>
                              <span class="sh-comp-tags">
                                <Show when={c.host}>
                                  <Chip>{c.host}</Chip>
                                </Show>
                                <Show when={c.criticality === "high" || c.criticality === "critical"}>
                                  <span class="sh-crit" data-level={c.criticality}>
                                    {levelLabel(c.criticality!)}
                                  </span>
                                </Show>
                              </span>
                            </A>
                          </li>
                        )}
                      </For>
                    </ul>
                  </details>
                )}
              </For>
            </details>
          )}
        </For>
      </Show>
    </div>
  );
}
