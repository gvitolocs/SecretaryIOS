import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import type { PromptEntry } from "../../api/agents";
import { agentsApi } from "../../api/agents";
import { t } from "../../i18n";
import { projectTimeline } from "../../lib/agentsTimeline";
import { agentFeatures, agentsState, detect, loadTimeline, sendPrompt, stopAgent } from "../../state/agents";
import { PermissionList } from "./PermissionList";
import { TimelineView } from "./TimelineView";

const coarsePointer = (): boolean => {
  try {
    return typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches;
  } catch {
    return false;
  }
};

/** The composer: Send while the agent is idle; while it runs, Steer (adds to the running turn) and Stop. */
export function Composer(props: { id: string }) {
  const [text, setText] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [note, setNote] = createSignal<string | null>(null);
  const status = () => agentsState.snapshots[props.id]?.status ?? agentsState.items[props.id]?.status ?? "idle";
  const running = () => status() === "running";
  const archived = () => !!(agentsState.snapshots[props.id]?.archivedAt ?? agentsState.items[props.id]?.archivedAt);

  async function submit(e?: Event) {
    e?.preventDefault();
    const prompt = text().trim();
    if (!prompt || busy()) return;
    setBusy(true);
    setNote(null);
    const err = await sendPrompt(props.id, prompt, running());
    setBusy(false);
    if (err) setNote(err);
    else setText("");
  }

  async function stop() {
    setNote(null);
    const err = await stopAgent(props.id);
    if (err) setNote(err);
  }

  return (
    <form class="ag-composer" onSubmit={submit}>
      <Show when={note()}>
        <p class="ag-note" role="alert">{note()}</p>
      </Show>
      <div class="ag-composer__row">
        <textarea
          rows={2}
          value={text()}
          disabled={archived()}
          placeholder={archived() ? t("agents.archivedHint") : running() ? t("agents.steerPlaceholder") : t("agents.sendPlaceholder")}
          aria-label={running() ? t("agents.steerLabel") : t("agents.messageLabel")}
          onInput={(e) => setText(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing && !coarsePointer()) void submit(e);
          }}
        />
        <div class="ag-composer__buttons">
          <button type="submit" class="ag-btn ag-btn--primary" disabled={busy() || archived() || !text().trim()}>
            {running() ? t("agents.steer") : t("agents.send")}
          </button>
          <Show when={running()}>
            <button type="button" class="ag-btn" onClick={() => void stop()}>
              {t("agents.stop")}
            </button>
          </Show>
        </div>
      </div>
    </form>
  );
}

