import { createEffect, createSignal, on, onCleanup, onMount, Show, For } from "solid-js";
import { toApiError } from "../api/client";
import type { ControlState, Sandbox } from "../api/computer";
import { t } from "../i18n";
import { DeskSocket, type DeskMode, type DeskPhase } from "../lib/deskClient";
import { hasFocusMode, onFocusModeChange, setFocusMode } from "../lib/focusMode";
import { createTouchMouse } from "../lib/touch";
import {
  createWheel,
  fitRect,
  keyButton,
  keyMessage,
  mouse,
  textMessages,
  toHost,
  wheel,
  type ClientMessage,
  type DeskCodec,
  type DeskDisplay,
  type MouseButton,
} from "../lib/deskWire";
import "../styles/lucinadesk.css";

// LucinaDesk in /computer: Lucina's desktop over the RustDesk protocol, through the gateway on
// nezopt (lucinadesk repo, docs/CONTRACT.md), decoded here with WebCodecs onto a canvas. The Cua
// viewer stays as the "classic" fallback, chosen by the owner or by us when this cannot work.
//
// Control is RustDesk's: the page watches; the first click or key takes over (a confirm only when
// Lucina or an agent is mid-task), and that click or key is then delivered. A pill says who drives,
// with Give back and Exit. Taking over opens a control socket (the server takes the lease) and drops
// the watch socket once the control one shows a frame; giving back is the same the other way, so the
// picture never goes black.
//
// Keyboard: the canvas takes focus on a click and gets keydowns (⌘ is sent as Control: the owner is
// on a Mac, Lucina on Linux). On a touch screen nothing raises the keyboard but the Keyboard button,
// which focuses a hidden textarea and reads its `input` events (what the iPhone keyboard produces).
//
// Touch: tap = click, long press = right click, drag = left drag, two fingers = scroll, two-finger
// tap = right click, pinch = local zoom (lib/touch.ts, shared with Simple mode).
//
// Focus: inside Secretary Code the stream fills the whole content area and the shell hides its rail
// (lib/focusMode; the shell may leave focus itself, on Esc or a view change, and says so
// through `onFocusModeChange`); in a browser "Full screen" uses the Fullscreen API, and on iPhone,
// which has none for elements, a fixed, safe-area aware stage.

export type DeskFallback = "unavailable" | "unsupported" | "manual";

export interface DeskPlayerProps {
  sandbox: Sandbox;
  /** Who drives, from the page's control poll (the confirm overlay and the pill read it). */
  state: ControlState | null;
  /** Switch to the Cua viewer. */
  onFallback: (why: DeskFallback) => void;
  /** Something changed who drives: poll now. */
  onChanged?: () => void;
}

/** A takeover's trigger, delivered once the control socket is live. */
type Replay = { kind: "click"; x: number; y: number; button: MouseButton } | { kind: "msg"; msg: ClientMessage };

const SENTINEL = " ";
const RECONNECT_MS = [500, 1500, 4000, 8000];
const IDLE_CLOSE_MS = 30_000;
const KEYS: { label: string; key: string; modifiers?: string[] }[] = [
  { label: "Esc", key: "Escape" },
  { label: "Tab", key: "Tab" },
  { label: "⌫", key: "Backspace" },
  { label: "↵", key: "Return" },
  { label: "←", key: "LeftArrow" },
  { label: "↑", key: "UpArrow" },
  { label: "↓", key: "DownArrow" },
  { label: "→", key: "RightArrow" },
  { label: "Ctrl+L", key: "l", modifiers: ["Control"] },
  { label: "Ctrl+C", key: "c", modifiers: ["Control"] },
  { label: "Ctrl+V", key: "v", modifiers: ["Control"] },
];

function touchScreen(): boolean {
  return typeof window !== "undefined" && window.matchMedia?.("(pointer: coarse)").matches === true;
}

function callerName(who: string): string {
  const name = who.replace(/^mcp:/, "").replace(/-/g, " ");
  return name.charAt(0).toUpperCase() + name.slice(1);
}

