import { A } from "@solidjs/router";
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { t } from "../../i18n";
import { agentHref, type AgentTab } from "../../lib/agentsRoute";
import { agentTitle } from "../../lib/agentsTimeline";
import {
  agentFeatures,
  agentsState,
  archiveAgent,
  detachAgent,
  loadSnapshot,
  renameAgent,
  stopAgent,
  unarchiveAgent,
  workspaceOf,
  loadTerminals,
} from "../../state/agents";
import { AgentChat, PromptsMenu } from "./AgentChat";
import { StatusBadge } from "./status";
import { ScriptsPane, TerminalPane } from "./Terminals";
import { DiffPane, FilesPane } from "./Workspace";

const BACK_LABEL = () => t("agents.back");

/** One agent: header (title, status, actions), tabs (chat, changes, files, terminal, scripts) and the open tab. */
export function AgentDetail(props: { id: string; tab: AgentTab }) {
  const item = () => agentsState.items[props.id];
  const snap = () => agentsState.snapshots[props.id];
  const ws = () => workspaceOf(props.id);
  const [missing, setMissing] = createSignal(false);
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [note, setNote] = createSignal<string | null>(null);

  createEffect(
    on(
      () => props.id,
      (id) => {
        setMissing(false);
        setEditing(false);
        setNote(null);
        void loadSnapshot(id)
          .then((s) => setMissing(s === null))
          .catch(() => undefined);
      },
    ),
  );
  createEffect(() => {
    if (agentFeatures.terminals === undefined && ws()) void loadTerminals().catch(() => undefined);
  });

  const archived = () => !!(snap()?.archivedAt ?? item()?.archivedAt);
  const parent = () => (snap()?.labels ?? item()?.labels ?? {})["lucina.parent-agent-id"];
  const title = () => (item() ? agentTitle(item()!) : props.id.slice(0, 7));

  const tabs = (): { id: AgentTab; label: string }[] => {
    const out: { id: AgentTab; label: string }[] = [{ id: "chat", label: t("agents.tabChat") }];
    if (ws()) {
      out.push({ id: "changes", label: t("agents.tabChanges") }, { id: "files", label: t("agents.tabFiles") });
      if (agentFeatures.terminals) out.push({ id: "terminal", label: t("agents.tabTerminal") }, { id: "scripts", label: t("agents.tabScripts") });
    }
    return out;
  };

  // A fade at the right edge of the tab row while more tabs are hidden past it (the row scrolls sideways).
  let tabRow: HTMLElement | undefined;
  const updateFade = () => {
    if (tabRow) tabRow.dataset.more = tabRow.scrollWidth - tabRow.scrollLeft - tabRow.clientWidth > 2 ? "yes" : "no";
  };
  createEffect(() => {
    tabs();
    queueMicrotask(() => {
      updateFade();
      tabRow?.querySelector<HTMLElement>('[data-on="yes"]')?.scrollIntoView?.({ inline: "nearest", block: "nearest" });
    });
  });
  onMount(() => {
    window.addEventListener("resize", updateFade);
    onCleanup(() => window.removeEventListener("resize", updateFade));
  });

  async function run(fn: () => Promise<string | null>) {
    setNote(null);
    setNote(await fn());
  }

  async function saveTitle(e: Event) {
    e.preventDefault();
    const next = draft().trim();
    setEditing(false);
    if (next && next !== item()?.title) await run(() => renameAgent(props.id, next));
  }

  return (
    <section class="ag-detail" aria-label={title()}>
      <Show when={!missing()} fallback={<p class="ag-note" role="alert">{t("agents.notFound")}</p>}>
        <header class="ag-head">
          <A href="/agents" class="ag-back" aria-label={BACK_LABEL()}>‹ {BACK_LABEL()}</A>
          <div class="ag-head__text">
            <Show
              when={editing()}
              fallback={
                <h2 class="ag-head__title">
                  <span>{title()}</span>
                  <button type="button" class="ag-iconbtn" aria-label={t("agents.rename")} title={t("agents.rename")} onClick={() => { setDraft(item()?.title ?? ""); setEditing(true); }}>✎</button>
                </h2>
              }
            >
              <form class="ag-rename" onSubmit={saveTitle}>
                <input
                  value={draft()}
                  maxLength={200}
                  aria-label={t("agents.rename")}
                  ref={(el) => queueMicrotask(() => el.focus())}
                  onInput={(e) => setDraft(e.currentTarget.value)}
                  onKeyDown={(e) => e.key === "Escape" && setEditing(false)}
                />
                <button type="submit" class="ag-btn ag-btn--small ag-btn--primary">{t("agents.save")}</button>
              </form>
            </Show>
            <p class="ag-muted ag-head__meta">
              {item()?.provider}
              <Show when={item()?.model}>{` · ${item()!.model}`}</Show>
              <Show when={ws()}>{` · ${ws()!.projectDisplayName ? ws()!.projectDisplayName + " / " : ""}${ws()!.name}`}</Show>
              <Show when={!ws() && item()?.cwd}>{` · ${item()!.cwd}`}</Show>
            </p>
          </div>
          <Show when={item()}>{(a) => <StatusBadge agent={a()} />}</Show>
          <div class="ag-head__actions">
            <Show when={props.tab === "chat" || !ws()}>
              <PromptsMenu id={props.id} />
            </Show>
            <Show when={item()?.status === "running" && props.tab !== "chat"}>
              <button type="button" class="ag-btn ag-btn--small" onClick={() => void run(() => stopAgent(props.id))}>{t("agents.stop")}</button>
            </Show>
            <Show when={parent() && agentFeatures.detach !== false}>
              <button type="button" class="ag-btn ag-btn--small" onClick={() => void run(() => detachAgent(props.id))}>{t("agents.detach")}</button>
            </Show>
            <button type="button" class="ag-btn ag-btn--small" onClick={() => void run(() => (archived() ? unarchiveAgent(props.id) : archiveAgent(props.id)))}>
              {archived() ? t("agents.unarchive") : t("agents.archive")}
            </button>
          </div>
        </header>
        <Show when={note()}>
          <p class="ag-note" role="alert">{note()}</p>
        </Show>
        <Show when={tabs().length > 1}>
          <nav class="ag-tabs" aria-label={t("agents.sections")} ref={tabRow} onScroll={updateFade}>
            <For each={tabs()}>
              {(x) => (
                <A href={agentHref(props.id, x.id)} class="ag-tab" aria-current={props.tab === x.id ? "page" : undefined} data-on={props.tab === x.id ? "yes" : "no"}>
                  {x.label}
                </A>
              )}
            </For>
          </nav>
        </Show>
        <div class="ag-body">
          <Show when={props.tab === "chat" || !ws()}>
            <AgentChat id={props.id} />
          </Show>
          <Show when={props.tab === "changes" && ws()}>
            <DiffPane workspaceId={ws()!.workspaceId} />
          </Show>
          <Show when={props.tab === "files" && ws()}>
            <FilesPane workspaceId={ws()!.workspaceId} />
          </Show>
          <Show when={props.tab === "terminal" && ws() && agentFeatures.terminals}>
            <TerminalPane workspaceId={ws()!.workspaceId} />
          </Show>
          <Show when={props.tab === "scripts" && ws() && agentFeatures.terminals}>
            <ScriptsPane workspaceId={ws()!.workspaceId} />
          </Show>
        </div>
      </Show>
    </section>
  );
}
