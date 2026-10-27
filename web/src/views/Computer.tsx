import { A } from "@solidjs/router";
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { toApiError } from "../api/client";
import {
  computerApi,
  releaseOnExit,
  toScreen,
  type AuditEntry,
  type ControlState,
  type InputEvent,
  type Sandbox,
  type View,
} from "../api/computer";
import { BackIcon } from "../components/Icons";
import { t } from "../i18n";
import { readString, writeString } from "../lib/storage";
import "../styles/computer.css";

// Lucina's computer (Plugins → Lucina's computer): her Linux desktop and Android sandbox, live.
//
// "Live" embeds Cua's own viewer (H.264 over a WebSocket, through secretary-server and Lucina) in a
// sandboxed iframe: watch-only until the owner takes over, then a control ticket with mouse,
// keyboard and text clipboard. "Simple" polls a screenshot and sends clicks, scrolls, text and keys
// as input batches: for a weak connection or a browser without WebCodecs.
//
// Who drives is Lucina's: taking over stops her running task and refuses her and the agents' next
// steps until the owner gives it back. The takeover is a 45 s lease this page renews every 15 s;
// leaving the page gives it back. Every takeover and input batch is logged on Lucina's side.

const HEARTBEAT_MS = 15_000;
const STATE_POLL_MS = 3_000;
const SHOT_POLL_MS = 1_500;
const KEYS: { label: string; key: string; modifiers?: ("ctrl" | "shift" | "alt" | "super")[] }[] = [
  { label: "Esc", key: "Escape" },
  { label: "Tab", key: "Tab" },
  { label: "⌫", key: "BackSpace" },
  { label: "↵", key: "Return" },
  { label: "←", key: "Left" },
  { label: "↑", key: "Up" },
  { label: "↓", key: "Down" },
  { label: "→", key: "Right" },
  { label: "Ctrl+L", key: "l", modifiers: ["ctrl"] },
];

/** `mcp:claude` → "Claude", `app` → "App". */
export function callerName(who: string): string {
  const name = who.replace(/^mcp:/, "").replace(/-/g, " ");
  return name.charAt(0).toUpperCase() + name.slice(1);
}

export function driverLabel(s: ControlState | null, mine: boolean): string {
  if (!s) return t("computer.loading");
  switch (s.driver) {
    case "owner":
      return mine ? t("computer.driverOwnerMine") : t("computer.driverOwnerOther", { device: s.owner?.device ?? "?" });
    case "lucina":
      return s.task && s.task.who !== "lucina"
        ? t("computer.driverAgent", { who: callerName(s.task.who) })
        : t("computer.driverLucina");
    case "agent":
      return t("computer.driverAgent", { who: callerName(s.lastAgent?.who ?? "agent") });
    default:
      return t("computer.driverNone");
  }
}

export function auditLine(e: AuditEntry): string {
  const device = e.device ?? "";
  switch (e.event) {
    case "take":
      return t("computer.evTake", { device });
    case "release":
      return t("computer.evRelease", { device });
    case "expired":
      return t("computer.evExpired", { device });
    case "input": {
      const b = e.batch;
      const what = b?.via === "live" ? `${b.bytes ?? 0} B` : (b?.kinds ?? []).join(", ");
      return `${t("computer.evInput", { device })} ${what}`.trim();
    }
    case "view_open":
      return t("computer.evViewOpen", { device, mode: e.mode === "control" ? t("computer.modeControl") : t("computer.modeWatch") });
    case "view_socket_open":
      return t("computer.evSocketOpen");
    case "view_socket_closed":
      return t("computer.evSocketClosed", { why: e.why ?? "" });
    default:
      return t("computer.evOther", { event: e.event });
  }
}

function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

