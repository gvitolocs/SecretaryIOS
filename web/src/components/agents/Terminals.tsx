import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, Show } from "solid-js";
import { agentsApi, type Script } from "../../api/agents";
import { toApiError } from "../../api/client";
import { t } from "../../i18n";
import { ansiToText } from "../../lib/agentsTimeline";
import { agentsState, loadScripts, loadTerminals } from "../../state/agents";

// Terminals (phase 5). There is no WebSocket through p1: output is SSE (`snapshot` first, then base64 `output`
// events, then `exit`), input is a POST. xterm.js is not in the web app, so this is a text view of the output (the
// ANSI codes are dropped) with an input line and the usual control keys.

const MAX_RAW = 120_000;

function decode(b64: string, dec: TextDecoder): string {
  try {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return dec.decode(bytes, { stream: true });
  } catch {
    return "";
  }
}

const KEYS: { key: string; label: () => string }[] = [
  { key: "C-c", label: () => "Ctrl-C" },
  { key: "C-d", label: () => "Ctrl-D" },
  { key: "Tab", label: () => "Tab" },
  { key: "Escape", label: () => "Esc" },
  { key: "Up", label: () => "↑" },
  { key: "Down", label: () => "↓" },
  { key: "Enter", label: () => "Enter" },
];

function TerminalOutput(props: { id: string }) {
  const [raw, setRaw] = createSignal("");
  const [exit, setExit] = createSignal<number | null | undefined>(undefined);
  const [live, setLive] = createSignal(false);
  let pre: HTMLPreElement | undefined;

  createEffect(
    on(
      () => props.id,
      (id) => {
        setRaw("");
        setExit(undefined);
        const dec = new TextDecoder();
        const es = new EventSource(agentsApi.terminalStreamUrl(id), { withCredentials: true });
        es.onopen = () => setLive(true);
        es.onerror = () => setLive(false);
        es.addEventListener("snapshot", (m) => {
          try {
            const d = JSON.parse((m as MessageEvent<string>).data) as { data?: string };
            setRaw(d.data ?? "");
          } catch {
            // ignore a malformed frame
          }
        });
        es.addEventListener("output", (m) => {
          try {
            const d = JSON.parse((m as MessageEvent<string>).data) as { b64?: string };
            const text = d.b64 ? decode(d.b64, dec) : "";
            if (text) setRaw((r) => (r + text).slice(-MAX_RAW));
          } catch {
            // ignore
          }
        });
        es.addEventListener("exit", (m) => {
          try {
            setExit((JSON.parse((m as MessageEvent<string>).data) as { exitCode?: number | null }).exitCode ?? null);
          } catch {
            setExit(null);
          }
          es.close();
          setLive(false);
        });
        onCleanup(() => es.close());
      },
    ),
  );

  const text = createMemo(() => ansiToText(raw()));
  createEffect(() => {
    text();
    queueMicrotask(() => {
      if (pre) pre.scrollTop = pre.scrollHeight;
    });
  });

  return (
    <>
      <pre class="ag-code ag-term" ref={pre} tabindex="0" aria-label={t("agents.terminalOutput")} aria-live="off">{text()}</pre>
      <p class="ag-muted ag-term__state" role="status">
        {exit() !== undefined ? t("agents.terminalExited", { code: exit() === null ? "?" : String(exit()) }) : live() ? t("agents.terminalLive") : t("agents.terminalConnecting")}
      </p>
    </>
  );
}

