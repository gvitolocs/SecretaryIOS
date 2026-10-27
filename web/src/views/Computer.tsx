import { A } from "@solidjs/router";
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { toApiError } from "../api/client";
import {
  computerApi,
  probeLan,
  releaseOnExit,
  toScreen,
  viewerAsset,
  type AuditEntry,
  type ControlState,
  type InputEvent,
  type Sandbox,
  type View,
} from "../api/computer";
import { BackIcon } from "../components/Icons";
import { t } from "../i18n";
import { readString, writeString } from "../lib/storage";
import { createTouchMouse } from "../lib/touch";
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
//
// On a phone (the owner's iPhone) this is the whole point, so:
//
// - The viewer's assets are immutable and cached; only index.html and the socket carry a ticket.
//   A takeover or a renewal asks the server for a ticket on the SAME view id and hands it to the
//   running viewer with postMessage (`viewer/lucina-shim.js` applies it and re-attaches the media
//   session), so the screen does not reload.
// - The keyboard is never raised by a tap: the shim keeps Cua's hidden textarea read-only until
//   the "Keyboard" button asks for it.
// - Touch is a mouse: tap, long-press, drag, two-finger scroll/tap, pinch (see `lib/touch.ts` for
//   Simple mode; the shim does the same inside the live viewer).
// - "Full screen" uses the Fullscreen API where it exists and a fixed, scroll-locked, safe-area
//   aware screen with a floating mini bar on iPhone, which does not implement it for elements.

