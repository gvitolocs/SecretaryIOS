import { A } from "@solidjs/router";
import { createMemo, createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { catalogApi, rank, type CatalogComponent, type CatalogTree, type ComponentStatus, type Effective, type TreeProject } from "../../api/catalog";
import { toApiError } from "../../api/client";
import { t } from "../../i18n";
import { agentHref, mapHref } from "../../lib/agentsRoute";
import { ago } from "./status";

// The Homelab service map (/agents/map[/<id>]): group → project → components with live health, the agents and
// workspaces working on each project, and per component its checks, dependencies, last deploy, runbook, logs and
// rollback. Data: the catalog on nezopt (scanned every 5 minutes) through /v1/agents/catalog.

const REFRESH_MS = 60_000;

export function MapPage(props: { componentId?: string }) {
  const [tree, { refetch }] = createResource(() => catalogApi.tree());
  onMount(() => {
    const timer = setInterval(() => void refetch(), REFRESH_MS);
    onCleanup(() => clearInterval(timer));
  });
  return (
    <Show when={props.componentId} fallback={<TreeView tree={tree.error ? undefined : tree()} error={tree.error} loading={tree.loading} refresh={() => void refetch()} />}>
      {(id) => <ComponentDetail id={id()} />}
    </Show>
  );
}

export function levelLabel(e: Effective | undefined): string {
  switch (e) {
    case "ok":
      return t("agents.mapOk");
    case "idle":
      return t("agents.mapIdle");
    case "warn":
      return t("agents.mapWarn");
    case "down":
      return t("agents.mapDown");
    case "broken":
      return t("agents.mapBroken");
    case "degraded":
      return t("agents.mapDegraded");
    default:
      return t("agents.mapUnknown");
  }
}

function Dot(props: { level: Effective | undefined }) {
  return <span class="ag-cat-dot" data-level={props.level ?? "unknown"} aria-hidden="true" />;
}

/** Why a component is not healthy, in one line. */
function why(c: CatalogComponent): string {
  const s = c.status;
  if (!s) return "";
  if (s.impacted_by.length) return t("agents.mapBecause", { chain: s.impacted_by.join(" ← ") });
  const bad = s.findings.find((f) => f.level === "down" || f.level === "warn" || f.level === "unknown");
  return bad ? `${bad.what}: ${bad.detail}` : "";
}

function TreeView(props: { tree: CatalogTree | undefined; error: unknown; loading: boolean; refresh: () => void }) {
  const [query, setQuery] = createSignal("");
  const [problemsOnly, setProblemsOnly] = createSignal(false);
  const all = createMemo(() => (props.tree?.groups ?? []).flatMap((g) => g.projects.flatMap((p) => p.components)));
  const problems = createMemo(() =>
    all()
      .filter((c) => rank(c.status?.effective) >= 3)
      .sort((a, b) => rank(b.status?.effective) - rank(a.status?.effective) || crit(b) - crit(a) || a.id.localeCompare(b.id)),
  );
  const matches = (c: CatalogComponent, q: string) =>
    !q || [c.id, c.name ?? "", c.host ?? "", c.kind, ...(c.serves ?? []), ...(c.units ?? []), ...(c.containers ?? [])].some((x) => x.toLowerCase().includes(q));
  const visible = (p: TreeProject) => {
    const q = query().trim().toLowerCase();
    return p.components.filter((c) => matches(c, q) && (!problemsOnly() || rank(c.status?.effective) >= 3));
  };
  const projectShown = (p: TreeProject) => visible(p).length > 0 || (!problemsOnly() && !!query().trim() && p.id.includes(query().trim().toLowerCase()));
  const hosts = createMemo(() => Object.entries(props.tree?.hosts ?? {}));

  return (
    <section class="ag-page ag-cat" aria-labelledby="ag-map-title">
      <div class="ag-cat__head">
        <h2 id="ag-map-title">{t("agents.map")}</h2>
        <button type="button" class="ag-btn ag-btn--small" onClick={() => props.refresh()} disabled={props.loading}>
          {t("agents.mapRefresh")}
        </button>
      </div>
      <Show when={props.error}>
        <p class="ag-error" role="alert">{toApiError(props.error).message}</p>
      </Show>
      <Show when={props.loading && !props.tree}>
        <p class="ag-muted" role="status">{t("agents.loading")}</p>
      </Show>
      <Show when={props.tree}>
        {(tr) => (
          <>
            <p class="ag-muted">
              {t("agents.mapSummary", { when: tr().generated_at ? ago(tr().generated_at) : "—", count: all().length, unknown: tr().drift.unknown, missing: tr().drift.missing })}
            </p>
            <div class="ag-cat-hosts" role="list" aria-label={t("agents.mapHosts")}>
              <For each={hosts()}>
                {([id, h]) => (
                  <A role="listitem" href={mapHref(id)} class="ag-cat-host" data-up={h.reachable ? "yes" : "no"}>
                    <Dot level={h.reachable ? "ok" : "down"} />
                    <span class="ag-cat-host__name">{id}</span>
                    <span class="ag-muted">{h.reachable ? (h.tctl ? `${Math.round(h.tctl)} °C` : h.uptime) : t("agents.mapUnreachable")}</span>
                  </A>
                )}
              </For>
            </div>
            <Show when={problems().length > 0} fallback={<p class="ag-note">{t("agents.mapAllHealthy")}</p>}>
              <h3 class="ag-h3">{t("agents.mapProblems", { count: problems().length })}</h3>
              <ul class="ag-cat-rows" aria-label={t("agents.mapProblemsLabel")}>
                <For each={problems()}>{(c) => <ComponentRow c={c} showWhy />}</For>
              </ul>
            </Show>
            <div class="ag-cat-filters">
              <div class="ag-search">
                <input
                  type="search"
                  placeholder={t("agents.mapFilter")}
                  aria-label={t("agents.mapFilter")}
                  autocomplete="off"
                  value={query()}
                  onInput={(e) => setQuery(e.currentTarget.value)}
                />
              </div>
              <label class="ag-check">
                <input type="checkbox" checked={problemsOnly()} onChange={(e) => setProblemsOnly(e.currentTarget.checked)} />
                {t("agents.mapProblemsOnly")}
              </label>
            </div>
            <For each={tr().groups}>
              {(g) => (
                <Show when={g.projects.some(projectShown)}>
                  <details class="ag-cat-group" open={rank(g.level) >= 3 || !!query().trim() || problemsOnly()}>
                    <summary>
                      <Dot level={g.level} />
                      <span class="ag-cat-group__name">{g.name}</span>
                      <span class="ag-muted">{t("agents.mapProjects", { count: g.projects.length })}</span>
                    </summary>
                    <For each={g.projects.filter(projectShown)}>{(p) => <ProjectBlock p={p} components={visible(p)} open={!!query().trim() || problemsOnly()} />}</For>
                  </details>
                </Show>
              )}
            </For>
          </>
        )}
      </Show>
    </section>
  );
}

function crit(c: CatalogComponent): number {
  return ["low", "medium", "high", "critical"].indexOf(c.criticality);
}

function ProjectBlock(props: { p: TreeProject; components: CatalogComponent[]; open: boolean }) {
  const agents = () => props.p.agents ?? [];
  const workspaces = () => props.p.workspaces ?? [];
  return (
    <details class="ag-cat-project" open={props.open || rank(props.p.level) >= 3}>
      <summary>
        <Dot level={props.p.level} />
        <span class="ag-cat-project__name">{props.p.id}</span>
        <span class="ag-muted">{t("agents.mapComponents", { count: props.p.components.length })}</span>
        <Show when={agents().length > 0}>
          <span class="ag-cat-badge">{t("agents.mapAgents", { count: agents().length })}</span>
        </Show>
        <Show when={workspaces().length > 0}>
          <span class="ag-cat-badge">{t("agents.mapWorkspaces", { count: workspaces().length })}</span>
        </Show>
      </summary>
      <ul class="ag-cat-rows" aria-label={props.p.id}>
        <For each={props.components}>{(c) => <ComponentRow c={c} />}</For>
      </ul>
      <Show when={agents().length > 0}>
        <ul class="ag-cat-agents" aria-label={t("agents.mapAgentsLabel", { project: props.p.id })}>
          <For each={agents()}>
            {(a) => (
              <li>
                <A href={agentHref(a.id)} class="ag-link">{a.title || a.shortId}</A>
                <span class="ag-muted"> · {a.provider} · {a.status}</span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={workspaces().length > 0}>
        <p class="ag-muted ag-cat-ws">{t("agents.mapWorkspacesList", { names: workspaces().map((w) => w.branch || w.name).join(", ") })}</p>
      </Show>
    </details>
  );
}

function ComponentRow(props: { c: CatalogComponent; showWhy?: boolean }) {
  const e = () => props.c.status?.effective;
  return (
    <li>
      <A href={mapHref(props.c.id)} class="ag-cat-row" data-level={e() ?? "unknown"}>
        <Dot level={e()} />
        <span class="ag-cat-row__main">
          <span class="ag-cat-row__name">{props.c.name || props.c.id}</span>
          <span class="ag-muted">
            {levelLabel(e())} · {props.c.kind}
            <Show when={props.c.host}> · {props.c.host}</Show>
            <Show when={props.showWhy}> · {props.c.project}</Show>
          </span>
          <Show when={props.showWhy && why(props.c)}>
            <span class="ag-muted ag-cat-row__why">{why(props.c)}</span>
          </Show>
        </span>
      </A>
    </li>
  );
}

function ComponentDetail(props: { id: string }) {
  const [comp] = createResource(() => props.id, (id) => catalogApi.component(id));
  const [live, setLive] = createSignal<ComponentStatus | undefined>();
  const [checking, setChecking] = createSignal(false);
  const [checkError, setCheckError] = createSignal<string | undefined>();
  const [impactFor, setImpactFor] = createSignal<string | undefined>();
  const [impact] = createResource(impactFor, (id) => catalogApi.impact([id]));
  const status = () => live() ?? comp()?.status ?? undefined;

  const checkNow = async () => {
    const id = props.id;
    setChecking(true);
    setCheckError(undefined);
    try {
      const r = await catalogApi.checkNow(id);
      if (id === props.id) setLive(r.status);
    } catch (e) {
      setCheckError(toApiError(e).message);
    } finally {
      setChecking(false);
    }
  };

  return (
    <section class="ag-page ag-cat" aria-labelledby="ag-comp-title">
      <A href={mapHref()} class="ag-link ag-cat-back">‹ {t("agents.map")}</A>
      <Show when={comp.error}>
        <p class="ag-error" role="alert">{toApiError(comp.error).message}</p>
      </Show>
      <Show when={comp.loading}>
        <p class="ag-muted" role="status">{t("agents.loading")}</p>
      </Show>
      <Show when={!comp.error && comp()}>
        {(c) => (
          <>
            <div class="ag-cat__head">
              <h2 id="ag-comp-title">
                <Dot level={status()?.effective} /> {c().name || c().id}
              </h2>
              <button type="button" class="ag-btn ag-btn--small" onClick={() => void checkNow()} disabled={checking()}>
                {checking() ? t("agents.mapChecking") : t("agents.mapCheckNow")}
              </button>
            </div>
            <p class="ag-cat-state" data-level={status()?.effective ?? "unknown"} role="status">
              <strong>{levelLabel(status()?.effective)}</strong>
              <Show when={status()?.since}> · {t("agents.mapSince", { when: ago(status()?.since) })}</Show>
              <Show when={status()?.impacted_by.length}>
                {" "}
                · {t("agents.mapBecauseOf")}{" "}
                <For each={status()?.impacted_by ?? []}>
                  {(id, i) => (
                    <>
                      <Show when={i() > 0}> ← </Show>
                      <A href={mapHref(id)} class="ag-link">{id}</A>
                    </>
                  )}
                </For>
              </Show>
            </p>
            <Show when={checkError()}>
              <p class="ag-error" role="alert">{checkError()}</p>
            </Show>
            <p class="ag-muted">
              {c().id} · {c().kind} · {c().group} / {c().project}
              <Show when={c().host}> · <A href={mapHref(c().host!)} class="ag-link">{c().host}</A></Show> · {t("agents.mapCriticality", { level: c().criticality })}
            </p>
            <Show when={c().description}>
              <p>{c().description}</p>
            </Show>

            <Show when={(status()?.findings.length ?? 0) > 0}>
              <h3 class="ag-h3">{t("agents.mapChecks")}</h3>
              <ul class="ag-cat-findings">
                <For each={status()?.findings ?? []}>
                  {(f) => (
                    <li>
                      <Dot level={f.level} />
                      <span>
                        <strong>{f.what}</strong> — {f.detail}
                      </span>
                    </li>
                  )}
                </For>
              </ul>
            </Show>

            <dl class="ag-cat-facts">
              <Links label={t("agents.mapNeeds")} ids={c().depends_on} />
              <Links label={t("agents.mapUses")} ids={c().soft_depends_on} />
              <For each={c().depends_on_any ?? []}>{(g) => <Links label={t("agents.mapNeedsOneOf")} ids={g} />}</For>
              <Links label={t("agents.mapNeededBy")} ids={(c().dependents ?? []).map((d) => d.id)} />
              <Facts label={t("agents.mapServes")} items={c().serves} />
              <Facts label={t("agents.mapUnits")} items={c().units} />
              <Facts label={t("agents.mapContainers")} items={c().containers} />
              <Facts label="k8s" items={c().k8s} />
              <Facts label={t("agents.mapPorts")} items={c().ports?.map(String)} />
              <Show when={c().repo || c().path}>
                <dt>{t("agents.mapRepo")}</dt>
                <dd>
                  <Show when={c().repo}>
                    <A href={mapHref(c().repo!)} class="ag-link">{c().repo}</A>
                  </Show>
                  <Show when={c().branch}> · {t("agents.mapLine", { branch: c().branch! })}</Show>
                  <Show when={c().path}> · <code>{c().path}</code></Show>
                </dd>
              </Show>
              <Show when={c().deploy}>
                {(d) => (
                  <>
                    <dt>{t("agents.mapDeployed")}</dt>
                    <dd>
                      <code>{d().commit.slice(0, 10)}</code> {d().subject} · {d().target} · {ago(d().recorded)}
                    </dd>
                  </>
                )}
              </Show>
            </dl>

            <Command label={t("agents.mapLogs")} cmd={c().logs} />
            <Command label={t("agents.mapRestart")} cmd={c().restart} />
            <Command label={t("agents.mapRollback")} cmd={c().rollback} />
            <Show when={c().runbook?.trim()}>
              <h3 class="ag-h3">{t("agents.mapRunbook")}</h3>
              <p class="ag-cat-runbook">{c().runbook!.trim()}</p>
            </Show>

            <h3 class="ag-h3">{t("agents.mapImpact")}</h3>
            <Show
              when={impactFor() === c().id && impact()}
              fallback={
                <button type="button" class="ag-btn" onClick={() => setImpactFor(c().id)} disabled={impact.loading}>
                  {t("agents.mapWhatBreaks")}
                </button>
              }
            >
              {(im) => (
                <Show when={im().broken.length + im().degraded.length > 0} fallback={<p class="ag-muted">{t("agents.mapNoDependents")}</p>}>
                  <p class="ag-muted">{t("agents.mapImpactSummary", { broken: im().broken.length, degraded: im().degraded.length })}</p>
                  <ul class="ag-cat-rows" aria-label={t("agents.mapImpact")}>
                    <For each={[...im().broken, ...im().degraded]}>
                      {(i) => (
                        <li>
                          <A href={mapHref(i.id)} class="ag-cat-row" data-level={i.effect}>
                            <Dot level={i.effect} />
                            <span class="ag-cat-row__main">
                              <span class="ag-cat-row__name">{i.id}</span>
                              <span class="ag-muted">
                                {levelLabel(i.effect)} · {i.criticality}
                                <Show when={i.serves.length}> · {i.serves.join(", ")}</Show>
                              </span>
                            </span>
                          </A>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              )}
            </Show>
            <Show when={impact.error}>
              <p class="ag-error" role="alert">{toApiError(impact.error).message}</p>
            </Show>
          </>
        )}
      </Show>
    </section>
  );
}

function Links(props: { label: string; ids: string[] | undefined }) {
  return (
    <Show when={(props.ids?.length ?? 0) > 0}>
      <dt>{props.label}</dt>
      <dd>
        <For each={props.ids}>
          {(id, i) => (
            <>
              <Show when={i() > 0}>, </Show>
              <A href={mapHref(id)} class="ag-link">{id}</A>
            </>
          )}
        </For>
      </dd>
    </Show>
  );
}

function Facts(props: { label: string; items: string[] | undefined }) {
  return (
    <Show when={(props.items?.length ?? 0) > 0}>
      <dt>{props.label}</dt>
      <dd>
        <For each={props.items}>
          {(x, i) => (
            <>
              <Show when={i() > 0}>, </Show>
              <code>{x}</code>
            </>
          )}
        </For>
      </dd>
    </Show>
  );
}

function Command(props: { label: string; cmd: string | null | undefined }) {
  return (
    <Show when={props.cmd}>
      <h3 class="ag-h3">{props.label}</h3>
      <pre class="ag-cat-cmd"><code>{props.cmd}</code></pre>
    </Show>
  );
}