export default function ComputerView() {
  const [sandbox, setSandbox] = createSignal<Sandbox>(readString("local", "computer.sandbox") === "android" ? "android" : "workspace");
  const [mode, setMode] = createSignal<"live" | "simple">(readString("local", "computer.mode") === "simple" ? "simple" : "live");
  const [state, setState] = createSignal<ControlState | null>(null);
  const [recent, setRecent] = createSignal<AuditEntry[]>([]);
  const [mine, setMine] = createSignal(false);
  const [view, setView] = createSignal<View | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [tick, setTick] = createSignal(Date.now());
  const [text, setText] = createSignal("");
  const [clip, setClip] = createSignal("");
  let renewTimer: number | undefined;
  let img: HTMLImageElement | undefined;

  const loadState = async () => {
    try {
      const r = await computerApi.control(sandbox());
      setState(r.state);
      setRecent(r.recent);
      setError(null);
      if (mine() && r.state.driver !== "owner") lost();
    } catch (e) {
      const err = toApiError(e);
      setError(err.status === 503 || err.lucinaOffline ? t("computer.unavailable") : err.message);
    }
  };

  /** Mint a view for the current sandbox and mode; renew it a minute before it expires. */
  const openView = async (kind: "watch" | "control") => {
    window.clearTimeout(renewTimer);
    if (mode() !== "live") {
      setView(null);
      return;
    }
    try {
      const v = await computerApi.view(sandbox(), kind);
      setView(v);
      renewTimer = window.setTimeout(() => void openView(kind), Math.max(v.ttlSecs - 60, 30) * 1000);
    } catch (e) {
      const err = toApiError(e);
      setView(null);
      setError(err.status === 503 || err.lucinaOffline ? t("computer.unavailable") : err.message);
    }
  };

  /** The lease ended elsewhere (expired, released from another device). */
  const lost = () => {
    setMine(false);
    setNotice(t("computer.takeoverEnded"));
    void openView("watch");
  };

  const takeOver = async () => {
    const s = state();
    if (s?.driver === "lucina" && s.task) {
      const msg = s.task.who === "lucina"
        ? t("computer.confirmTakeOver", { what: s.task.what })
        : t("computer.confirmTakeOverAgent", { who: callerName(s.task.who) });
      if (!window.confirm(msg)) return;
    } else if (s?.driver === "agent" && !window.confirm(t("computer.confirmTakeOverAgent", { who: callerName(s.lastAgent?.who ?? "agent") }))) {
      return;
    }
    setBusy(true);
    try {
      const r = await computerApi.take(sandbox());
      setState(r.state);
      setMine(true);
      setNotice(r.stoppedTask ? t("computer.stoppedTask", { what: r.stoppedTask }) : null);
      await openView("control");
      await loadState();
    } catch (e) {
      setError(toApiError(e).message);
    } finally {
      setBusy(false);
    }
  };

  const giveBack = async () => {
    setBusy(true);
    try {
      const r = await computerApi.release(sandbox());
      setState(r.state);
      setMine(false);
      setNotice(null);
      await openView("watch");
      await loadState();
    } catch (e) {
      setError(toApiError(e).message);
    } finally {
      setBusy(false);
    }
  };

  const send = async (events: InputEvent[]) => {
    if (!mine()) return;
    try {
      const r = await computerApi.input(sandbox(), events);
      if (r.error) setError(r.error.message);
      setTick(Date.now());
      return r;
    } catch (e) {
      const err = toApiError(e);
      if (err.code === "not_driving") lost();
      else setError(err.message);
    }
  };

  const onShotClick = (e: MouseEvent, button: "left" | "right") => {
    if (!mine() || !img) return;
    e.preventDefault();
    const p = toScreen(e, { width: img.clientWidth, height: img.clientHeight }, { width: img.naturalWidth || 1280, height: img.naturalHeight || 800 });
    void send([{ kind: "click", ...p, button }]);
  };

  let lastWheel = 0;
  const onShotWheel = (e: WheelEvent) => {
    if (!mine() || !img) return;
    e.preventDefault();
    if (Date.now() - lastWheel < 250) return;
    lastWheel = Date.now();
    const p = toScreen(e, { width: img.clientWidth, height: img.clientHeight }, { width: img.naturalWidth || 1280, height: img.naturalHeight || 800 });
    const direction = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? (e.deltaX > 0 ? "right" : "left") : e.deltaY > 0 ? "down" : "up";
    void send([{ kind: "scroll", ...p, direction, amount: 3 }]);
  };

  const readClipboard = async () => {
    const r = await send([{ kind: "clipboard_read" }]);
    const got = r?.outputs.find((o) => o.tool === "clipboard_read")?.text ?? "";
    if (!got.trim()) {
      setNotice(t("computer.clipboardEmpty"));
      return;
    }
    setClip(got);
    try {
      await navigator.clipboard.writeText(got);
      setNotice(t("computer.clipboardCopied"));
    } catch {
      // No clipboard permission: the text stays in the field to copy by hand.
    }
  };

  onMount(() => {
    void loadState();
    void openView("watch");
    const poll = window.setInterval(() => {
      if (!document.hidden) void loadState();
    }, STATE_POLL_MS);
    const shots = window.setInterval(() => {
      if (!document.hidden && mode() === "simple") setTick(Date.now());
    }, SHOT_POLL_MS);
    const beat = window.setInterval(() => {
      if (!mine()) return;
      computerApi.heartbeat(sandbox()).catch((e) => {
        if (toApiError(e).code === "not_driving") lost();
      });
    }, HEARTBEAT_MS);
    const exit = () => {
      if (mine()) releaseOnExit(sandbox());
    };
    window.addEventListener("pagehide", exit);
    onCleanup(() => {
      window.clearInterval(poll);
      window.clearInterval(shots);
      window.clearInterval(beat);
      window.clearTimeout(renewTimer);
      window.removeEventListener("pagehide", exit);
      exit();
    });
  });

  // Switching computer or view mode: give the old one back, then watch the new one.
  createEffect(
    on([sandbox, mode], ([sb, m], prev) => {
      if (!prev) return;
      const [oldSb] = prev;
      writeString("local", "computer.sandbox", sb);
      writeString("local", "computer.mode", m);
      if (mine() && oldSb !== sb) {
        void computerApi.release(oldSb).catch(() => undefined);
        setMine(false);
      }
      setState(null);
      void loadState();
      void openView(mine() ? "control" : "watch");
    }),
  );

  return (
    <section class="computer-page" aria-labelledby="computer-title">
      <A href="/plugins" class="computer-back">
        <BackIcon /> {t("plugins.title")}
      </A>
      <h1 id="computer-title">{t("computer.title")}</h1>

      <div class="computer-bar">
        <div class="computer-seg" role="group" aria-label={t("computer.sandboxes")}>
          <button type="button" aria-pressed={sandbox() === "workspace"} onClick={() => setSandbox("workspace")}>
            {t("computer.desktop")}
          </button>
          <button type="button" aria-pressed={sandbox() === "android"} onClick={() => setSandbox("android")}>
            {t("computer.android")}
          </button>
        </div>
        <div class="computer-seg" role="group" aria-label={t("computer.viewMode")}>
          <button type="button" aria-pressed={mode() === "live"} onClick={() => setMode("live")}>
            {t("computer.live")}
          </button>
          <button type="button" aria-pressed={mode() === "simple"} onClick={() => setMode("simple")}>
            {t("computer.simple")}
          </button>
        </div>
      </div>

      <div class="computer-status" data-driver={state()?.driver ?? "loading"} data-mine={mine() ? "yes" : "no"}>
        <span class="computer-dot" aria-hidden="true" />
        <div class="computer-status-text">
          <strong role="status">{driverLabel(state(), mine())}</strong>
          <Show when={state()?.task}>{(task) => <span class="computer-muted">{t("computer.working", { what: task().what })}</span>}</Show>
          <Show when={!mine()}>
            <span class="computer-muted">{t("computer.watchOnly")}</span>
          </Show>
        </div>
        <Show
          when={mine()}
          fallback={
            <button type="button" class="btn btn--primary" disabled={busy() || !state()} onClick={() => void takeOver()}>
              {t("computer.takeOver")}
            </button>
          }
        >
          <button type="button" class="btn" disabled={busy()} onClick={() => void giveBack()}>
            {t("computer.giveBack")}
          </button>
        </Show>
      </div>

      <Show when={notice()}>
        <p class="computer-notice" role="status">{notice()}</p>
      </Show>
      <Show when={error()}>
        <p class="computer-error" role="alert">{error()}</p>
      </Show>

      <div class="computer-screen" data-sandbox={sandbox()}>
        <Show
          when={mode() === "live"}
          fallback={
            <img
              ref={img}
              src={computerApi.screenshotUrl(sandbox(), tick())}
              alt={t("computer.screen")}
              width="1280"
              height="800"
              data-clickable={mine() ? "yes" : "no"}
              onClick={(e) => onShotClick(e, "left")}
              onContextMenu={(e) => onShotClick(e, "right")}
              onWheel={onShotWheel}
            />
          }
        >
          <Show when={view()} fallback={<p class="computer-loading">{t("computer.loading")}</p>}>
            {(v) => (
              <iframe
                title={t("computer.viewer")}
                src={v().viewUrl}
                sandbox="allow-scripts allow-pointer-lock"
                allow="fullscreen; clipboard-read; clipboard-write; autoplay"
                referrerpolicy="no-referrer"
              />
            )}
          </Show>
        </Show>
      </div>

      <Show when={mine()}>
        <div class="computer-card computer-input">
          <form
            class="computer-type"
            onSubmit={(e) => {
              e.preventDefault();
              const s = text();
              if (!s) return;
              setText("");
              void send([{ kind: "type", text: s }]);
            }}
          >
            <label class="sr-only" for="computer-type">{t("computer.typeLabel")}</label>
            <input id="computer-type" class="text-input" value={text()} placeholder={t("computer.typePlaceholder")} autocomplete="off" onInput={(e) => setText(e.currentTarget.value)} />
            <button type="submit" class="btn">{t("computer.send")}</button>
          </form>
          <div class="computer-keys" role="group" aria-label={t("computer.keys")}>
            <For each={KEYS}>
              {(k) => (
                <button type="button" class="computer-key" onClick={() => void send([{ kind: "key", key: k.key, modifiers: k.modifiers }])}>
                  {k.label}
                </button>
              )}
            </For>
          </div>
          <div class="computer-clip">
            <label for="computer-clip">{t("computer.clipboardLabel")}</label>
            <textarea id="computer-clip" class="text-input" rows="2" value={clip()} placeholder={t("computer.clipboardPlaceholder")} onInput={(e) => setClip(e.currentTarget.value)} />
            <div class="computer-clip-actions">
              <button type="button" class="btn" disabled={!clip()} onClick={() => void send([{ kind: "clipboard_write", text: clip() }])}>
                {t("computer.clipboardSend")}
              </button>
              <button type="button" class="btn" onClick={() => void readClipboard()}>
                {t("computer.clipboardRead")}
              </button>
            </div>
          </div>
        </div>
      </Show>

      <Show when={recent().length > 0}>
        <h2>{t("computer.activity")}</h2>
        <ul class="computer-card computer-log">
          <For each={recent().slice(0, 10)}>
            {(e) => (
              <li>
                <span class="computer-muted">{clock(e.at)}</span> {auditLine(e)}
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}