/** The conversation of one agent: timeline, pending permissions, the finished-run gate and the composer. */
export function AgentChat(props: { id: string }) {
  let scroller: HTMLDivElement | undefined;
  let stick = true;

  const tl = () => agentsState.timelines[props.id];
  const entries = createMemo(() => projectTimeline(tl()?.rows ?? []));
  const snap = () => agentsState.snapshots[props.id];

  createEffect(
    on(
      () => props.id,
      (id) => {
        stick = true;
        void loadTimeline(id);
      },
    ),
  );

  // Follow the end of the conversation while the reader is at the end.
  createEffect(() => {
    entries().length;
    const last = entries()[entries().length - 1];
    last?.item.text;
    snap()?.pendingPermissions.length;
    queueMicrotask(() => {
      if (stick && scroller) scroller.scrollTop = scroller.scrollHeight;
    });
  });

  onMount(() => {
    const el = scroller;
    if (!el) return;
    const onScroll = () => {
      stick = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    onCleanup(() => el.removeEventListener("scroll", onScroll));
  });

  const gate = () => snap()?.gate;

  return (
    <div class="ag-chat">
      <div class="ag-scroll" ref={scroller} role="log" aria-live="polite" aria-label={t("agents.conversation")}>
        <Show when={tl()?.status === "error"}>
          <p class="ag-note" role="alert">{tl()?.error}</p>
        </Show>
        <Show when={tl()?.status === "loading" && entries().length === 0}>
          <p class="ag-muted" role="status">{t("agents.loading")}</p>
        </Show>
        <Show when={tl()?.status === "ready" && entries().length === 0}>
          <p class="ag-muted">{t("agents.emptyChat")}</p>
        </Show>
        <TimelineView entries={entries()} />
        <Show when={snap()?.status === "error" && snap()?.lastError}>
          <p class="ag-error" role="alert">{snap()!.lastError}</p>
        </Show>
        <PermissionList agentId={props.id} />
        <Show when={gate()}>
          {(g) => (
            <section class="ag-gate" data-verdict={g().verdict} aria-label={t("agents.gate")}>
              <h3>{t("agents.gate")}: {t(`agents.gate_${g().verdict}`)}</h3>
              <Show when={g().tampered}>
                <p class="ag-error">{t("agents.gateTampered")}</p>
              </Show>
              <Show when={(g().details ?? []).length > 0}>
                <ul>{(g().details ?? []).map((d) => <li>{d}</li>)}</ul>
              </Show>
              <Show when={g().proofCommand}>
                <p class="ag-muted">{t("agents.gateProof")}: <code>{g().proofCommand}</code></p>
              </Show>
            </section>
          )}
        </Show>
      </div>
      <Composer id={props.id} />
    </div>
  );
}

/**
 * The prompt index (phase 6, GET /{id}/prompts): a menu in the detail header that jumps to one of the owner's
 * messages in the conversation. Hidden where the route is not deployed, and with an empty conversation.
 */
export function PromptsMenu(props: { id: string }) {
  const [prompts, setPrompts] = createSignal<PromptEntry[] | null>(null);
  const [open, setOpen] = createSignal(false);
  createEffect(
    on(
      () => props.id,
      () => {
        setPrompts(null);
        setOpen(false);
      },
    ),
  );

  // Probe the route once there is a conversation, so the button is not shown where it would answer 404.
  createEffect(() => {
    if (agentFeatures.prompts === undefined && (agentsState.timelines[props.id]?.rows.length ?? 0) > 0) {
      detect("prompts", () => agentsApi.prompts(props.id))
        .then((list) => list && setPrompts(list))
        .catch(() => undefined);
    }
  });

  async function toggle() {
    if (open()) return setOpen(false);
    setOpen(true);
    if (prompts() !== null) return;
    try {
      const list = await detect("prompts", () => agentsApi.prompts(props.id));
      setPrompts(list ?? []);
    } catch {
      setPrompts([]);
    }
  }

  function jump(seq: number) {
    setOpen(false);
    const scroller = document.querySelector<HTMLElement>(".ag-scroll");
    const target = [...(scroller?.querySelectorAll<HTMLElement>("[data-seq]") ?? [])].find((n) => Number(n.dataset.seq) >= seq);
    target?.scrollIntoView?.({ block: "start" });
  }

  return (
    <Show when={agentFeatures.prompts !== false && (agentsState.timelines[props.id]?.rows.length ?? 0) > 0}>
      <div class="ag-prompts">
        <button type="button" class="ag-btn ag-btn--small" aria-expanded={open()} onClick={() => void toggle()}>
          {t("agents.prompts")}
        </button>
        <Show when={open()}>
          <ul class="ag-prompts__list" aria-label={t("agents.prompts")}>
            <Show when={(prompts() ?? []).length > 0} fallback={<li class="ag-muted">{prompts() === null ? t("agents.loading") : t("agents.noPrompts")}</li>}>
              <For each={prompts() ?? []}>
                {(p) => (
                  <li>
                    <button type="button" onClick={() => jump(p.seq)}>{p.preview}</button>
                  </li>
                )}
              </For>
            </Show>
          </ul>
        </Show>
      </div>
    </Show>
  );
}
