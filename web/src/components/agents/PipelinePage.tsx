import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { agentsApi, notDeployed, type Bucket, type BoardJob, type BoardWorkspace, type PipelineBoard } from "../../api/agents";
import { toApiError } from "../../api/client";
import { t } from "../../i18n";
import { baseName } from "../../lib/agentsTimeline";
import { StatusRing, ago } from "./status";

// The merge and deploy pipeline at /agents/pipeline: what `lagent board` prints, read-only
// (docs/design/agents-pipeline.md). The board costs agentd seconds of git and process scans across every worktree,
// so it is read every 30 s while the page is visible, and never twice at once.

const REFRESH_MS = 30000;

/** The order on the page: what is moving first, stale work last. */
const STATE_ORDER = ["merging", "merge_queued", "open", "merged", "stale"];

const STATE_LOOK: Record<string, { label: () => string; bucket: Bucket; status?: string }> = {
  merging: { label: () => t("agents.plStateMerging"), bucket: "running" },
  merge_queued: { label: () => t("agents.plStateMergeQueued"), bucket: "needs_input" },
  open: { label: () => t("agents.plStateOpen"), bucket: "done", status: "idle" },
  merged: { label: () => t("agents.plStateMerged"), bucket: "attention" },
  stale: { label: () => t("agents.plStateStale"), bucket: "done" },
};

const JOB_LOOK: Record<BoardJob["state"], { label: () => string; bucket: Bucket; status?: string }> = {
  queued: { label: () => t("agents.plQueued"), bucket: "needs_input" },
  running: { label: () => t("agents.plRunning"), bucket: "running" },
  succeeded: { label: () => t("agents.plSucceeded"), bucket: "done", status: "idle" },
  failed: { label: () => t("agents.plFailed"), bucket: "failed" },
  cancelled: { label: () => t("agents.plCancelled"), bucket: "done" },
};

/** `local_ff6cd29a-3fc2-…` is a Claude session; `nez@nez_opt` a shell on nezopt. */
function shortSession(s: string | null | undefined): string {
  if (!s) return "";
  return s.startsWith("local_") ? `${s.slice(0, 14)}…` : s;
}

function loadLine(w: BoardWorkspace): string {
  if (!w.procs) return "";
  const top = Object.entries(w.commands ?? {})
    .sort((a, b) => b[1] - a[1])
    .slice(0, 3)
    .map(([name, n]) => `${name}×${n}`)
    .join(" ");
  const load = t("agents.plLoad", { procs: w.procs, mb: w.rssMb ?? 0 });
  return top ? `${load} · ${top}` : load;
}

function WorkspaceRow(props: { w: BoardWorkspace }) {
  const look = () => STATE_LOOK[props.w.state] ?? { label: () => props.w.state, bucket: "done" as Bucket };
  const branch = () => props.w.branch ?? props.w.name ?? baseName(props.w.cwd);
  const title = () => props.w.task?.title || branch();
  const where = () => {
    const parts = [props.w.project ?? "", title() === branch() ? "" : branch()];
    if (props.w.ahead != null) parts.push(`+${props.w.ahead}`);
    if (props.w.dirty) parts.push(t("agents.plDirty"));
    if (props.w.idleDays) parts.push(t("agents.plIdleDays", { days: props.w.idleDays }));
    return parts.filter(Boolean).join(" · ");
  };
  const who = () => {
    const parts = [shortSession(props.w.task?.session ?? props.w.task?.owner) || t("agents.plNoSession"), loadLine(props.w)];
    return parts.filter(Boolean).join(" · ");
  };
  return (
    <li class="ag-row-link ag-pl-row">
      <StatusRing bucket={look().bucket} status={look().status} />
      <span class="ag-row-link__text">
        <span class="ag-row-link__title">{title()}</span>
        <span class="ag-row-link__meta">{where()}</span>
        <span class="ag-row-link__meta">{who()}</span>
      </span>
      <span class="ag-row-link__state" data-bucket={look().bucket}>
        {look().label()}
      </span>
    </li>
  );
}

function JobRow(props: { job: BoardJob }) {
  const look = () => JOB_LOOK[props.job.state] ?? { label: () => props.job.state, bucket: "done" as Bucket };
  const kind = () => (props.job.kind === "deploy" ? t("agents.plDeploy") : t("agents.plMerge"));
  const what = () => props.job.runtime ?? props.job.branch ?? props.job.jobId;
  const meta = () =>
    [
      props.job.repo ? baseName(props.job.repo) : "",
      shortSession(props.job.requestedBy),
      ago(props.job.finishedAt ?? props.job.startedAt ?? props.job.createdAt),
      props.job.state === "failed" ? (props.job.outcome?.code ?? "") : "",
    ]
      .filter(Boolean)
      .join(" · ");
  return (
    <li class="ag-row-link ag-pl-row">
      <StatusRing bucket={look().bucket} status={look().status} />
      <span class="ag-row-link__text">
        <span class="ag-row-link__title">{`${kind()}: ${what()}`}</span>
        <span class="ag-row-link__meta">{meta()}</span>
      </span>
      <span class="ag-row-link__state" data-bucket={look().bucket}>
        {look().label()}
      </span>
    </li>
  );
}

