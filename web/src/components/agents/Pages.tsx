import { A, useNavigate } from "@solidjs/router";
import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { agentsApi, type AgentSnapshot, type ProviderInfo, type Schedule, type UsageProvider, type UsageReport } from "../../api/agents";
import { toApiError } from "../../api/client";
import { t } from "../../i18n";
import { agentHref } from "../../lib/agentsRoute";
import { agentTitle } from "../../lib/agentsTimeline";
import { agentFeatures, agentsState, applySnapshot, detect, loadWorkspaces } from "../../state/agents";
import { PermissionCard } from "./PermissionCard";
import { PermissionList } from "./PermissionList";
import { ago } from "./status";

// ---------------------------------------------------------------- new run

/** The "new run" sheet: provider, model, mode, a workspace (or a new worktree of a project) and the first prompt. */
export function NewRun() {
  const navigate = useNavigate();
  const [providers] = createResource(() => agentsApi.providers());
  const [projects] = createResource(() => agentsApi.projects().catch(() => []));
  const [provider, setProvider] = createSignal("");
  const [model, setModel] = createSignal("");
  const [mode, setMode] = createSignal("");
  const [target, setTarget] = createSignal("");
  const [prompt, setPrompt] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [problem, setProblem] = createSignal<string | null>(null);
  void loadWorkspaces().catch(() => undefined);

  const list = (): ProviderInfo[] => (providers.error ? [] : (providers() ?? []));
  const current = () => list().find((p) => p.id === provider()) ?? list()[0];
  const workspaces = () => Object.values(agentsState.workspaces).filter((w) => !w.archivedAt && !w.background);
  const chosen = () => target() || (workspaces()[0] ? `ws:${workspaces()[0]!.workspaceId}` : projects()?.[0] ? `new:${projects()![0]!.projectId}` : "");

  async function submit(e: Event) {
    e.preventDefault();
    const p = current();
    const choice = chosen();
    if (!p || !choice || busy()) return;
    setBusy(true);
    setProblem(null);
    try {
      let cwd = "";
      let workspaceId: string | undefined;
      if (choice.startsWith("new:")) {
        const r = await agentsApi.createWorkspace({ isolation: "worktree", projectId: choice.slice(4), prompt: prompt().trim() || undefined, provider: p.id });
        cwd = r.workspace.cwd;
        workspaceId = r.workspace.workspaceId;
        await loadWorkspaces().catch(() => undefined);
      } else {
        const w = agentsState.workspaces[choice.slice(3)];
        if (!w) throw new Error(t("agents.pickWorkspace"));
        cwd = w.cwd;
        workspaceId = w.workspaceId;
      }
      const snap = await agentsApi.createAgent({
        provider: p.id,
        model: model() || undefined,
        cwd,
        workspaceId,
        prompt: prompt().trim() || undefined,
        modeId: mode() || undefined,
      });
      applySnapshot(snap);
      navigate(agentHref(snap.id));
    } catch (err) {
      setProblem(toApiError(err).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="ag-page" aria-labelledby="ag-new-title">
      <h2 id="ag-new-title">{t("agents.newRun")}</h2>
      <Show when={providers.error}>
        <p class="ag-note" role="alert">{toApiError(providers.error).message}</p>
      </Show>
      <form class="ag-form" onSubmit={submit}>
        <label>
          <span>{t("agents.provider")}</span>
          <select value={current()?.id ?? ""} onChange={(e) => { setProvider(e.currentTarget.value); setModel(""); setMode(""); }}>
            <For each={list()}>{(p) => <option value={p.id}>{p.label}</option>}</For>
          </select>
        </label>
        <label>
          <span>{t("agents.model")}</span>
          <select value={model()} onChange={(e) => setModel(e.currentTarget.value)}>
            <option value="">{t("agents.defaultOption")}</option>
            <For each={(current()?.models ?? []).filter((m) => m.isSelectable !== false)}>{(m) => <option value={m.id}>{m.label}</option>}</For>
          </select>
        </label>
        <Show when={(current()?.modes ?? []).length > 0}>
          <label>
            <span>{t("agents.mode")}</span>
            <select value={mode()} onChange={(e) => setMode(e.currentTarget.value)}>
              <option value="">{t("agents.defaultOption")}</option>
              <For each={current()?.modes ?? []}>{(m) => <option value={m.id}>{m.label}</option>}</For>
            </select>
          </label>
        </Show>
        <label>
          <span>{t("agents.workspace")}</span>
          <select value={chosen()} onChange={(e) => setTarget(e.currentTarget.value)}>
            <For each={workspaces()}>{(w) => <option value={`ws:${w.workspaceId}`}>{`${w.projectDisplayName ? w.projectDisplayName + " / " : ""}${w.name}`}</option>}</For>
            <For each={projects() ?? []}>{(p) => <option value={`new:${p.projectId}`}>{t("agents.newWorktreeIn", { project: p.name })}</option>}</For>
          </select>
        </label>
        <label>
          <span>{t("agents.prompt")}</span>
          <textarea rows={6} value={prompt()} placeholder={t("agents.promptPlaceholder")} onInput={(e) => setPrompt(e.currentTarget.value)} />
        </label>
        <Show when={problem()}>
          <p class="ag-note" role="alert">{problem()}</p>
        </Show>
        <div class="ag-form__actions">
          <button type="submit" class="ag-btn ag-btn--primary" disabled={busy() || !current() || !chosen()}>
            {busy() ? t("agents.starting") : t("agents.start")}
          </button>
          <A href="/agents" class="ag-btn">{t("agents.cancel")}</A>
        </div>
      </form>
    </section>
  );
}

// ---------------------------------------------------------------- permissions

/** Every pending permission (agents' and external ones), and the ones answered lately. */
export function PermissionsPage(props: { focusId?: string }) {
  const recent = createMemo(() =>
    Object.values(agentsState.permissions)
      .filter((p) => p.status !== "pending")
      .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
      .slice(0, 15),
  );
  return (
    <section class="ag-page" aria-labelledby="ag-perm-title">
      <h2 id="ag-perm-title">{t("agents.permissions")}</h2>
      <p class="ag-muted">{t("agents.permissionsSub")}</p>
      <PermissionList showAgent focusId={props.focusId} />
      <Show when={recent().length > 0}>
        <h3 class="ag-h3">{t("agents.permRecent")}</h3>
        <For each={recent()}>{(p) => <PermissionCard permission={p} showAgent />}</For>
      </Show>
    </section>
  );
}

// ---------------------------------------------------------------- schedules (phase 6)

function cadenceText(s: Schedule): string {
  return s.cadence.type === "cron" ? s.cadence.expression + (s.cadence.timezone ? ` (${s.cadence.timezone})` : "") : t("agents.everyMinutes", { count: Math.max(1, Math.round(s.cadence.everyMs / 60000)) });
}

/** Schedules and heartbeats: status, next run, pause, resume and run once. */
export function SchedulesPage() {
  const [rows, { mutate, refetch }] = createResource(async () => (await detect("schedules", () => agentsApi.schedules())) ?? []);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [problem, setProblem] = createSignal<string | null>(null);

  async function act(s: Schedule, action: "pause" | "resume" | "run-once") {
    setBusy(s.id);
    setProblem(null);
    try {
      const next = await agentsApi.scheduleAction(s.id, action);
      mutate((list) => (list ?? []).map((x) => (x.id === s.id ? { ...x, ...next } : x)));
    } catch (e) {
      setProblem(toApiError(e).message);
      void refetch();
    } finally {
      setBusy(null);
    }
  }
  const list = (): Schedule[] => (rows.error ? [] : (rows() ?? []));

  return (
    <section class="ag-page" aria-labelledby="ag-sch-title">
      <h2 id="ag-sch-title">{t("agents.schedules")}</h2>
      <Show when={agentFeatures.schedules === false}>
        <p class="ag-muted">{t("agents.notAvailable")}</p>
      </Show>
      <Show when={rows.error}>
        <p class="ag-note" role="alert">{toApiError(rows.error).message}</p>
      </Show>
      <Show when={problem()}>
        <p class="ag-note" role="alert">{problem()}</p>
      </Show>
      <Show when={agentFeatures.schedules !== false && !rows.loading && list().length === 0 && !rows.error}>
        <p class="ag-muted">{t("agents.noSchedules")}</p>
      </Show>
      <ul class="ag-cards">
        <For each={list()}>
          {(s) => (
            <li class="ag-card" data-state={s.status}>
              <div class="ag-card__main">
                <strong>{s.name || s.prompt.slice(0, 60)}</strong>
                <span class="ag-tag" data-state={s.status}>{s.status === "active" ? t("agents.schActive") : s.status === "paused" ? t("agents.schPaused") : t("agents.schCompleted")}</span>
                <span class="ag-muted">{cadenceText(s)}</span>
                <Show when={s.nextRunAt}><span class="ag-muted">{t("agents.nextRun", { when: ago(s.nextRunAt) })}</span></Show>
                <Show when={s.target.type === "agent"}>
                  <A class="ag-link" href={agentHref((s.target as { agentId: string }).agentId)}>{t("agents.openAgent")}</A>
                </Show>
              </div>
              <div class="ag-card__actions">
                <Show when={s.status === "active"}>
                  <button type="button" class="ag-btn ag-btn--small" disabled={busy() === s.id} onClick={() => void act(s, "pause")}>{t("agents.pause")}</button>
                </Show>
                <Show when={s.status === "paused"}>
                  <button type="button" class="ag-btn ag-btn--small" disabled={busy() === s.id} onClick={() => void act(s, "resume")}>{t("agents.resume")}</button>
                </Show>
                <Show when={s.status !== "completed"}>
                  <button type="button" class="ag-btn ag-btn--small" disabled={busy() === s.id} onClick={() => void act(s, "run-once")}>{t("agents.runOnce")}</button>
                </Show>
              </div>
            </li>
          )}
        </For>
      </ul>
    </section>
  );
}

// ---------------------------------------------------------------- usage

const n = (v: number | undefined): string => new Intl.NumberFormat(document.documentElement.lang || undefined, { notation: "compact", maximumFractionDigits: 1 }).format(v ?? 0);
const usd = (v: number | undefined): string => `$${(v ?? 0).toFixed(2)}`;

/** Tokens and cost per provider (the server's report, else summed from the agents' last usage) and per agent. */
export function UsagePage() {
  const [data] = createResource(async () => {
    const snaps: AgentSnapshot[] = await agentsApi.snapshots(false).catch(() => []);
    const report = await detect("usage", () => agentsApi.usage()).catch(() => undefined);
    return { snaps, report: report as UsageReport | undefined };
  });
  const snaps = () => (data.error ? [] : (data()?.snaps ?? []));

  const providers = createMemo<UsageProvider[]>(() => {
    const r = data.error ? undefined : data()?.report;
    if (r?.providers) return r.providers;
    const by = new Map<string, UsageProvider>();
    for (const s of snaps()) {
      const p = by.get(s.provider) ?? { provider: s.provider, agents: 0, running: 0, inputTokens: 0, cachedInputTokens: 0, outputTokens: 0, totalCostUsd: 0 };
      p.agents++;
      if (s.status === "running") p.running++;
      p.inputTokens += s.lastUsage?.inputTokens ?? 0;
      p.cachedInputTokens += s.lastUsage?.cachedInputTokens ?? 0;
      p.outputTokens += s.lastUsage?.outputTokens ?? 0;
      p.totalCostUsd += s.lastUsage?.totalCostUsd ?? 0;
      by.set(s.provider, p);
    }
    return [...by.values()];
  });
  const perAgent = () =>
    snaps()
      .filter((s) => s.lastUsage)
      .sort((a, b) => (b.lastUsage?.totalCostUsd ?? 0) - (a.lastUsage?.totalCostUsd ?? 0))
      .slice(0, 20);

  return (
    <section class="ag-page" aria-labelledby="ag-use-title">
      <h2 id="ag-use-title">{t("agents.usage")}</h2>
      <Show when={data.error}>
        <p class="ag-note" role="alert">{toApiError(data.error).message}</p>
      </Show>
      <Show when={data.loading}>
        <p class="ag-muted" role="status">{t("agents.loading")}</p>
      </Show>
      <Show when={!data.loading && providers().length === 0 && !data.error}>
        <p class="ag-muted">{t("agents.noUsage")}</p>
      </Show>
      <Show when={providers().length > 0}>
        <div class="ag-tablewrap">
          <table class="ag-table">
            <caption class="ag-sr">{t("agents.usageByProvider")}</caption>
            <thead>
              <tr>
                <th scope="col">{t("agents.provider")}</th>
                <th scope="col">{t("agents.agentsCount")}</th>
                <th scope="col">{t("agents.tokensIn")}</th>
                <th scope="col">{t("agents.tokensCached")}</th>
                <th scope="col">{t("agents.tokensOut")}</th>
                <th scope="col">{t("agents.cost")}</th>
              </tr>
            </thead>
            <tbody>
              <For each={providers()}>
                {(p) => (
                  <tr>
                    <th scope="row">{p.provider}</th>
                    <td>{p.agents}{p.running ? ` (${p.running} ${t("agents.running").toLowerCase()})` : ""}</td>
                    <td>{n(p.inputTokens)}</td>
                    <td>{n(p.cachedInputTokens)}</td>
                    <td>{n(p.outputTokens)}</td>
                    <td>{usd(p.totalCostUsd)}</td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>
      <Show when={perAgent().length > 0}>
        <h3 class="ag-h3">{t("agents.usageByAgent")}</h3>
        <ul class="ag-cards">
          <For each={perAgent()}>
            {(s) => (
              <li class="ag-card">
                <div class="ag-card__main">
                  <A class="ag-link" href={agentHref(s.id)}>{agentTitle({ title: s.title, id: s.id, cwd: s.cwd })}</A>
                  <span class="ag-muted">{s.provider}</span>
                  <span class="ag-muted">{`${n(s.lastUsage?.inputTokens)} / ${n(s.lastUsage?.outputTokens)}`}</span>
                  <strong>{usd(s.lastUsage?.totalCostUsd)}</strong>
                </div>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={data()?.report?.schedules}>
        {(s) => <p class="ag-muted">{t("agents.schedulesSummary", { active: s().active, paused: s().paused, completed: s().completed })}</p>}
      </Show>
    </section>
  );
}