export function TerminalPane(props: { workspaceId: string }) {
  const [sel, setSel] = createSignal<string | null>(null);
  const [text, setText] = createSignal("");
  const [note, setNote] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const terminals = createMemo(() => Object.values(agentsState.terminals).filter((x) => x.workspaceId === props.workspaceId));
  createEffect(() => {
    if (!sel() || !terminals().some((x) => x.id === sel())) setSel(terminals()[0]?.id ?? null);
  });
  createEffect(on(() => props.workspaceId, () => void loadTerminals().catch(() => undefined)));

  async function run<T>(fn: () => Promise<T>): Promise<T | undefined> {
    setNote(null);
    setBusy(true);
    try {
      return await fn();
    } catch (e) {
      setNote(toApiError(e).message);
      return undefined;
    } finally {
      setBusy(false);
    }
  }

  async function create() {
    const x = await run(() => agentsApi.createTerminal(props.workspaceId));
    if (x) setSel(x.id);
  }
  async function send(e: Event) {
    e.preventDefault();
    const id = sel();
    if (!id) return;
    const line = text();
    setText("");
    await run(() => agentsApi.terminalInput(id, { keys: line ? [line, "Enter"] : "Enter" }));
  }
  const key = (k: string) => {
    const id = sel();
    if (id) void run(() => agentsApi.terminalInput(id, { keys: k }));
  };

  return (
    <div class="ag-termpane">
      <div class="ag-toolbar">
        <Show when={terminals().length > 0}>
          <select aria-label={t("agents.terminal")} value={sel() ?? ""} onChange={(e) => setSel(e.currentTarget.value)}>
            <For each={terminals()}>{(x) => <option value={x.id}>{`${x.title || x.name}${x.status === "exited" ? ` (${t("agents.exited")})` : ""}`}</option>}</For>
          </select>
        </Show>
        <button type="button" class="ag-btn ag-btn--small" disabled={busy()} onClick={() => void create()}>{t("agents.newTerminal")}</button>
        <Show when={sel()}>
          <button type="button" class="ag-btn ag-btn--small" disabled={busy()} onClick={() => void run(() => agentsApi.killTerminal(sel()!))}>{t("agents.closeTerminal")}</button>
        </Show>
      </div>
      <Show when={note()}>
        <p class="ag-note" role="alert">{note()}</p>
      </Show>
      <Show when={sel()} fallback={<p class="ag-muted">{t("agents.noTerminals")}</p>}>
        <TerminalOutput id={sel()!} />
        <form class="ag-term__input" onSubmit={send}>
          <input type="text" value={text()} autocapitalize="none" autocomplete="off" spellcheck={false} aria-label={t("agents.terminalInput")} placeholder={t("agents.terminalInput")} onInput={(e) => setText(e.currentTarget.value)} />
          <button type="submit" class="ag-btn ag-btn--primary" disabled={busy()}>{t("agents.send")}</button>
        </form>
        <div class="ag-keys" role="group" aria-label={t("agents.keys")}>
          <For each={KEYS}>{(k) => <button type="button" class="ag-btn ag-btn--small" onClick={() => key(k.key)}>{k.label()}</button>}</For>
        </div>
      </Show>
    </div>
  );
}

/** Scripts and services of a workspace (paseo.json / .lucina/agents.toml): status, start and stop, open a service. */
export function ScriptsPane(props: { workspaceId: string }) {
  const [problem, setProblem] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [services] = createResource(
    () => props.workspaceId,
    (id) => agentsApi.services(id).catch(() => []),
  );
  createEffect(
    on(
      () => props.workspaceId,
      (id) => {
        setProblem(null);
        void loadScripts(id).catch((e) => setProblem(toApiError(e).message));
      },
    ),
  );
  const scripts = (): Script[] => agentsState.scripts[props.workspaceId] ?? [];
  const detected = () => (services() ?? []).filter((s) => s.kind === "detected");

  async function toggle(s: Script) {
    setBusy(s.scriptName);
    setProblem(null);
    try {
      await agentsApi.scriptAction(props.workspaceId, s.scriptName, s.lifecycle === "running" ? "stop" : "start");
      await loadScripts(props.workspaceId);
    } catch (e) {
      setProblem(toApiError(e).message);
    } finally {
      setBusy(null);
    }
  }

  return (
    <div class="ag-scripts">
      <Show when={problem()}>
        <p class="ag-note" role="alert">{problem()}</p>
      </Show>
      <Show when={scripts().length > 0} fallback={<p class="ag-muted">{t("agents.noScripts")}</p>}>
        <ul class="ag-cards">
          <For each={scripts()}>
            {(s) => (
              <li class="ag-card" data-state={s.lifecycle}>
                <div class="ag-card__main">
                  <strong>{s.scriptName}</strong>
                  <span class="ag-tag">{s.type === "service" ? t("agents.service") : t("agents.script")}</span>
                  <span class="ag-tag" data-state={s.lifecycle}>{s.lifecycle === "running" ? t("agents.running") : t("agents.stopped")}</span>
                  <Show when={s.health}><span class="ag-tag" data-health={s.health}>{s.health === "healthy" ? t("agents.healthy") : t("agents.unhealthy")}</span></Show>
                  <Show when={s.command}><code class="ag-card__cmd">{s.command}</code></Show>
                </div>
                <div class="ag-card__actions">
                  <Show when={s.servicePath && s.lifecycle === "running"}>
                    <a class="ag-btn ag-btn--small" href={s.servicePath!} target="_blank" rel="noopener noreferrer">{t("agents.openService")}</a>
                  </Show>
                  <button type="button" class="ag-btn ag-btn--small" disabled={busy() === s.scriptName} onClick={() => void toggle(s)}>
                    {s.lifecycle === "running" ? t("agents.stopScript") : t("agents.startScript")}
                  </button>
                </div>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={detected().length > 0}>
        <h3 class="ag-h3">{t("agents.detectedPorts")}</h3>
        <ul class="ag-cards">
          <For each={detected()}>
            {(s) => (
              <li class="ag-card">
                <div class="ag-card__main"><strong>{s.name}</strong><span class="ag-tag">:{s.port}</span></div>
                <div class="ag-card__actions"><a class="ag-btn ag-btn--small" href={s.path} target="_blank" rel="noopener noreferrer">{t("agents.openService")}</a></div>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}