const HEARTBEAT_MS = 15_000;
const STATE_POLL_MS = 3_000;
const SHOT_POLL_MS = 1_500;
/** A device is not asleep for the whole lease, so renew a little before it ends. */
const RENEW_MARGIN_SECS = 120;
// `key`/`modifiers` are the driver's (X11) names for the screenshot mode; `live` is the same chord in
// the Cua viewer's own key names, sent over the live socket like a touch.
const KEYS: { label: string; key: string; modifiers?: ("ctrl" | "shift" | "alt" | "super")[]; live: string[] }[] = [
  { label: "Esc", key: "Escape", live: ["escape"] },
  { label: "Tab", key: "Tab", live: ["tab"] },
  { label: "⌫", key: "BackSpace", live: ["backspace"] },
  { label: "↵", key: "Return", live: ["enter"] },
  { label: "←", key: "Left", live: ["arrowleft"] },
  { label: "↑", key: "Up", live: ["arrowup"] },
  { label: "↓", key: "Down", live: ["arrowdown"] },
  { label: "→", key: "Right", live: ["arrowright"] },
  { label: "Ctrl+L", key: "l", modifiers: ["ctrl"], live: ["control", "l"] },
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

/**
 * The part of an `<img>` that actually shows the picture when `object-fit: contain`
 * letterboxes it (full screen in the wrong aspect ratio). Client coordinates in, so a CSS
 * transform on the image is accounted for: the rect is the visual one.
 */
export function contentRect(
  box: { left: number; top: number; width: number; height: number },
  natural: { width: number; height: number },
): { left: number; top: number; width: number; height: number } {
  if (natural.width <= 0 || natural.height <= 0) return box;
  const scale = Math.min(box.width / natural.width, box.height / natural.height);
  const width = natural.width * scale;
  const height = natural.height * scale;
  return { left: box.left + (box.width - width) / 2, top: box.top + (box.height - height) / 2, width, height };
}

/** A client point on a picture that is fitted and possibly letterboxed. */
export function pointIn(
  client: { clientX: number; clientY: number },
  box: { left: number; top: number; width: number; height: number },
  natural: { width: number; height: number },
): { x: number; y: number } {
  const shown = contentRect(box, natural);
  const clamp = (v: number, max: number) => Math.min(Math.max(Math.round(v), 0), Math.max(max - 1, 0));
  return {
    x: clamp(((client.clientX - shown.left) * natural.width) / Math.max(shown.width, 1), natural.width),
    y: clamp(((client.clientY - shown.top) * natural.height) / Math.max(shown.height, 1), natural.height),
  };
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
  const [full, setFull] = createSignal(false);
  const [keyboard, setKeyboard] = createSignal(false);
  const [zoom, setZoom] = createSignal({ scale: 1, dx: 0, dy: 0 });
  const [via, setVia] = createSignal<"lan" | "remote" | null>(null);
  let lanSeen: { url: string; ok: boolean; at: number } | null = null;
  const [mark, setMark] = createSignal<{ x: number; y: number; button: "left" | "right"; n: number } | null>(null);
  let renewTimer: number | undefined;
  let markTimer: number | undefined;
  let preload: HTMLLinkElement | undefined;
  let img: HTMLImageElement | undefined;
  let screen: HTMLDivElement | undefined;
  let frame: HTMLIFrameElement | undefined;
  let touchLast = 0;
  let pinchBase: { scale: number; dx: number; dy: number } | null = null;

  /** The mode the current view is in: what the running viewer is showing. */
  const viewMode = () => (mine() ? "control" : "watch");

  const loadState = async () => {
    try {
      const r = await computerApi.control(sandbox());
      setState(r.state);
      setRecent(r.recent);
      setError(null);
      if (mine() && r.state.driver !== "owner") lost();
      preloadViewer(r.viewer?.assets);
    } catch (e) {
      const err = toApiError(e);
      setError(err.status === 503 || err.lucinaOffline ? t("computer.unavailable") : err.message);
    }
  };

  /**
   * Start fetching the 258 KB viewer now, while the view is still being minted. It is immutable,
   * so this is the first visit's cost only; the preload shares the cache partition with the
   * opaque-origin iframe (same top-level site), so the iframe does not fetch it again.
   */
  const preloadViewer = (version?: string) => {
    if (!version || mode() !== "live" || preload) return;
    if (typeof document === "undefined") return;
    const link = document.createElement("link");
    link.rel = "preload";
    link.as = "script";
    link.href = viewerAsset(version, "viewer.js");
    link.dataset.lucina = "viewer-preload";
    document.head.appendChild(link);
    preload = link;
  };

  const post = (msg: unknown) => {
    try {
      frame?.contentWindow?.postMessage(msg, "*");
    } catch {
      // The frame went away (sandbox switch, navigation).
    }
  };

  /** Mint a view for the current sandbox and mode; renew it a couple of minutes before it expires. */
  const openView = async (kind: "watch" | "control") => {
    window.clearTimeout(renewTimer);
    if (mode() !== "live") {
      setView(null);
      return;
    }
    try {
      const v = await computerApi.view(sandbox(), kind);
      // At home, the same view straight from nezopt when the door answers; else through the server.
      const lan = v.lan?.url && (await lanReachable(v.lan.url)) ? v.lan.url : null;
      setVia(lan ? "lan" : "remote");
      setView({ ...v, viewUrl: lan ? `${lan}${v.viewUrl}` : v.viewUrl });
      setKeyboard(false);
      setZoom({ scale: 1, dx: 0, dy: 0 });
      scheduleRenew(v.ttlSecs);
    } catch (e) {
      const err = toApiError(e);
      setView(null);
      setError(err.status === 503 || err.lucinaOffline ? t("computer.unavailable") : err.message);
    }
  };

  /** The probe's answer, remembered for a minute (a renewal or a sandbox switch does not ask again). */
  const lanReachable = async (url: string) => {
    if (lanSeen && lanSeen.url === url && Date.now() - lanSeen.at < 60_000) return lanSeen.ok;
    const ok = await probeLan(url);
    lanSeen = { url, ok, at: Date.now() };
    return ok;
  };

  const scheduleRenew = (ttlSecs: number) => {
    window.clearTimeout(renewTimer);
    renewTimer = window.setTimeout(() => void refreshTicket(viewMode()), Math.max(ttlSecs - RENEW_MARGIN_SECS, 30) * 1000);
  };

  /**
   * A ticket for the same view: a renewal, or the watch↔control switch of a takeover. The running
   * viewer takes it (and re-attaches its media session when the mode changed), so nothing reloads.
   * Only when the server cannot re-issue (the view is gone) does it fall back to a new view.
   */
  const refreshTicket = async (kind: "watch" | "control") => {
    const v = view();
    if (!v) {
      await openView(kind);
      return;
    }
    try {
      const ticket = await computerApi.viewTicket(v.viewId, kind);
      post({ type: "lucina:ticket", ticket: ticket.ticket, mode: ticket.mode });
      scheduleRenew(ticket.ttlSecs);
    } catch {
      await openView(kind);
    }
  };

  /** The lease ended elsewhere (expired, released from another device). */
  const lost = () => {
    setMine(false);
    setKeyboard(false);
    setNotice(t("computer.takeoverEnded"));
    void refreshTicket("watch");
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
      await refreshTicket("control");
      await loadState();
    } catch (e) {
      setError(toApiError(e).message);
    } finally {
      setBusy(false);
    }
  };

  const giveBack = async () => {
    setBusy(true);
    setKeyboard(false);
    try {
      // Switch the running viewer to watching first: a control view dies with the lease, so after
      // the release there would be nothing left to re-issue and the viewer would have to reload.
      await refreshTicket("watch");
      const r = await computerApi.release(sandbox());
      setState(r.state);
      setMine(false);
      setNotice(null);
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

  const showMark = (at: { clientX: number; clientY: number }, button: "left" | "right") => {
    const box = screen?.getBoundingClientRect();
    setMark({ x: at.clientX - (box?.left ?? 0), y: at.clientY - (box?.top ?? 0), button, n: Date.now() });
    window.clearTimeout(markTimer);
    markTimer = window.setTimeout(() => setMark(null), 550);
  };

  // ---------------------------------------------------------------- the mouse

  const naturalSize = () => ({ width: img?.naturalWidth || 1280, height: img?.naturalHeight || 800 });

  const onShotClick = (e: MouseEvent, button: "left" | "right") => {
    if (!mine() || !img) return;
    // A touch tap is handled (and marked) by the pointer path; ignore its synthetic click.
    if (Date.now() - touchLast < 900) return;
    e.preventDefault();
    const box = img.getBoundingClientRect();
    void send([{ kind: "click", ...pointIn(e, box, naturalSize()), button }]);
    showMark({ clientX: e.clientX, clientY: e.clientY }, button);
  };

  let lastWheel = 0;
  const onShotWheel = (e: WheelEvent) => {
    if (!mine() || !img) return;
    e.preventDefault();
    if (Date.now() - lastWheel < 250) return;
    lastWheel = Date.now();
    const p = toScreen(e, { width: img.clientWidth, height: img.clientHeight }, naturalSize());
    const direction = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? (e.deltaX > 0 ? "right" : "left") : e.deltaY > 0 ? "down" : "up";
    void send([{ kind: "scroll", ...p, direction, amount: 3 }]);
  };

  // ---------------------------------------------------------------- touch

  const touch = createTouchMouse(
    {
      click: (at, button) => void send([{ kind: "click", ...at, button }]),
      down: (at) => void send([{ kind: "down", ...at, button: "left" }]),
      move: (at) => void send([{ kind: "move", ...at }]),
      up: (at) => void send([{ kind: "up", ...at, button: "left" }]),
      scroll: (at, direction, amount) => void send([{ kind: "scroll", ...at, direction, amount }]),
    },
    {
      onMark: showMark,
      onZoom: ({ scale, dx, dy }) => {
        const base = pinchBase ?? zoom();
        if (!pinchBase) pinchBase = base;
        setZoom({ scale: Math.min(4, Math.max(1, base.scale * scale)), dx: base.dx + dx, dy: base.dy + dy });
      },
    },
  );

  const at = (e: PointerEvent) => {
    const box = img?.getBoundingClientRect() ?? { left: 0, top: 0, width: 1, height: 1 };
    return pointIn(e, box, naturalSize());
  };

  const onShotDown = (e: PointerEvent) => {
    if (!mine() || !img) return;
    if (e.pointerType && e.pointerType !== "touch") return;
    touchLast = Date.now();
    pinchBase = null;
    (e.currentTarget as Element | null)?.setPointerCapture?.(e.pointerId);
    touch.down(e, at(e));
  };
  const onShotMove = (e: PointerEvent) => {
    if (!mine() || !img || (e.pointerType && e.pointerType !== "touch")) return;
    touchLast = Date.now();
    touch.move(e, at(e));
  };
  const onShotUp = (e: PointerEvent) => {
    if (!mine() || !img || (e.pointerType && e.pointerType !== "touch")) return;
    touchLast = Date.now();
    touch.up(e, at(e));
  };
  const onShotCancel = () => {
    touch.cancel();
    pinchBase = null;
  };

  const resetZoom = () => {
    setZoom({ scale: 1, dx: 0, dy: 0 });
    pinchBase = null;
  };

  /** A key button: through the running viewer in Live mode (instant), else as an input batch. */
  const pressKey = (k: (typeof KEYS)[number]) => {
    if (!mine()) return;
    if (mode() === "live" && view() && frame) {
      post({ type: "lucina:keys", keys: k.live });
      return;
    }
    void send([{ kind: "key", key: k.key, modifiers: k.modifiers }]);
  };

  // ---------------------------------------------------------------- keyboard & full screen

  /** Ask the viewer for its keyboard; it is the only thing that may raise the iOS one. */
  const toggleKeyboard = () => {
    if (!mine()) return;
    const next = !keyboard();
    setKeyboard(next);
    post({ type: "lucina:keyboard", open: next });
  };

  const closeKeyboard = () => {
    if (!keyboard()) return;
    setKeyboard(false);
    post({ type: "lucina:keyboard", open: false });
  };

  const exitFull = () => {
    if (document.fullscreenElement) {
      void document.exitFullscreen().catch(() => undefined);
      return;
    }
    setFull(false);
  };

  const toggleFull = () => {
    if (document.fullscreenElement) {
      exitFull();
      return;
    }
    const el = screen;
    if (el && typeof el.requestFullscreen === "function" && document.fullscreenEnabled) {
      el.requestFullscreen()
        .then(() => setFull(true))
        .catch(() => setFull(!full()));
      return;
    }
    // iPhone Safari cannot full-screen an element: a fixed, scroll-locked screen instead.
    setFull(!full());
  };

  createEffect(
    on(full, (on) => {
      if (typeof document === "undefined") return;
      document.documentElement.classList.toggle("computer-fullscreen", on);
      if (!on) closeKeyboard();
    }),
  );

  createEffect(
    on(full, (on) => {
      if (typeof document === "undefined" || typeof window === "undefined") return;
      if (!on) return;
      const escape = (e: KeyboardEvent) => {
        if (e.key === "Escape") {
          e.preventDefault();
          exitFull();
        }
      };
      window.addEventListener("keydown", escape);
      onCleanup(() => window.removeEventListener("keydown", escape));
    }),
  );

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
    void openView(viewMode());
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
    // The first frame, from the viewer itself (its own status dot).
    const onMessage = (e: MessageEvent) => {
      // Only the viewer frame speaks this protocol (its origin is opaque, so check the window).
      if (!frame || e.source !== frame.contentWindow) return;
      const data = e.data as { type?: string; state?: string } | null;
      if (!data || typeof data.type !== "string") return;
      if (data.type === "lucina:state" && data.state === "failed") {
        setError(t("computer.unavailable"));
      }
    };
    const onFullscreen = () => setFull(!!document.fullscreenElement);
    const exit = () => {
      if (mine()) releaseOnExit(sandbox());
    };
    window.addEventListener("message", onMessage);
    window.addEventListener("pagehide", exit);
    document.addEventListener("fullscreenchange", onFullscreen);
    onCleanup(() => {
      window.clearInterval(poll);
      window.clearInterval(shots);
      window.clearInterval(beat);
      window.clearTimeout(renewTimer);
      window.clearTimeout(markTimer);
      window.removeEventListener("message", onMessage);
      window.removeEventListener("pagehide", exit);
      document.removeEventListener("fullscreenchange", onFullscreen);
      document.documentElement.classList.remove("computer-fullscreen");
      preload?.remove();
      preload = undefined;
      exit();
    });
  });

  // Switching computer or view mode: give the old one back, then watch the new one.
  createEffect(
    on([sandbox, mode], ([sb, m], prev) => {
      if (!prev) return;
      const [oldSb, oldMode] = prev;
      writeString("local", "computer.sandbox", sb);
      writeString("local", "computer.mode", m);
      if (mine() && oldSb !== sb) {
        void computerApi.release(oldSb).catch(() => undefined);
        setMine(false);
      }
      if (oldMode !== m) {
        preload?.remove();
        preload = undefined;
        resetZoom();
        closeKeyboard();
      }
      setState(null);
      void loadState();
      void openView(mine() ? "control" : "watch");
    }),
  );

  return (
    <section class="computer-page" aria-labelledby="computer-title" data-full={full() ? "yes" : "no"}>
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
          <Show when={mode() === "live" && view() && via()}>
            {(how) => (
              <span class="computer-via" data-via={how()}>
                {how() === "lan" ? t("computer.viaLan") : t("computer.viaRemote")}
              </span>
            )}
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

      <div
        class="computer-screen"
        classList={{ "computer-screen--full": full() }}
        data-sandbox={sandbox()}
        ref={screen}
      >
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
              style={{
                transform: zoom().scale === 1 ? undefined : `translate(${zoom().dx}px, ${zoom().dy}px) scale(${zoom().scale})`,
              }}
              onClick={(e) => onShotClick(e, "left")}
              onContextMenu={(e) => onShotClick(e, "right")}
              onWheel={onShotWheel}
              onPointerDown={onShotDown}
              onPointerMove={onShotMove}
              onPointerUp={onShotUp}
              onPointerCancel={onShotCancel}
            />
          }
        >
          <Show when={view()} fallback={<p class="computer-loading">{t("computer.loading")}</p>}>
            {(v) => (
              <iframe
                ref={frame}
                title={t("computer.viewer")}
                src={v().viewUrl}
                sandbox="allow-scripts allow-pointer-lock"
                allow="fullscreen; clipboard-read; clipboard-write; autoplay"
                referrerpolicy="no-referrer"
              />
            )}
          </Show>
        </Show>
        <Show when={mark()}>
          {(m) => (
            <span
              class="computer-mark"
              data-button={m().button}
              style={{ left: `${m().x}px`, top: `${m().y}px` }}
              aria-hidden="true"
            />
          )}
        </Show>
      </div>

      <div class="computer-bar computer-actions">
        <button type="button" class="btn" onClick={() => toggleFull()} aria-pressed={full()}>
          {full() ? t("computer.exitFull") : t("computer.fullScreen")}
        </button>
        <Show when={mine()}>
          <button type="button" class="btn" aria-pressed={keyboard()} onClick={() => toggleKeyboard()}>
            {t("computer.keyboard")}
          </button>
        </Show>
        <Show when={zoom().scale > 1.05}>
          <button type="button" class="btn" onClick={() => resetZoom()}>
            {t("computer.resetZoom")}
          </button>
        </Show>
      </div>

      {/* The floating bar of the iPhone pseudo-full-screen: the page chrome is out of reach there. */}
      <Show when={full()}>
        <div class="computer-mini" role="toolbar" aria-label={t("computer.fullScreenBar")}>
          <button type="button" class="computer-mini-btn" aria-label={t("computer.exitFull")} onClick={() => exitFull()}>
            ✕
          </button>
          <Show
            when={mine()}
            fallback={
              <button type="button" class="computer-mini-btn" disabled={busy() || !state()} onClick={() => void takeOver()}>
                {t("computer.takeOver")}
              </button>
            }
          >
            <button type="button" class="computer-mini-btn" aria-pressed={keyboard()} onClick={() => toggleKeyboard()}>
              {t("computer.keyboard")}
            </button>
            <button type="button" class="computer-mini-btn" onClick={() => void giveBack()}>
              {t("computer.giveBack")}
            </button>
          </Show>
          <For each={KEYS.slice(0, 8)}>
            {(k) => (
              <button
                type="button"
                class="computer-mini-btn"
                aria-label={k.key}
                onClick={() => pressKey(k)}
              >
                {k.label}
              </button>
            )}
          </For>
        </div>
      </Show>

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
                <button type="button" class="computer-key" onClick={() => pressKey(k)}>
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