export function PipelinePage() {
  const [board, setBoard] = createSignal<PipelineBoard | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [absent, setAbsent] = createSignal(false);
  const [updatedAt, setUpdatedAt] = createSignal<string | null>(null);
  let busy = false;

  const load = async () => {
    if (busy || document.visibilityState === "hidden") return;
    busy = true;
    try {
      setBoard(await agentsApi.pipelineBoard());
      setError(null);
      setUpdatedAt(new Date().toISOString());
    } catch (e) {
      if (notDeployed(e)) setAbsent(true);
      else setError(toApiError(e).message);
    } finally {
      busy = false;
    }
  };

  onMount(() => {
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    const onVisible = () => void load();
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    });
  });

  const workspaces = () => board()?.workspaces ?? [];
  const groups = createMemo(() => {
    const by = new Map<string, BoardWorkspace[]>();
    for (const w of workspaces()) by.set(w.state, [...(by.get(w.state) ?? []), w]);
    const known = STATE_ORDER.filter((s) => by.has(s));
    const others = [...by.keys()].filter((s) => !STATE_ORDER.includes(s));
    return [...known, ...others].map((state) => ({ state, rows: by.get(state)! }));
  });
  const running = createMemo(() => (board()?.queue ?? []).filter((j) => j.state === "queued" || j.state === "running"));
  const recent = createMemo(() => (board()?.recentJobs ?? []).filter((j) => j.state !== "queued" && j.state !== "running").slice(0, 8));
  const leases = createMemo(() => (board()?.leases ?? []).filter((d) => d.lease));
  const deploys = createMemo(() => (board()?.deploys ?? []).slice(0, 5));

  return (
    <section class="ag-page" aria-labelledby="ag-pl-title">
      <h2 id="ag-pl-title">{t("agents.pipeline")}</h2>
      <Show when={absent()}>
        <p class="ag-note" role="status">{t("agents.notAvailable")}</p>
      </Show>
      <Show when={error()}>
        <p class="ag-note" role="alert">{t("agents.plLoadFailed", { reason: error()! })}</p>
      </Show>
      <Show when={!board() && !error() && !absent()}>
        <p class="ag-muted" role="status">{t("agents.loading")}</p>
      </Show>
      <Show when={board()}>
        <p class="ag-muted">
          {t("agents.plSummary", { workspaces: workspaces().length, queue: running().length })}
          <Show when={updatedAt()}>{(at) => ` · ${t("agents.plUpdated", { when: ago(at()) })}`}</Show>
        </p>

        <h3 class="ag-h3">{t("agents.plRunningJobs")}</h3>
        <Show when={running().length > 0} fallback={<p class="ag-muted">{t("agents.plNoJobs")}</p>}>
          <ul class="ag-rows" aria-label={t("agents.plRunningJobs")}>
            <For each={running()}>{(job) => <JobRow job={job} />}</For>
          </ul>
        </Show>

        <h3 class="ag-h3">{t("agents.plWorkspaces")}</h3>
        <Show when={workspaces().length > 0} fallback={<p class="ag-muted">{t("agents.plNoWorkspaces")}</p>}>
          <For each={groups()}>
            {(group) => (
              <>
                <h4 class="ag-pl-group">{`${STATE_LOOK[group.state]?.label() ?? group.state} (${group.rows.length})`}</h4>
                <ul class="ag-rows" aria-label={STATE_LOOK[group.state]?.label() ?? group.state}>
                  <For each={group.rows}>{(w) => <WorkspaceRow w={w} />}</For>
                </ul>
              </>
            )}
          </For>
        </Show>
        <Show when={(board()?.mainCheckouts?.length ?? 0) > 0}>
          <p class="ag-muted">{t("agents.plMainCheckouts", { count: board()!.mainCheckouts!.length })}</p>
        </Show>
        <Show when={(board()?.unmanaged?.length ?? 0) > 0}>
          <p class="ag-muted">{t("agents.plUnmanaged", { count: board()!.unmanaged!.length })}</p>
        </Show>

        <Show when={leases().length > 0}>
          <h3 class="ag-h3">{t("agents.plLeases")}</h3>
          <ul class="ag-rows" aria-label={t("agents.plLeases")}>
            <For each={leases()}>
              {(d) => (
                <li class="ag-row-link ag-pl-row">
                  <StatusRing bucket="running" />
                  <span class="ag-row-link__text">
                    <span class="ag-row-link__title">{d.device}</span>
                    <span class="ag-row-link__meta">{[shortSession(d.lease!.holder), d.lease!.purpose ?? ""].filter(Boolean).join(" · ")}</span>
                  </span>
                  <span class="ag-row-link__state">{t("agents.plUntil", { when: ago(d.lease!.expiresAt) })}</span>
                </li>
              )}
            </For>
          </ul>
        </Show>

        <Show when={recent().length > 0}>
          <h3 class="ag-h3">{t("agents.plRecentJobs")}</h3>
          <ul class="ag-rows" aria-label={t("agents.plRecentJobs")}>
            <For each={recent()}>{(job) => <JobRow job={job} />}</For>
          </ul>
        </Show>

        <Show when={deploys().length > 0}>
          <h3 class="ag-h3">{t("agents.plDeploys")}</h3>
          <ul class="ag-rows" aria-label={t("agents.plDeploys")}>
            <For each={deploys()}>
              {(d) => (
                <li class="ag-row-link ag-pl-row">
                  <StatusRing bucket="done" status="idle" />
                  <span class="ag-row-link__text">
                    <span class="ag-row-link__title">{`${d.runtime} ${d.commit.slice(0, 8)}`}</span>
                    <Show when={d.note}>
                      <span class="ag-row-link__meta">{d.note}</span>
                    </Show>
                  </span>
                  <span class="ag-row-link__state">{ago(d.recorded)}</span>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>
    </section>
  );
}
