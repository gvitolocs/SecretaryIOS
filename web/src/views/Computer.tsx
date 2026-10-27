import { A } from "@solidjs/router";
import { createEffect, createSignal, For, lazy, on, onCleanup, onMount, Show } from "solid-js";
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
import type { DeskFallback } from "../components/DeskPlayer";
import { BackIcon } from "../components/Icons";
import { t } from "../i18n";
import { hasFocusMode, onFocusModeChange, setFocusMode } from "../lib/focusMode";
import { readString, writeString } from "../lib/storage";
import { deskCapable } from "../lib/deskClient";
import { createTouchMouse } from "../lib/touch";
import "../styles/computer.css";

// LucinaDesk (the RustDesk-protocol view of the desktop) in its own chunk: only the owner's live view loads it.
const LucinaDesk = lazy(() => import("../components/DeskPlayer"));

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
//   aware screen with a floating pill on iPhone, which does not implement it for elements. Inside
//   Secretary Code it is the shell's focus mode (`lib/focusMode.ts`) with the same layout: the
//   Fullscreen API there would put the whole Mac window into macOS full screen. The viewer's own
//   fullscreen icon asks the page (`lucina:fullscreen`), so both ways end in the same place.
// - Full screen behaves like a remote desktop app: while watching, the first click or key press on
//   the stream takes over (a small confirm first if Lucina or an agent is mid-task) and is then
//   replayed into the desktop; the pill shows who drives, Take over / Give back, Keyboard, the key
//   row and Exit. With a hardware keyboard the stream gets keyboard focus as soon as it is driven.
//   Esc leaves full screen only while watching; driving, it is the desktop's (exit with the pill).

/** The click or key that took control from full screen, replayed into the desktop once it works. */
type Pass = { click?: { x: number; y: number; button: "left" | "right" }; text?: string; keys?: string[] };

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

/** Audit events that are plumbing (sockets, tickets, live-socket byte counts), not something anyone did. */
const QUIET_EVENTS = new Set(["view_open", "view_ticket", "view_socket_open", "view_socket_closed"]);

/** Recent activity worth showing: who took, gave back, ran tasks or sent input batches. */
export function meaningful(e: AuditEntry): boolean {
  if (QUIET_EVENTS.has(e.event)) return false;
  return !(e.event === "input" && e.batch?.via === "live");
}