export default function DeskPlayer(props: DeskPlayerProps) {
  const touch = touchScreen();
  /** Inside Secretary Code, whose focus mode replaces full screen. */
  const inShell = hasFocusMode();
  const [phase, setPhase] = createSignal<DeskPhase>("ticket");
  const [driving, setDriving] = createSignal(false);
  const [switching, setSwitching] = createSignal<null | "take" | "give">(null);
  const [display, setDisplay] = createSignal<DeskDisplay | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [confirm, setConfirm] = createSignal<{ text: string; replay: Replay | null } | null>(null);
  const [keyboard, setKeyboard] = createSignal(false);
  // Inside Secretary Code the stream owns the content area from the start, if it is on screen: the
  // shell also (re)loads this page in the background, and focus there would hide its rail over another
  // view. A background mount asks when it becomes visible (`onVisible`).
  const [focus, setFocus] = createSignal(inShell && typeof document !== "undefined" && document.visibilityState === "visible");
  /** Show the tucked-away pill for a moment (focus starts, who drives changes). */
  const [peek, setPeek] = createSignal(true);
  let peekTimer: number | undefined;
  const peekPill = () => {
    setPeek(true);
    window.clearTimeout(peekTimer);
    peekTimer = window.setTimeout(() => setPeek(false), 3000);
  };
  const [zoom, setZoom] = createSignal({ scale: 1, dx: 0, dy: 0 });
  const [cursor, setCursor] = createSignal<{ x: number; y: number } | null>(null);
  const [stats, setStats] = createSignal<{ codec: string; fps: number; rtt: number; firstFrameMs: number | null }>({
    codec: "",
    fps: 0,
    rtt: 0,
    firstFrameMs: null,
  });
  const showStats = (() => {
    try {
      return new URLSearchParams(location.search).has("deskstats") || localStorage.getItem("desk.stats") === "1";
    } catch {
      return false;
    }
  })();

  let stage: HTMLDivElement | undefined;
  let screenEl: HTMLDivElement | undefined;
  let canvas: HTMLCanvasElement | undefined;
  let kbd: HTMLTextAreaElement | undefined;
  let ctx: CanvasRenderingContext2D | null = null;
  let cur: DeskSocket | null = null;
  let next: DeskSocket | null = null;
  let replay: Replay | null = null;
  /** The takeover happened in Secretary Code's focus mode: leaving focus gives the desktop back. */
  let takenInFocus = false;
  let attempts = 0;
  let retryTimer: number | undefined;
  let idleTimer: number | undefined;
  let frameTimes: number[] = [];
  let firstDrawAt: number | undefined;
  let pinchBase: { scale: number; dx: number; dy: number } | null = null;
  let disposed = false;
  const step = createWheel();

  // ------------------------------------------------------------------ sockets

  const open = (mode: DeskMode) => {
    if (disposed) return;
    window.clearTimeout(retryTimer);
    const s: DeskSocket = new DeskSocket(props.sandbox, mode, {
      phase: (p) => {
        if (s === cur && p !== "closed") setPhase(p);
      },
      login: (d) => {
        if (s === cur || s === next) setDisplay(d);
      },
      draw: (frame) => {
        if (s === next) promote(s);
        if (s !== cur) return;
        draw(frame);
      },
      cursor: (x, y) => {
        if (s === cur) setCursor({ x, y });
      },
      refused: (_what, why) => {
        // A watch socket refusing input means we raced a give-back; nothing to tell the owner.
        if (why !== "watch") console.warn("[lucinadesk] input refused:", why);
      },
      closed: (code, reason, wasOpen) => onClosed(s, code, reason, wasOpen),
      unsupported: (codec: DeskCodec) => {
        console.warn("[lucinadesk] cannot decode", codec);
        props.onFallback("unsupported");
      },
    });
    if (cur) next = s;
    else {
      cur = s;
      setDriving(mode === "control");
    }
    s.start().catch((e) => onTicketError(s, e));
  };

  /** The new socket showed its first frame: it takes over, the old one goes. */
  const promote = (s: DeskSocket) => {
    const old = cur;
    cur = s;
    next = null;
    old?.close();
    setDriving(s.mode === "control");
    setSwitching(null);
    setPhase("live");
    props.onChanged?.();
    if (s.mode === "control" && replay) {
      const r = replay;
      replay = null;
      if (r.kind === "click") {
        s.send(mouse("move", r.x, r.y));
        s.send(mouse("down", r.x, r.y, r.button));
        s.send(mouse("up", r.x, r.y, r.button));
      } else {
        s.send(r.msg);
      }
    }
  };

  const onTicketError = (s: DeskSocket, e: unknown) => {
    const err = toApiError(e);
    if (s === next) {
      next = null;
      setSwitching(null);
      replay = null;
      setNotice(t("desk.takeOverFailed"));
      return;
    }
    if (s !== cur) return;
    cur = null;
    // No endpoint, not the owner, Android, or the gateway is down: this cannot work now.
    if ([400, 403, 404, 405, 501].includes(err.status) || err.code === "desk_unavailable") {
      props.onFallback("unavailable");
      return;
    }
    if (err.status === 401) return; // the app's sign-in handles it
    retryLater("watch", err.message);
  };

  const onClosed = (s: DeskSocket, code: number, reason: string, wasOpen: boolean) => {
    if (s === next) {
      next = null;
      setSwitching(null);
      replay = null;
      if (s.mode === "control") {
        setNotice(t("desk.takeOverFailed"));
        return;
      }
      // The give-back's watch socket failed: drop the control one anyway (that is the give-back)
      // and start watching from scratch.
      cur?.close();
      cur = null;
      open("watch");
      return;
    }
    if (s !== cur) return;
    cur = null;
    if (code === 4003) {
      props.onFallback("unavailable");
      return;
    }
    if (code === 4001) {
      setNotice(t("desk.controlEnded"));
      setDriving(false);
      props.onChanged?.();
      open("watch");
      return;
    }
    if (code === 4000) {
      setDriving(false);
      setError(t("desk.ended", { reason: reason || "?" }));
      return;
    }
    // A restart (1001), the network, or a refused ticket (1006 before open).
    const mode = s.mode === "control" && wasOpen ? "control" : "watch";
    if (wasOpen && s.stats.frames > 0) attempts = 0;
    retryLater(mode, reason);
  };

  const retryLater = (mode: DeskMode, why: string) => {
    if (attempts >= RECONNECT_MS.length) {
      // Never got a picture after several tries: the desk is not working from here.
      if (stats().firstFrameMs === null) props.onFallback("unavailable");
      else setError(t("desk.ended", { reason: why || "?" }));
      return;
    }
    setPhase("connecting");
    setNotice(t("desk.reconnecting"));
    const wait = RECONNECT_MS[attempts++]!;
    retryTimer = window.setTimeout(() => {
      setNotice(null);
      open(mode);
    }, wait);
  };

  const retry = () => {
    attempts = 0;
    setError(null);
    cur?.close();
    cur = null;
    open("watch");
  };

  // ------------------------------------------------------------------ video

  const draw = (frame: VideoFrame) => {
    if (!canvas) return;
    if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
      canvas.width = frame.displayWidth;
      canvas.height = frame.displayHeight;
      ctx = null;
    }
    ctx ??= canvas.getContext("2d", { alpha: false, desynchronized: true });
    ctx?.drawImage(frame, 0, 0);
    const now = performance.now();
    frameTimes.push(now);
    // For measuring against Cua's viewer: navigation start → first picture.
    firstDrawAt ??= now;
  };

  // ------------------------------------------------------------------ control

  /** First click or key while watching: take over (asking first if Lucina or an agent is working). */
  const requestTakeover = (trigger: Replay | null) => {
    if (switching() || confirm()) return;
    const s = props.state;
    let text: string | null = null;
    if (s?.driver === "lucina" && s.task) {
      text = s.task.who === "lucina" ? t("desk.confirmLucina", { what: s.task.what }) : t("desk.confirmAgent", { who: callerName(s.task.who) });
    } else if (s?.driver === "agent") {
      text = t("desk.confirmAgent", { who: callerName(s.lastAgent?.who ?? "agent") });
    }
    if (text) {
      // A click that needed a confirm is not replayed: the screen may have changed meanwhile.
      setConfirm({ text, replay: null });
      return;
    }
    takeOver(trigger);
  };

  const takeOver = (trigger: Replay | null) => {
    takenInFocus = inShell && focus();
    setConfirm(null);
    setNotice(null);
    setError(null);
    replay = trigger;
    setSwitching("take");
    open("control");
  };

  const giveBack = () => {
    if (!driving() || switching()) return;
    closeKeyboard();
    setDriving(false);
    setSwitching("give");
    open("watch");
  };

  const exit = () => {
    if (driving()) giveBack();
    leaveFocus();
  };

  /** Send while driving; a first input while watching asks for the takeover instead. */
  const input = (msg: ClientMessage, trigger?: Replay) => {
    if (driving() && cur?.mode === "control") {
      if (msg.type === "mouse" && (msg.mask & 7) === 0) cur.sendMove(msg);
      else cur.sendNow(msg);
      return;
    }
    if (trigger !== undefined) requestTakeover(trigger);
  };

  // ------------------------------------------------------------------ mouse

  const shown = () => {
    const box = canvas!.getBoundingClientRect();
    return fitRect(box, { width: canvas!.width || 1280, height: canvas!.height || 800 });
  };
  const hostAt = (e: { clientX: number; clientY: number }) => {
    const d = display() ?? { x: 0, y: 0, width: canvas?.width || 1280, height: canvas?.height || 800 };
    return toHost(e, shown(), d);
  };
  const BUTTONS: Record<number, MouseButton> = { 0: "left", 1: "middle", 2: "right" };

  const onPointerDown = (e: PointerEvent) => {
    if (e.pointerType === "touch") {
      pinchBase = null;
      canvas?.setPointerCapture?.(e.pointerId);
      tm.down(e, hostAt(e));
      return;
    }
    canvas?.focus({ preventScroll: true });
    const button = BUTTONS[e.button] ?? "left";
    const p = hostAt(e);
    e.preventDefault();
    if (!driving()) {
      requestTakeover({ kind: "click", ...p, button });
      return;
    }
    canvas?.setPointerCapture?.(e.pointerId);
    input(mouse("move", p.x, p.y));
    input(mouse("down", p.x, p.y, button));
  };
  const onPointerMove = (e: PointerEvent) => {
    if (e.pointerType === "touch") {
      tm.move(e, hostAt(e));
      return;
    }
    if (!driving()) return;
    const p = hostAt(e);
    input(mouse("move", p.x, p.y));
  };
  const onPointerUp = (e: PointerEvent) => {
    if (e.pointerType === "touch") {
      tm.up(e, hostAt(e));
      return;
    }
    if (!driving()) return;
    const p = hostAt(e);
    input(mouse("up", p.x, p.y, BUTTONS[e.button] ?? "left"));
  };
  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    if (!driving()) return;
    const s = step(e.deltaX, e.deltaY, e.deltaMode);
    if (!s) return;
    const p = hostAt(e);
    input(mouse("move", p.x, p.y));
    input(wheel(s.x, s.y));
  };

  const tm = createTouchMouse(
    {
      click: (at, button) => {
        if (!driving()) {
          requestTakeover({ kind: "click", ...at, button });
          return;
        }
        input(mouse("move", at.x, at.y));
        input(mouse("down", at.x, at.y, button));
        input(mouse("up", at.x, at.y, button));
      },
      down: (at) => {
        input(mouse("move", at.x, at.y));
        input(mouse("down", at.x, at.y, "left"));
      },
      move: (at) => input(mouse("move", at.x, at.y)),
      up: (at) => input(mouse("up", at.x, at.y, "left")),
      scroll: (at, direction, amount) => {
        input(mouse("move", at.x, at.y));
        const n = Math.max(1, Math.round(amount));
        // Natural scrolling: fingers moving down bring the top of the page back (wire y > 0 is up).
        const steps = { up: [0, -n], down: [0, n], left: [-n, 0], right: [n, 0] }[direction] as [number, number];
        input(wheel(steps[0], steps[1]));
      },
    },
    {
      onZoom: ({ scale, dx, dy }) => {
        const base = pinchBase ?? zoom();
        if (!pinchBase) pinchBase = base;
        setZoom({ scale: Math.min(4, Math.max(1, base.scale * scale)), dx: base.dx + dx, dy: base.dy + dy });
      },
    },
  );

  const resetZoom = () => {
    setZoom({ scale: 1, dx: 0, dy: 0 });
    pinchBase = null;
  };

  // ------------------------------------------------------------------ keyboard

  /** Desktop keys on the focused canvas. */
  const onKeyDown = (e: KeyboardEvent) => {
    // ⌘V pastes the Mac's clipboard as text (the `paste` handler); Ctrl+V is Lucina's own paste.
    if (e.metaKey && !e.ctrlKey && e.key.toLowerCase() === "v") return;
    const msg = keyMessage(e);
    if (!msg) return;
    e.preventDefault();
    input(msg, { kind: "msg", msg });
  };

  const onPaste = (e: ClipboardEvent) => {
    if (document.activeElement !== canvas && document.activeElement !== kbd) return;
    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (!text) return;
    e.preventDefault();
    for (const m of textMessages(text)) input(m, { kind: "msg", msg: m });
  };

  const resetKbd = () => {
    if (!kbd) return;
    kbd.value = SENTINEL;
    kbd.setSelectionRange(SENTINEL.length, SENTINEL.length);
  };

  /** The touch keyboard: control keys from keydown, text from `input` events (iPhone, IME). */
  const onKbdKeyDown = (e: KeyboardEvent) => {
    const msg = keyMessage(e);
    if (!msg || msg.type === "text") return; // text comes as `input`
    e.preventDefault();
    input(msg, { kind: "msg", msg });
  };
  const onKbdInput = (e: InputEvent) => {
    const kind = e.inputType ?? "";
    if (kind === "deleteContentBackward") input({ type: "key", key: "Backspace" });
    else if (kind === "insertLineBreak" || kind === "insertParagraph") input({ type: "key", key: "Return" });
    else if (e.data) for (const m of textMessages(e.data)) input(m, { kind: "msg", msg: m });
    resetKbd();
  };

  const toggleKeyboard = () => {
    if (keyboard()) {
      closeKeyboard();
      return;
    }
    setKeyboard(true);
    resetKbd();
    // Must happen inside the tap for iOS to raise its keyboard.
    kbd?.focus({ preventScroll: true });
  };
  const closeKeyboard = () => {
    setKeyboard(false);
    kbd?.blur();
  };

  const pressKey = (k: (typeof KEYS)[number]) => {
    const msg = keyButton(k.key, k.modifiers);
    input(msg, { kind: "msg", msg });
  };

  // ------------------------------------------------------------------ focus & full screen

  const enterFocus = () => {
    if (inShell) {
      setFocus(true);
      return;
    }
    const el = stage;
    if (el && typeof el.requestFullscreen === "function" && document.fullscreenEnabled) {
      el.requestFullscreen().then(() => setFocus(true)).catch(() => setFocus(true));
      return;
    }
    setFocus(true);
  };
  const leaveFocus = () => {
    if (document.fullscreenElement) void document.exitFullscreen().catch(() => undefined);
    setFocus(false);
    closeKeyboard();
  };

  createEffect(on([focus, driving, switching], () => peekPill()));

  /** What the shell last said or was told, so its own changes are not echoed back to it. */
  let shellFocus = false;
  createEffect(
    on(focus, (f) => {
      if (inShell && f !== shellFocus) {
        shellFocus = f;
        setFocusMode(f);
      }
      document.documentElement.classList.toggle("ldesk-focus", f);
    }),
  );

  // The iPhone keyboard shrinks the visual viewport, not the layout one: keep the stage on screen.
  const fitViewport = () => {
    const vv = window.visualViewport;
    if (!vv || !stage) return;
    stage.style.setProperty("--desk-vh", `${vv.height}px`);
    stage.style.setProperty("--desk-top", `${vv.offsetTop}px`);
  };

  // ------------------------------------------------------------------ lifecycle

  onMount(() => {
    open("watch");
    const tick = window.setInterval(() => {
      const now = performance.now();
      frameTimes = frameTimes.filter((x) => now - x < 1000);
      const st = cur?.stats;
      const s = { codec: st?.codec ?? "", fps: frameTimes.length, rtt: st?.rttMs ?? 0, firstFrameMs: st?.firstFrameMs ?? stats().firstFrameMs };
      setStats(s);
      (window as unknown as { __lucinadesk?: unknown }).__lucinadesk = { ...s, firstDrawAt, mode: cur?.mode, via: st?.via, frames: st?.frames, bytes: st?.bytes };
    }, 500);
    const onFullscreen = () => {
      if (!document.fullscreenElement && !inShell) setFocus(false);
    };
    const onVisible = () => {
      window.clearTimeout(idleTimer);
      if (document.hidden) {
        // A phone in a pocket should not pull video, nor hold Lucina's computer.
        idleTimer = window.setTimeout(() => {
          cur?.close();
          next?.close();
          cur = next = null;
          setDriving(false);
          setPhase("closed");
        }, IDLE_CLOSE_MS);
      } else {
        // Secretary Code keeps this page warm while another view shows and leaves focus then:
        // coming back to Computer is a new visit, so the stream takes the whole area again.
        if (inShell) setFocus(true);
        if (!cur && !error()) {
          attempts = 0;
          open("watch");
        }
      }
    };
    // The shell leaves focus by itself (Esc, its "Exit focus", another view): follow it, never fight it.
    // Like the classic view there: a takeover made in focus mode ends with it (like closing the tab).
    const unFocus = onFocusModeChange((on) => {
      shellFocus = on;
      setFocus(on);
      if (on) return;
      closeKeyboard();
      if (takenInFocus && driving()) giveBack();
    });
    document.addEventListener("fullscreenchange", onFullscreen);
    document.addEventListener("visibilitychange", onVisible);
    document.addEventListener("paste", onPaste);
    window.visualViewport?.addEventListener("resize", fitViewport);
    window.visualViewport?.addEventListener("scroll", fitViewport);
    onCleanup(() => {
      disposed = true;
      window.clearInterval(tick);
      window.clearTimeout(retryTimer);
      window.clearTimeout(idleTimer);
      window.clearTimeout(peekTimer);
      unFocus();
      document.removeEventListener("fullscreenchange", onFullscreen);
      document.removeEventListener("visibilitychange", onVisible);
      document.removeEventListener("paste", onPaste);
      window.visualViewport?.removeEventListener("resize", fitViewport);
      window.visualViewport?.removeEventListener("scroll", fitViewport);
      tm.reset();
      // Closing the control socket gives the desktop back (the server drops the lease).
      cur?.close();
      next?.close();
      cur = next = null;
      if (shellFocus) setFocusMode(false);
      document.documentElement.classList.remove("ldesk-focus");
    });
  });

  const pillLabel = () => {
    if (switching() === "take") return t("desk.takingOver");
    if (switching() === "give") return t("desk.givingBack");
    if (driving()) return t("desk.driving");
    const s = props.state;
    if (s?.driver === "owner") return t("desk.drivingOther", { device: s.owner?.device ?? "?" });
    return t("desk.watching");
  };

  const phaseText = () => {
    switch (phase()) {
      case "ticket":
        return t("desk.ticket");
      case "relay":
        return t("desk.relay");
      case "secured":
        return t("desk.secured");
      default:
        return t("desk.connecting");
    }
  };

  const cursorAt = () => {
    const c = cursor();
    const d = display();
    // The host does not draw its pointer into the video. With a mouse while driving, the Mac's own
    // pointer is the one to follow (a second, laggier one would only confuse); otherwise draw hers.
    if (!c || !d || d.cursorEmbedded || !canvas || (driving() && !touch)) return null;
    const box = canvas.getBoundingClientRect();
    const host = screenEl!.getBoundingClientRect();
    const s = fitRect(box, { width: canvas.width, height: canvas.height });
    return {
      left: s.left - host.left + ((c.x - d.x) * s.width) / d.width,
      top: s.top - host.top + ((c.y - d.y) * s.height) / d.height,
    };
  };

  return (
    <div
      ref={stage}
      class="ldesk-stage"
      classList={{ "ldesk-stage--focus": focus() }}
      data-driving={driving() ? "yes" : "no"}
      data-phase={phase()}
      data-peek={peek() || !!confirm() || !!error() ? "yes" : "no"}
    >
      <div class="ldesk-screen" ref={screenEl}>
        <canvas
          ref={canvas}
          class="ldesk-canvas"
          width="1280"
          height="800"
          tabIndex={0}
          role="img"
          aria-label={t("desk.screen")}
          style={{ transform: zoom().scale === 1 ? undefined : `translate(${zoom().dx}px, ${zoom().dy}px) scale(${zoom().scale})` }}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={() => {
            tm.cancel();
            pinchBase = null;
          }}
          onWheel={onWheel}
          onKeyDown={onKeyDown}
          onContextMenu={(e) => e.preventDefault()}
        />
        <Show when={cursorAt()}>{(c) => <span class="ldesk-cursor" style={{ left: `${c().left}px`, top: `${c().top}px` }} aria-hidden="true"><svg viewBox="0 0 12 18" width="12" height="18"><path d="M1 1v14l3.6-3.4 2.6 5.6 2.2-1-2.6-5.4H12z" fill="#fff" stroke="#000" stroke-width="1" stroke-linejoin="round" /></svg></span>}</Show>

        <Show when={phase() !== "live" && !error()}>
          <div class="ldesk-overlay" role="status">
            <span class="ldesk-spinner" aria-hidden="true" />
            <span>{notice() ?? phaseText()}</span>
          </div>
        </Show>
        <Show when={error()}>
          <div class="ldesk-overlay" role="alert">
            <span>{error()}</span>
            <div class="ldesk-overlay-actions">
              <button type="button" class="ldesk-btn ldesk-btn--primary" onClick={retry}>
                {t("desk.retry")}
              </button>
              <button type="button" class="ldesk-btn" onClick={() => props.onFallback("manual")}>
                {t("desk.classic")}
              </button>
            </div>
          </div>
        </Show>
        <Show when={confirm()}>
          {(c) => (
            <div class="ldesk-overlay ldesk-confirm" role="dialog" aria-modal="true" aria-label={t("desk.confirmTitle")}>
              <strong>{t("desk.confirmTitle")}</strong>
              <span>{c().text}</span>
              <div class="ldesk-overlay-actions">
                <button type="button" class="ldesk-btn ldesk-btn--primary" onClick={() => takeOver(c().replay)}>
                  {t("desk.confirmYes")}
                </button>
                <button type="button" class="ldesk-btn" onClick={() => setConfirm(null)}>
                  {t("desk.confirmNo")}
                </button>
              </div>
            </div>
          )}
        </Show>
      </div>

      <div class="ldesk-pill" role="toolbar" aria-label={pillLabel()}>
        <span class="ldesk-pill-dot" aria-hidden="true" />
        <span class="ldesk-pill-label" role="status">
          {pillLabel()}
        </span>
        <Show when={!driving() && !switching() && phase() === "live"}>
          <span class="ldesk-pill-hint">{touch ? t("desk.tapToDrive") : t("desk.clickToDrive")}</span>
        </Show>
        <Show when={showStats && stats().codec}>
          <span class="ldesk-pill-hint">{t("desk.stats", { codec: stats().codec, fps: stats().fps, rtt: stats().rtt })}</span>
        </Show>
        <Show when={driving() && touch}>
          <button type="button" class="ldesk-pill-btn" aria-pressed={keyboard()} onClick={toggleKeyboard}>
            {t("desk.keyboard")}
          </button>
        </Show>
        <Show when={driving()}>
          <button type="button" class="ldesk-pill-btn" disabled={!!switching()} onClick={giveBack}>
            {t("desk.giveBack")}
          </button>
        </Show>
        <Show
          when={focus()}
          fallback={
            <button type="button" class="ldesk-pill-btn" onClick={enterFocus}>
              {inShell ? t("desk.focus") : t("desk.fullScreen")}
            </button>
          }
        >
          <button type="button" class="ldesk-pill-btn" onClick={exit}>
            {t("desk.exit")}
          </button>
        </Show>
        <Show when={zoom().scale > 1.05}>
          <button type="button" class="ldesk-pill-btn" onClick={resetZoom}>
            {t("desk.resetZoom")}
          </button>
        </Show>
        <Show when={!focus()}>
          <button type="button" class="ldesk-pill-btn ldesk-pill-btn--quiet" onClick={() => props.onFallback("manual")}>
            {t("desk.classic")}
          </button>
        </Show>
      </div>

      <Show when={driving() && touch && keyboard()}>
        <div class="ldesk-keys" role="group" aria-label={t("desk.keysBar")}>
          <For each={KEYS}>
            {(k) => (
              <button type="button" class="ldesk-key" onPointerDown={(e) => e.preventDefault()} onClick={() => pressKey(k)}>
                {k.label}
              </button>
            )}
          </For>
        </div>
      </Show>

      <textarea
        ref={kbd}
        class="ldesk-kbd"
        aria-hidden="true"
        tabIndex={-1}
        autocapitalize="off"
        autocomplete="off"
        autocorrect="off"
        spellcheck={false}
        onKeyDown={onKbdKeyDown}
        onInput={(e) => onKbdInput(e as unknown as InputEvent)}
        onBlur={() => setKeyboard(false)}
      />
    </div>
  );
}
