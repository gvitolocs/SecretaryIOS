import { createEffect, createSignal, on, onCleanup, onMount, Show, For } from "solid-js";
import { toApiError } from "../api/client";
import { releaseOnExit, type ControlState, type CopiedFile, type Sandbox } from "../api/computer";
import { downloadFile, sendFiles, tooLarge, watchCopiedFiles } from "../lib/desktopFiles";
import { t } from "../i18n";
import { decodableCodecs, DeskSocket, type DeskMode, type DeskPhase } from "../lib/deskClient";
import { createDeskPipe, type DeskPipe } from "../lib/deskPipe";
import { hasFocusMode, onFocusModeChange, setFocusMode } from "../lib/focusMode";
import { createTouchMouse } from "../lib/touch";
import {
  clipboardMessage,
  clipImageFrame,
  createWheel,
  fitRect,
  isMacPaste,
  keyButton,
  keyMessage,
  PASTE_KEY,
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
// on a Mac, Lucina on Linux; ⌘Q/W/H/M/Tab stay the Mac's).
//
// Clipboard (control only, CONTRACT §8): ⌘V (or the iPhone's Paste) puts the Mac's text on Lucina's
// clipboard, then presses Ctrl+V there; what she copies comes back as a `clipboard` frame and goes on the
// Mac's clipboard, or, where the browser refuses that, on a "tap to copy" chip. Watching, none of it.
// The text is never logged. Files (lib/desktopFiles, computer-mobile's endpoints): a ⌘V of Finder files
// uploads them to `~/Desktop/From Mac/`, puts them on her clipboard and presses Ctrl+V; dropping files on
// the stream only uploads them (on her clipboard too); files she copies are offered as downloads. On a touch screen nothing raises the keyboard but the Keyboard button,
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
  /** This player holds the takeover (the page's own status reads it). */
  onDriving?: (driving: boolean) => void;
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

/** "1.2 MB" (the page's own `byteSize` lives in views/Computer.tsx, which loads this lazily). */
function sizeOf(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  return `${(n / 1024 / 1024).toFixed(n < 10 * 1024 * 1024 ? 1 : 0)} MB`;
}

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
  /** What Lucina copied that the browser would not let us put on the clipboard: tap to copy. */
  /** Files Lucina copied in her file manager, offered as downloads. */
  const [offer, setOffer] = createSignal<CopiedFile[] | null>(null);
  const [copied, setCopied] = createSignal<{ text?: string; png?: Uint8Array; thumb?: string } | null>(null);
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
  /** The decoded picture's size (the canvas may belong to the worker, whose size this side cannot read). */
  let natural = { width: 1280, height: 800 };
  /** The player's video worker (lib/deskPipe.ts), or null: then the page decodes and draws. */
  let pipeReady: Promise<DeskPipe | null> = Promise.resolve(null);
  let pinchBase: { scale: number; dx: number; dy: number } | null = null;
  let disposed = false;
  let codecRetried = false;
  // Only while this player drives (the server checks the lease too).
  const copiedFiles = watchCopiedFiles(
    () => props.sandbox,
    () => driving() && cur?.mode === "control",
    (files) => setOffer(files),
  );
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
      drawn: (width, height) => {
        if (s === next) promote(s);
        if (s !== cur) return;
        natural = { width, height };
        const now = performance.now();
        frameTimes.push(now);
        // For measuring against Cua's viewer: navigation start → first picture.
        firstDrawAt ??= now;
      },
      cursor: (x, y) => {
        if (s === cur) setCursor({ x, y });
      },
      refused: (what, why) => {
        // A watch socket refusing input means we raced a give-back; nothing to tell the owner.
        if (what === "clipboard" && why === "too_large") setNotice(t("desk.pasteTooLarge"));
      else if (what === "clipboard" && why === "invalid") setNotice(t("desk.pasteRefused"));
        else if (why !== "watch") console.warn("[lucinadesk] input refused:", what, why);
      },
      clipboard: (text) => {
        if (s === cur && driving()) fromLucina({ text });
      },
      clipboardImage: (png) => {
        if (s === cur && driving()) fromLucina({ png });
      },
      closed: (code, reason, wasOpen) => onClosed(s, code, reason, wasOpen),
      unsupported: (codec: DeskCodec) => {
        console.warn("[lucinadesk] cannot decode", codec);
        // The ticket went with a remembered or guessed codec list: once, ask again with the probed one.
        if (codecRetried) {
          props.onFallback("unsupported");
          return;
        }
        codecRetried = true;
        void decodableCodecs().then((ok) => {
          if (disposed) return;
          if (!ok.length || ok.includes(codec)) props.onFallback("unsupported");
          else retry();
        });
      },
    }, pipeReady);
    if (cur) next = s;
    else {
      cur = s;
      s.makeCurrent();
      setDriving(mode === "control");
    }
    s.start().catch((e) => onTicketError(s, e));
  };

  /** The new socket showed its first frame: it takes over, the old one goes. */
  const promote = (s: DeskSocket) => {
    const old = cur;
    cur = s;
    next = null;
    s.makeCurrent();
    old?.close();
    // A new takeover starts fresh: her clipboard's files as they are now are not "just copied".
    if (s.mode === "control" && old?.mode !== "control") copiedFiles.reset();
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
    return fitRect(box, natural);
  };
  const hostAt = (e: { clientX: number; clientY: number }) => {
    const d = display() ?? { x: 0, y: 0, ...natural };
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

  // ------------------------------------------------------------------ clipboard (control only)

  /** The Mac's text onto Lucina's clipboard, then Ctrl+V there (the gateway holds the key until it is set). */
  const pasteText = (text: string) => {
    if (!driving() || !text) return;
    const msg = clipboardMessage(text);
    if (!msg) {
      setNotice(t("desk.pasteTooLarge"));
      return;
    }
    input(msg);
    input(PASTE_KEY);
  };

  /** The Mac's image (PNG) onto Lucina's clipboard, then Ctrl+V there. */
  const pasteImage = (png: Uint8Array) => {
    if (!driving() || cur?.mode !== "control") return;
    const frame = clipImageFrame(png);
    if (!frame) {
      setNotice(t("desk.imageTooLarge"));
      return;
    }
    cur.sendBinary(frame);
    input(PASTE_KEY);
  };

  /** Files onto her desktop (`From Mac`) and her clipboard; `paste` also presses Ctrl+V there. */
  const sendToLucina = async (files: File[], paste: boolean) => {
    if (!driving() || !files.length) return;
    const big = tooLarge(files);
    if (big.length) {
      setNotice(t("computer.filesTooBig", { name: big[0]!.name }));
      return;
    }
    try {
      await sendFiles(props.sandbox, files, (f) => setNotice(t("computer.filesSending", { name: f.name })));
      setNotice(files.length === 1 ? t("computer.filesSent", { name: files[0]!.name }) : t("computer.filesSentMany", { n: String(files.length) }));
      if (paste && driving()) input(PASTE_KEY);
    } catch (e) {
      setNotice(t("computer.filesFailed", { why: toApiError(e).message }));
    }
  };

  type MacClip = { text: string } | { png: Uint8Array } | { files: File[] };
  const pasteClip = (c: MacClip) => ("files" in c ? void sendToLucina(c.files, true) : "png" in c ? pasteImage(c.png) : pasteText(c.text));

  /** The Mac's clipboard through the Clipboard API: an image first (PNG), else text; null if refused. */
  const readMacClipboard = async (): Promise<MacClip | null> => {
    const cb = navigator.clipboard;
    if (cb?.read) {
      try {
        const items = await cb.read();
        for (const it of items) {
          if (it.types.includes("image/png")) return { png: new Uint8Array(await (await it.getType("image/png")).arrayBuffer()) };
        }
        for (const it of items) {
          if (it.types.includes("text/plain")) return { text: await (await it.getType("text/plain")).text() };
        }
        return null;
      } catch {
        // not allowed here: try text only
      }
    }
    try {
      const text = await cb?.readText?.();
      return text ? { text } : null;
    } catch {
      return null;
    }
  };

  /** A ⌘V waiting for the Mac's clipboard: the Clipboard API or the browser's own paste event, first wins. */
  let pendingPaste: ((c: MacClip) => void) | null = null;
  const PASTE_WAIT_MS = 1500;

  const macPaste = () => {
    let done = false;
    const finish = (c: MacClip) => {
      if (done) return;
      done = true;
      pendingPaste = null;
      window.clearTimeout(timer);
      pasteClip(c);
    };
    pendingPaste = finish;
    const timer = window.setTimeout(() => {
      if (done) return;
      done = true;
      pendingPaste = null;
      setNotice(t("desk.pasteDenied"));
    }, PASTE_WAIT_MS);
    void readMacClipboard().then((c) => c && finish(c));
  };

  const onPaste = (e: ClipboardEvent) => {
    if (!pendingPaste || (document.activeElement !== canvas && document.activeElement !== kbd)) return;
    const data = e.clipboardData;
    const files = Array.from(data?.files ?? []);
    // A screenshot comes as one `image.png`: an image to paste, not a file to send.
    const image = files.length === 1 && files[0]!.type === "image/png" && files[0]!.name === "image.png" ? files[0]! : null;
    const text = data?.getData("text/plain") ?? "";
    if (!files.length && !text) return;
    e.preventDefault();
    const finish = pendingPaste;
    if (image) void image.arrayBuffer().then((b) => finish({ png: new Uint8Array(b) }));
    else if (files.length) finish({ files });
    else finish({ text });
  };

  // What Lucina copies can come as a text frame and an image frame; within this long they are one copy.
  const COPY_MERGE_MS = 200;
  let incoming: { text?: string; png?: Uint8Array } = {};
  let incomingTimer: number | undefined;
  const fromLucina = (part: { text?: string; png?: Uint8Array }) => {
    incoming = { ...incoming, ...part };
    window.clearTimeout(incomingTimer);
    if (incoming.text !== undefined && incoming.png) flushIncoming();
    else incomingTimer = window.setTimeout(flushIncoming, COPY_MERGE_MS);
  };
  const flushIncoming = () => {
    window.clearTimeout(incomingTimer);
    const got = incoming;
    incoming = {};
    if (got.text !== undefined || got.png) copyLocal(got);
  };

  // The PNG's bytes are never shared memory; TypeScript cannot know that of a plain Uint8Array.
  const pngBlob = (png: Uint8Array) => new Blob([png as Uint8Array<ArrayBuffer>], { type: "image/png" });

  /** A copy as one clipboard write: text, an image, or both (the image as PNG). */
  const writeMac = (c: { text?: string; png?: Uint8Array }): Promise<void> => {
    const cb = navigator.clipboard;
    if (!c.png) return cb?.writeText ? cb.writeText(c.text ?? "") : Promise.reject(new Error("no clipboard API"));
    if (!cb?.write || typeof ClipboardItem !== "function") return Promise.reject(new Error("no image clipboard"));
    const parts: Record<string, Blob> = { "image/png": pngBlob(c.png) };
    if (c.text !== undefined) parts["text/plain"] = new Blob([c.text], { type: "text/plain" });
    return cb.write([new ClipboardItem(parts)]);
  };

  let copiedTimer: number | undefined;
  const clearCopied = () => {
    const c = copied();
    if (c?.thumb) URL.revokeObjectURL(c.thumb);
    setCopied(null);
  };
  /** Lucina copied something: onto the Mac's clipboard, or a chip to tap where the browser says no. */
  const copyLocal = (c: { text?: string; png?: Uint8Array }) => {
    writeMac(c).then(clearCopied, () => {
      clearCopied();
      setCopied({ ...c, thumb: c.png ? URL.createObjectURL(pngBlob(c.png)) : undefined });
      window.clearTimeout(copiedTimer);
      copiedTimer = window.setTimeout(clearCopied, 20_000);
    });
  };

  /** The chip's tap: a user gesture, so the Clipboard API works, or (text) the old copy command does. */
  const copyChip = () => {
    const c = copied();
    if (!c) return;
    const viaCommand = () => {
      if (c.text === undefined) return;
      const area = document.createElement("textarea");
      area.value = c.text;
      area.setAttribute("readonly", "");
      area.style.cssText = "position:fixed;opacity:0;left:0;top:0";
      document.body.appendChild(area);
      area.select();
      try {
        document.execCommand("copy");
      } finally {
        area.remove();
      }
    };
    writeMac(c).catch(viaCommand).finally(clearCopied);
  };

  /** Desktop keys on the focused canvas. */
  const onKeyDown = (e: KeyboardEvent) => {
    if (isMacPaste(e)) {
      if (!driving()) {
        // Watching: ⌘V only asks to take over; nothing of the clipboard moves.
        e.preventDefault();
        requestTakeover(null);
        return;
      }
      // Not prevented: where the Clipboard API is not allowed, the browser's own paste event brings the text.
      macPaste();
      return;
    }
    const msg = keyMessage(e);
    if (!msg) return;
    e.preventDefault();
    input(msg, { kind: "msg", msg });
    afterKey(msg);
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
    if (kind === "insertFromPaste") {
      // The iPhone's Paste: a real paste on Lucina's side (clipboard, then Ctrl+V), not typing.
      const v = kbd?.value ?? "";
      pasteText(e.data ?? (v.startsWith(SENTINEL) ? v.slice(SENTINEL.length) : v));
    } else if (kind === "deleteContentBackward") input({ type: "key", key: "Backspace" });
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

  /** Files dragged onto the stream while driving: into `From Mac` and onto her clipboard (no paste). */
  const dragsFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes("Files");
  const onDragOver = (e: DragEvent) => {
    if (!driving() || !dragsFiles(e)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
  };
  const onDrop = (e: DragEvent) => {
    if (!dragsFiles(e)) return;
    e.preventDefault();
    if (!driving()) return;
    void sendToLucina(Array.from(e.dataTransfer?.files ?? []), false);
  };

  /** Ctrl+C / Ctrl+X went to her: look for copied files now rather than at the next 2 s poll. */
  const afterKey = (msg: ClientMessage) => {
    if (driving() && msg.type === "key" && (msg.char === "c" || msg.char === "x") && msg.modifiers?.includes("Control")) copiedFiles.soon();
  };

  const pressKey = (k: (typeof KEYS)[number]) => {
    const msg = keyButton(k.key, k.modifiers);
    input(msg, { kind: "msg", msg });
    afterKey(msg);
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
  createEffect(on(driving, (d) => props.onDriving?.(d)));
  createEffect(on(driving, (d) => !d && setOffer(null)));

  // Over a live picture a notice is a toast that goes by itself (before that, the overlay shows it).
  createEffect(
    on([notice, phase], ([n, p]) => {
      if (!n || p !== "live") return;
      const timer = window.setTimeout(() => setNotice(null), 5000);
      onCleanup(() => window.clearTimeout(timer));
    }),
  );

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
    // The video goes to a worker where the browser allows it; the first ticket is asked for meanwhile.
    if (canvas) pipeReady = createDeskPipe(canvas).catch(() => null);
    open("watch");
    const tick = window.setInterval(() => {
      const now = performance.now();
      frameTimes = frameTimes.filter((x) => now - x < 1000);
      const st = cur?.stats;
      const s = { codec: st?.codec ?? "", fps: frameTimes.length, rtt: st?.rttMs ?? 0, firstFrameMs: st?.firstFrameMs ?? stats().firstFrameMs };
      setStats(s);
      (window as unknown as { __lucinadesk?: unknown }).__lucinadesk = { ...s, firstDrawAt, mode: cur?.mode, via: st?.via, frames: st?.frames, bytes: st?.bytes, marks: st?.marks };
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
    // Leaving the page while driving: the closing socket gives the desktop back, but not a lease the
    // owner already held from elsewhere, so say it outright (keepalive survives the navigation).
    const exitPage = () => {
      if (driving()) releaseOnExit(props.sandbox);
    };
    window.addEventListener("pagehide", exitPage);
    document.addEventListener("fullscreenchange", onFullscreen);
    document.addEventListener("visibilitychange", onVisible);
    document.addEventListener("paste", onPaste);
    window.visualViewport?.addEventListener("resize", fitViewport);
    window.visualViewport?.addEventListener("scroll", fitViewport);
    onCleanup(() => {
      disposed = true;
      void pipeReady.then((p) => p?.dispose());
      window.clearInterval(tick);
      window.clearTimeout(retryTimer);
      window.clearTimeout(idleTimer);
      window.clearTimeout(peekTimer);
      window.clearTimeout(copiedTimer);
      window.clearTimeout(incomingTimer);
      clearCopied();
      copiedFiles.stop();
      unFocus();
      window.removeEventListener("pagehide", exitPage);
      exitPage();
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
    const s = fitRect(box, natural);
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
      onDragOver={onDragOver}
      onDrop={onDrop}
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

      <Show when={offer()}>
        {(files) => (
          <div class="ldesk-files" role="status">
            <span>
              {files().length === 1
                ? t("computer.filesOffer", { name: files()[0]!.name, size: sizeOf(files()[0]!.bytes) })
                : t("computer.filesOfferMany", { n: String(files().length) })}
            </span>
            <button
              type="button"
              class="ldesk-btn ldesk-btn--primary"
              onClick={() => {
                for (const f of files()) downloadFile(props.sandbox, f);
                setOffer(null);
              }}
            >
              {t("computer.download")}
            </button>
            <button type="button" class="ldesk-btn" aria-label={t("computer.dismiss")} onClick={() => setOffer(null)}>
              ✕
            </button>
          </div>
        )}
      </Show>

      <Show when={notice() && phase() === "live"}>
        <div class="ldesk-toast" role="status">
          {notice()}
        </div>
      </Show>

      <Show when={copied() !== null && driving() ? copied() : null}>
        {(c) => (
          <button type="button" class="ldesk-chip" onClick={copyChip}>
            <Show when={c().thumb}>{(src) => <img class="ldesk-chip-thumb" src={src()} alt="" />}</Show>
            {t("desk.copiedChip")}
          </button>
        )}
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