/** A KeyboardEvent as the viewer's key: printable text, or the key name Cua uses (`enter`, `arrowleft`). */
export function passKey(e: { key: string; ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean }): { text?: string; keys?: string[] } | null {
  if (["Shift", "Control", "Alt", "Meta", "CapsLock", "Dead", "Unidentified", "Process"].includes(e.key)) return null;
  if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) return { text: e.key };
  const key = e.key === " " ? "space" : e.key.toLowerCase();
  if (!/^[a-z0-9]{1,16}$/.test(key)) return null;
  const mods = [e.ctrlKey ? "control" : null, e.altKey ? "alt" : null, e.metaKey ? "meta" : null].filter((m): m is string => !!m);
  return { keys: [...mods, key] };
}

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
  const [classic, setClassic] = createSignal(readString("local", "computer.classic") === "yes");
  /** LucinaDesk failed on this page (no endpoint, not the owner, a codec, no picture): Cua until reload. */
  const [deskBroken, setDeskBroken] = createSignal(false);
  /** LucinaDesk's player holds the takeover (its own control socket, not this page's lease calls). */
  const [deskDriving, setDeskDriving] = createSignal(false);
  /**
   * The desktop's Live view is LucinaDesk (components/DeskPlayer.tsx), not Cua's viewer. It drives
   * itself (first click or key takes over, its own pill, its own focus mode), so this page's full
   * screen, takeover bar, key bar and input card step aside while it shows.
   */
  const desk = () => sandbox() === "workspace" && mode() === "live" && !classic() && !deskBroken() && deskCapable();
  /** The pill's key row and clipboard popover. */
  const [tools, setTools] = createSignal(false);
  /** A takeover waiting for "yes" in full screen (Lucina or an agent is mid-task). */
  const [confirming, setConfirming] = createSignal<{ message: string; pass?: Pass } | null>(null);
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
  /** Bumped by every take/give-back/loss: a state poll started before one cannot undo it. */
  let controlGen = 0;
  /** Full screen ended while a takeover was still in flight: give back as soon as it lands. */
  let giveBackWhenDone = false;
  /** Control was taken from full screen (focus mode): leaving it gives control back. */
  let takenInFull = false;
  const coarse = () => typeof window !== "undefined" && !!window.matchMedia?.("(pointer: coarse)").matches;
  let pinchBase: { scale: number; dx: number; dy: number } | null = null;

  /** The mode the current view is in: what the running viewer is showing. */
  const viewMode = () => (mine() ? "control" : "watch");

  const loadState = async () => {
    const gen = controlGen;
    try {
      const r = await computerApi.control(sandbox());
      // A take or give-back happened while this was in flight: its answer is stale.
      if (gen !== controlGen) return;
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
    if (!version || mode() !== "live" || desk() || preload) return;
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
    if (mode() !== "live" || desk()) {
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
    controlGen += 1;
    takenInFull = false;
    setMine(false);
    setKeyboard(false);
    setNotice(t("computer.takeoverEnded"));
    void refreshTicket("watch");
  };

  /** Whether taking over needs a "yes" first (Lucina or an agent is mid-task), and what it says. */
  const confirmText = (): string | null => {
    const s = state();
    if (s?.driver === "lucina" && s.task) {
      return s.task.who === "lucina"
        ? t("computer.confirmTakeOver", { what: s.task.what })
        : t("computer.confirmTakeOverAgent", { who: callerName(s.task.who) });
    }
    if (s?.driver === "agent") return t("computer.confirmTakeOverAgent", { who: callerName(s.lastAgent?.who ?? "agent") });
    return null;
  };

  /**
   * Take the desktop. In full screen a needed "yes" is a small overlay (no browser dialog over the
   * stream), and `pass` is the click or key that asked for it, replayed once control works.
   */
  const takeOver = async (pass?: Pass, confirmed = false) => {
    if (busy() || mine()) return;
    const ask = confirmed ? null : confirmText();
    if (ask) {
      if (full()) {
        setConfirming({ message: ask, pass });
        return;
      }
      if (!window.confirm(ask)) return;
    }
    setConfirming(null);
    setBusy(true);
    controlGen += 1;
    try {
      const r = await computerApi.take(sandbox());
      setState(r.state);
      setMine(true);
      takenInFull = full() && hasFocusMode();
      setNotice(r.stoppedTask ? t("computer.stoppedTask", { what: r.stoppedTask }) : null);
      await refreshTicket("control");
      if (pass) post({ type: "lucina:pass", ...pass });
      focusStream();
      controlGen += 1;
      await loadState();
    } catch (e) {
      setError(toApiError(e).message);
    } finally {
      setBusy(false);
    }
    if (giveBackWhenDone) {
      giveBackWhenDone = false;
      if (mine()) await giveBack();
    }
  };

  /** Keyboard focus to the stream, so a hardware keyboard types into the desktop at once. */
  const focusStream = () => {
    if (coarse() || mode() !== "live" || !frame) return;
    try {
      frame.focus();
    } catch {
      // The frame went away.
    }
    post({ type: "lucina:focus" });
  };

  const giveBack = async () => {
    if (busy() || !mine()) return;
    setBusy(true);
    setKeyboard(false);
    setTools(false);
    controlGen += 1;
    takenInFull = false;
    try {
      // Switch the running viewer to watching first: a control view dies with the lease, so after
      // the release there would be nothing left to re-issue and the viewer would have to reload.
      await refreshTicket("watch");
      const r = await computerApi.release(sandbox());
      setState(r.state);
      setMine(false);
      setNotice(null);
      controlGen += 1;
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

  /** Full screen on or off, the way this page's host does it (see the file header). */
  const enterFull = () => {
    if (full()) return;
    if (hasFocusMode()) {
      setFocusMode(true);
      setFull(true);
      return;
    }
    const el = screen;
    if (el && typeof el.requestFullscreen === "function" && document.fullscreenEnabled) {
      el.requestFullscreen()
        .then(() => setFull(true))
        .catch(() => setFull(true));
      return;
    }
    // iPhone Safari cannot full-screen an element: a fixed, scroll-locked screen instead.
    setFull(true);
  };

  const exitFull = () => {
    if (hasFocusMode()) {
      setFocusMode(false);
      leftFull();
      return;
    }
    if (document.fullscreenElement) {
      void document.exitFullscreen().catch(() => undefined);
      return;
    }
    leftFull();
  };

  /** Full screen ended (our Exit, Esc, the shell leaving focus mode, the browser's own exit). */
  const leftFull = () => {
    setFull(false);
    setTools(false);
    setConfirming(null);
    // Taken from focus mode in the Mac app: leaving it gives the desktop back, like closing the tab.
    if (takenInFull && mine()) {
      if (busy()) giveBackWhenDone = true;
      else void giveBack();
    }
  };

  const toggleFull = () => (full() ? exitFull() : enterFull());

  /** The stream asked for control while watching in full screen: a click or a key. */
  const onCatchPointer = (e: PointerEvent) => {
    if (mine() || !frame) return;
    e.preventDefault();
    const box = frame.getBoundingClientRect();
    void takeOver({ click: { x: e.clientX - box.left, y: e.clientY - box.top, button: e.button === 2 ? "right" : "left" } });
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
      const keys = (e: KeyboardEvent) => {
        if (e.key === "Escape") {
          if (confirming()) {
            setConfirming(null);
          } else if (tools()) {
            setTools(false);
          } else if (mine() && mode() === "live") {
            // Driving: Esc is the desktop's (focus was on the pill). Leaving is the pill's ✕.
            post({ type: "lucina:keys", keys: ["escape"] });
            focusStream();
          } else {
            exitFull();
          }
          e.preventDefault();
          return;
        }
        // Watching: the first key press takes over and is typed into the desktop.
        if (mine() || busy() || confirming() || mode() !== "live") return;
        const target = e.target as HTMLElement | null;
        if (target?.closest?.("input, textarea, select, button, .computer-pill")) return;
        const pass = passKey(e);
        if (!pass) return;
        e.preventDefault();
        void takeOver(pass);
      };
      window.addEventListener("keydown", keys);
      onCleanup(() => window.removeEventListener("keydown", keys));
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
      // The viewer's own fullscreen icon: the same as the page's button.
      if (data.type === "lucina:fullscreen") toggleFull();
      // Esc pressed in the frame while only watching (driving, Esc stays the desktop's).
      if (data.type === "lucina:escape" && full() && !mine()) {
        if (confirming()) setConfirming(null);
        else exitFull();
      }
    };
    const onFullscreen = () => {
      // LucinaDesk full-screens its own stage; that is not this page's classic full screen.
      if (desk()) return;
      if (document.fullscreenElement) setFull(true);
      else if (full() && !hasFocusMode()) leftFull();
    };
    // Secretary Code's focus mode, including when the shell leaves it by itself.
    const unfocus = onFocusModeChange((on) => {
      // LucinaDesk's focus mode is its own.
      if (desk()) return;
      if (on) setFull(true);
      else if (full()) leftFull();
    });
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
      unfocus();
      if (full() && hasFocusMode()) setFocusMode(false);
      document.documentElement.classList.remove("computer-fullscreen");
      preload?.remove();
      preload = undefined;
      exit();
    });
  });

  const deskFallback = (why: DeskFallback) => {
    if (why === "manual") {
      writeString("local", "computer.classic", "yes");
      setClassic(true);
    } else {
      setDeskBroken(true);
    }
  };

  const useDesk = () => {
    writeString("local", "computer.classic", "no");
    setClassic(false);
  };

  // LucinaDesk ↔ Cua's viewer: the Cua view is minted only when it is the one showing.
  createEffect(
    on(desk, (on, was) => {
      if (was === undefined || on === was) return;
      if (full()) exitFull();
      preload?.remove();
      preload = undefined;
      void openView(viewMode());
    }),
  );

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
          <strong role="status">{driverLabel(state(), mine() || (desk() && deskDriving()))}</strong>
          <Show when={state()?.task}>{(task) => <span class="computer-muted">{t("computer.working", { what: task().what })}</span>}</Show>
          {/* LucinaDesk's pill says it (and its first click takes over, no button needed). */}
          <Show when={!mine() && !desk()}>
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
        <Show when={!desk()}>
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
        </Show>
      </div>

      <Show when={notice()}>
        <p class="computer-notice" role="status">{notice()}</p>
      </Show>
      <Show when={error()}>
        <p class="computer-error" role="alert">{error()}</p>
      </Show>

      <Show when={desk()}>
        <LucinaDesk
          sandbox={sandbox()}
          state={state()}
          onFallback={deskFallback}
          onChanged={() => void loadState()}
          onDriving={setDeskDriving}
        />
      </Show>

      <Show when={!desk()}>
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
          {/* Full screen, watching: the first click on the stream takes over (then reaches the desktop). */}
          <Show when={full() && mode() === "live" && view() && !mine()}>
            <div
              class="computer-catch"
              data-busy={busy() ? "yes" : "no"}
              aria-label={t("computer.clickToDrive")}
              title={t("computer.clickToDrive")}
              onPointerDown={onCatchPointer}
              onContextMenu={(e) => e.preventDefault()}
            />
          </Show>
          <Show when={confirming()}>
            {(c) => (
              <div class="computer-confirm" role="alertdialog" aria-label={t("computer.takeOver")}>
                <p>{c().message}</p>
                <div class="computer-confirm-actions">
                  <button type="button" class="btn" onClick={() => setConfirming(null)}>
                    {t("computer.cancel")}
                  </button>
                  <button type="button" class="btn btn--primary" onClick={() => void takeOver(c().pass, true)}>
                    {t("computer.takeOver")}
                  </button>
                </div>
              </div>
            )}
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
          <Show when={classic() && sandbox() === "workspace" && mode() === "live" && deskCapable()}>
            <button type="button" class="btn" onClick={() => useDesk()}>
              {t("desk.useDesk")}
            </button>
          </Show>
        </div>
      </Show>

      {/* Full screen's only chrome: who drives, the toggle, keyboard, keys & clipboard, exit. */}
      <Show when={full()}>
        <div class="computer-pill" role="toolbar" aria-label={t("computer.fullScreenBar")} data-mine={mine() ? "yes" : "no"}>
          <span class="computer-pill-state" role="status">
            <span class="computer-dot" aria-hidden="true" />
            {mine() ? t("computer.pillDriving") : t("computer.pillWatching")}
          </span>
          <Show
            when={mine()}
            fallback={
              <button type="button" class="computer-pill-btn computer-pill-btn--primary" disabled={busy() || !state()} onClick={() => void takeOver()}>
                {t("computer.takeOver")}
              </button>
            }
          >
            <button type="button" class="computer-pill-btn" disabled={busy()} onClick={() => void giveBack()}>
              {t("computer.giveBack")}
            </button>
            <button type="button" class="computer-pill-btn" aria-pressed={keyboard()} onClick={() => toggleKeyboard()}>
              {t("computer.keyboard")}
            </button>
            <button type="button" class="computer-pill-btn" aria-pressed={tools()} aria-expanded={tools()} onClick={() => setTools(!tools())}>
              {t("computer.keys")}
            </button>
          </Show>
          <button type="button" class="computer-pill-btn" aria-label={t("computer.exitFull")} title={t("computer.exitFull")} onClick={() => exitFull()}>
            ✕
          </button>
          <Show when={tools() && mine()}>
            <div class="computer-pill-pop" role="group" aria-label={t("computer.keys")}>
              <div class="computer-keys">
                <For each={KEYS}>
                  {(k) => (
                    <button type="button" class="computer-key" aria-label={k.key} onClick={() => pressKey(k)}>
                      {k.label}
                    </button>
                  )}
                </For>
              </div>
              <div class="computer-clip">
                <textarea class="text-input" rows="2" aria-label={t("computer.clipboardLabel")} value={clip()} placeholder={t("computer.clipboardPlaceholder")} onInput={(e) => setClip(e.currentTarget.value)} />
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
        </div>
      </Show>

      <Show when={mine() && !desk()}>
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

      <Show when={recent().some(meaningful)}>
        <h2>{t("computer.activity")}</h2>
        <ul class="computer-card computer-log">
          <For each={recent().filter(meaningful).slice(0, 10)}>
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
