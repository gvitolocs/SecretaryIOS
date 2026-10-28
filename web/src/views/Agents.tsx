import { A, useLocation } from "@solidjs/router";
import { createMemo, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";
import { agentsApi } from "../api/agents";
import { AgentDetail } from "../components/agents/AgentDetail";
import { AgentList } from "../components/agents/AgentList";
import { MapPage } from "../components/agents/MapPage";
import { NewRun, PermissionsPage, SchedulesPage, UsagePage } from "../components/agents/Pages";
import { BackIcon } from "../components/Icons";
import { t } from "../i18n";
import { parseAgentsPath } from "../lib/agentsRoute";
import { agentFeatures, agentsConnected, agentsLoaded, detect, pendingPermissions, startAgents } from "../state/agents";
import "../styles/agents.css";

// Lucina Agents at /agents (a Plugins card; owner devices): the coding agents on nezopt (Claude, Codex, OpenCode,
// Cursor, …) through /v1/agents. Routes: /agents (list), /agents/new, /agents/<id>[/changes|/files|/terminal|/scripts],
// /agents/permissions[/<id>], /agents/schedules, /agents/usage, /agents/map[/<componentId>] (the Homelab service map). Wide screens show the list beside the page; a
// phone shows one of them (list -> detail). The stream, caches and actions live in state/agents.ts.

export default function AgentsView() {
  const location = useLocation();
  const route = createMemo(() => parseAgentsPath(location.pathname));
  const permissionId = () => {
    const r = route();
    return r.page === "permissions" ? r.id : undefined;
  };
  const open = () => pendingPermissions().length;
  const mapId = () => {
    const r = route();
    return r.page === "map" ? r.id : undefined;
  };

  onMount(() => {
    onCleanup(startAgents());
    void detect("schedules", () => agentsApi.schedules()).catch(() => undefined);
  });

  const agent = createMemo(() => {
    const r = route();
    return r.page === "agent" ? r : undefined;
  });
  const currentId = () => agent()?.id;

  return (
    <section class="ag" data-pane={route().page === "list" ? "side" : "main"} aria-labelledby="agents-title">
      <aside class="ag-side" aria-label={t("agents.listLabel")}>
        <header class="ag-side__head">
          <A href="/plugins" class="media-back ag-back-plugins">
            <BackIcon /> {t("agents.backToPlugins")}
          </A>
          <div class="ag-side__title">
            <h1 id="agents-title">{t("agents.title")}</h1>
            <Show when={agentsLoaded() && !agentsConnected()}>
              <span class="ag-muted" role="status">{t("agents.reconnecting")}</span>
            </Show>
          </div>
          <nav class="ag-nav" aria-label={t("agents.sections")}>
            <A href="/agents/new" class="ag-btn ag-btn--primary" aria-current={route().page === "new" ? "page" : undefined}>{t("agents.newRun")}</A>
            <A href="/agents/permissions" class="ag-btn" aria-current={route().page === "permissions" ? "page" : undefined}>
              {t("agents.permissions")}
              <Show when={open() > 0}>
                <span class="ag-count" aria-label={t("agents.pendingCount", { count: open() })}>{open()}</span>
              </Show>
            </A>
            <Show when={agentFeatures.schedules !== false}>
              <A href="/agents/schedules" class="ag-btn" aria-current={route().page === "schedules" ? "page" : undefined}>{t("agents.schedules")}</A>
            </Show>
            <A href="/agents/usage" class="ag-btn" aria-current={route().page === "usage" ? "page" : undefined}>{t("agents.usage")}</A>
            <A href="/agents/map" class="ag-btn" aria-current={route().page === "map" ? "page" : undefined}>{t("agents.map")}</A>
          </nav>
        </header>
        <AgentList current={currentId()} />
      </aside>
      <div class="ag-main">
        <Switch fallback={<p class="ag-muted ag-placeholder">{t("agents.pickOne")}</p>}>
          <Match when={route().page === "new"}>
            <PageFrame><NewRun /></PageFrame>
          </Match>
          <Match when={route().page === "permissions"}>
            <PageFrame><PermissionsPage focusId={permissionId()} /></PageFrame>
          </Match>
          <Match when={route().page === "schedules"}>
            <PageFrame><SchedulesPage /></PageFrame>
          </Match>
          <Match when={route().page === "usage"}>
            <PageFrame><UsagePage /></PageFrame>
          </Match>
          <Match when={route().page === "map"}>
            <PageFrame><MapPage componentId={mapId()} /></PageFrame>
          </Match>
          <Match when={agent()}>{(a) => <AgentDetail id={a().id} tab={a().tab} />}</Match>
        </Switch>
      </div>
    </section>
  );
}

/** A page that is not an agent: scrolls on its own, with a way back to the list on a phone. */
function PageFrame(props: { children: JSX.Element }) {
  return (
    <div class="ag-pageframe">
      <A href="/agents" class="ag-back">‹ {t("agents.back")}</A>
      {props.children}
    </div>
  );
}
