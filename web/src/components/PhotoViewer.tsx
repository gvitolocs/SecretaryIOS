import { children, createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, untrack, type Accessor, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { t } from "../i18n";
import { imageCore, type CoreView, type DragOut, type Gesture, type ImageCore, type Rect, type ViewRequest } from "../lib/imageCore";
import "../styles/viewer.css";

/**
 * The gallery's photo viewer, shared by the chat lightbox and the Mac image viewer
 * (Secretary Code opens Finder files in it through `window.lucinaViewer`, lib/desktopViewer.ts).
 *
 * Zoom, pan, double tap and the swipe down to close follow the Rust view core (image-core.wasm,
 * the rules the iPhone viewer uses); pictures show fitted while it loads. Horizontal drags,
 * the arrow keys and a two-finger trackpad swipe move between items; a tap shows or hides the
 * controls, as in Google Photos.
 */

export interface ViewerItem {
  /** What to show: a URL the page can load (same origin, blob:, a shell's file protocol). */
  src: string | null;
  /** Full resolution, loaded once the zoom outgrows `src` (`previewPx` long side). */
  original?: string;
  previewPx?: number;
  name: string;
  mime?: string;
  kind?: "photo" | "video";
  /** The file itself, for sharing and saving a local file. */
  blob?: Blob;
  /** Drawn turned clockwise by this many degrees (the Gallery's saved rotation); 0 by default. */
  rotation?: 0 | 90 | 180 | 270;
  /** The library's id, for a `renderVideo` that has to ask the server for a playable URL. */
  id?: string;
  /** A video's still frame. */
  poster?: string | null;
}

export interface SaveResult {
  duplicate: boolean;
}

export interface ViewerActions {
  /** `true` uses the system share sheet (`navigator.share`). */
  share?: true | ((item: ViewerItem) => void | Promise<void>);
  saveToLucina?: (item: ViewerItem, onProgress: (sent: number, total: number) => void) => Promise<SaveResult>;
}

export interface PhotoViewerProps {
  items: ViewerItem[];
  index: number;
  onIndex: (index: number) => void;
  onClose: () => void;
  /** The dialog's accessible name; "Photo 2 of 9" by default. */
  label?: (index: number, count: number) => string;
  /** Past the last item comes the first (the chat); a folder stops at its ends. */
  wrap?: boolean;
  actions?: ViewerActions;
  /** The picture at `index` failed to load (e.g. HEIC in a browser without HEIC). */
  onImageError?: (index: number) => void;
  /** The top bar's extra buttons (`.pv-btn`s); hidden with the other controls. */
  toolbar?: JSX.Element;
  /** A panel beside the picture (a bottom sheet on phones); the stage shrinks to make room for it. */
  panel?: JSX.Element;
  /**
   * Draws a video item instead of the plain `<video src>` (the Gallery plays through a tokenised URL, with resume).
   * Only the current item has `current` set: neighbours must not start loading. With it, a horizontal drag that starts
   * on a video (outside its control strip) pages like on a picture, and a tap is left to the video itself.
   */
  renderVideo?: (ctx: { item: ViewerItem; index: number; current: boolean }) => JSX.Element;
  /**
   * Drawn right after a picture, in the same box as the picture (`style`, and `placed` once the core has placed it, as
   * for the picture's own `pv-img--placed` class), so a Live Photo's motion can lie exactly over its still. Called once
   * per page: the accessors follow the item, the zoom and the rotation.
   */
  renderOverlay?: (ctx: {
    item: Accessor<ViewerItem | undefined>;
    index: number;
    current: Accessor<boolean>;
    style: Accessor<JSX.CSSProperties | undefined>;
    placed: Accessor<boolean>;
  }) => JSX.Element;
  /** Called first for every key; return true to say the key is handled and the viewer must ignore it. */
  onKeyDown?: (e: KeyboardEvent) => boolean | void;
}

/** Points between pages, as on the phone. */
const PAGE_GAP = 24;
/** A tap moves less than this (points) and lasts less than TAP_MS. */
const TAP_SLOP = 6;
const TAP_MS = 300;
const DOUBLE_TAP_MS = 280;
const SETTLE_MS = 220;

type SaveState = { phase: "idle" } | { phase: "saving"; percent: number } | { phase: "saved"; duplicate: boolean } | { phase: "error" };

interface Pointer { x: number; y: number }
interface Sample { x: number; y: number; t: number }

export function PhotoViewer(props: PhotoViewerProps) {
  let panel: HTMLDivElement | undefined;
  let stage: HTMLDivElement | undefined;
  const [core, setCore] = createSignal<ImageCore | null>(null);
  const [size, setSize] = createSignal({ w: 0, h: 0 });
  const [natural, setNatural] = createSignal<{ w: number; h: number } | null>(null);
  const [view, setView] = createSignal<CoreView | null>(null);
  const [rect, setRect] = createSignal<Rect | null>(null);
  const [useOriginal, setUseOriginal] = createSignal(false);
  const [pagerDx, setPagerDx] = createSignal(0);
  const [out, setOut] = createSignal<DragOut | null>(null);
  const [animating, setAnimating] = createSignal(false);
  const [closing, setClosing] = createSignal(false);
  const [chrome, setChrome] = createSignal(true);
  const [save, setSave] = createSignal<SaveState>({ phase: "idle" });

  const count = () => props.items.length;
  const current = () => props.items[props.index];
  const isVideo = (item: ViewerItem | undefined) => item?.kind === "video" || (item?.mime?.startsWith("video/") ?? false);
  const neighbour = (delta: number): number | null => {
    const n = count();
    const i = props.index + delta;
    if (n < 2) return null;
    if (i >= 0 && i < n) return i;
    return props.wrap ? (i + n) % n : null;
  };
  const label = () => (props.label ?? ((i, n) => t("viewer.position", { index: i, count: n })))(props.index + 1, count());

  void imageCore().then((c) => setCore(c));

  // ---- the core ---------------------------------------------------------------------------

  const ask = (req: ViewRequest) => {
    const c = core();
    if (!c) return null;
    const answer = c.view({
      ...req,
      preview_px: current()?.previewPx ?? 1440,
      screen_scale: typeof devicePixelRatio === "number" && devicePixelRatio > 0 ? devicePixelRatio : 1,
    });
    return answer.ok ? answer : null;
  };

  const take = (answer: ReturnType<typeof ask>) => {
    if (!answer?.view || !answer.rect) return;
    setView(answer.view);
    setRect(answer.rect);
    if (answer.wants_original && current()?.original) setUseOriginal(true);
  };

  const apply = (gesture: Gesture) => {
    const v = view();
    if (v) take(ask({ view: v, gesture }));
  };

  const zoomed = () => (view()?.scale ?? 1) > 1.01;
  const turnOf = (item: ViewerItem | undefined) => item?.rotation ?? 0;
  /** A quarter turn (90 or 270): the picture lies on its side. */
  const turned = () => turnOf(current()) % 180 !== 0;

  // A fresh, fitted view once the picture, the viewport and the core are all known.
  createEffect(() => {
    const n = natural();
    const s = size();
    if (!core() || !n || !s.w || !s.h || view()) return;
    take(ask({ new: turned() ? [n.h, n.w, s.w, s.h] : [n.w, n.h, s.w, s.h] }));
  });

  // Rotated a quarter turn: the picture's width and height swap, so fit it again.
  createEffect(on(() => turnOf(current()), () => {
    setView(null);
    setRect(null);
  }, { defer: true }));

  // Another item: start fitted.
  createEffect(on(() => props.index, () => {
    setView(null);
    setRect(null);
    setNatural(null);
    setUseOriginal(false);
    setOut(null);
    setSave({ phase: "idle" });
  }, { defer: true }));

  const measure = () => {
    const r = stage?.getBoundingClientRect();
    if (!r || !r.width || !r.height) return;
    const s = { w: r.width, h: r.height };
    setSize(s);
    const v = view();
    if (v && (v.viewport_w !== s.w || v.viewport_h !== s.h)) {
      apply({ op: "resize", image_w: v.image_w, image_h: v.image_h, viewport_w: s.w, viewport_h: s.h });
    }
  };

  // ---- moving and closing -------------------------------------------------------------------

  let settleTimer: number | undefined;
  let settled: (() => void) | undefined;
  const finishSettle = () => {
    window.clearTimeout(settleTimer);
    const done = settled;
    settled = undefined;
    done?.();
    setAnimating(false);
  };
  const settle = (then: () => void) => {
    finishSettle();
    settled = then;
    setAnimating(true);
    settleTimer = window.setTimeout(finishSettle, SETTLE_MS);
  };

  /** To the next (1) or previous (-1) item, sliding when `animate`. */
  const move = (delta: number, animate = false) => {
    const next = neighbour(delta);
    if (next === null) {
      if (animate) {
        settle(() => {});
        setPagerDx(0);
      }
      return;
    }
    if (!animate) {
      setPagerDx(0);
      props.onIndex(next);
      return;
    }
    settle(() => {
      setPagerDx(0);
      props.onIndex(next);
    });
    setPagerDx(-delta * (size().w + PAGE_GAP));
  };

  const dismiss = () => {
    setClosing(true);
    window.setTimeout(() => props.onClose(), 160);
  };

  // ---- pointers -----------------------------------------------------------------------------

  const pointers = new Map<number, Pointer>();
  let start: Sample | null = null;
  let last: Pointer = { x: 0, y: 0 };
  let samples: Sample[] = [];
  let axis: "undecided" | "pager" | "out" | "pan" | "pinch" = "undecided";
  let pinchDistance = 0;
  let pinchMid: Pointer = { x: 0, y: 0 };
  let lastTap: Sample | null = null;
  let tapTimer: number | undefined;

  const local = (e: { clientX: number; clientY: number }): Pointer => {
    const r = stage?.getBoundingClientRect();
    return { x: e.clientX - (r?.left ?? 0), y: e.clientY - (r?.top ?? 0) };
  };

  const twoFingers = () => {
    const [a, b] = [...pointers.values()];
    return { distance: Math.hypot(a!.x - b!.x, a!.y - b!.y), mid: { x: (a!.x + b!.x) / 2, y: (a!.y + b!.y) / 2 } };
  };

  // A drag that began on a video: the stage takes the pointer only once it turns out to be a page swipe, so taps and
  // the video's own controls keep working.
  let onVideo = false;
  let captured = false;
  const CONTROL_STRIP = 64;

  const onPointerDown = (e: PointerEvent) => {
    const target = e.target as Element | null;
    if (target?.closest?.("button, .pv-top, [role=menu]")) return;
    const video = target?.closest?.("video");
    onVideo = false;
    if (video) {
      if (!props.renderVideo) return;
      if (e.clientY > video.getBoundingClientRect().bottom - CONTROL_STRIP) return;
      onVideo = true;
    }
    if (closing()) return;
    if (animating()) finishSettle();
    captured = !onVideo;
    if (captured) stage?.setPointerCapture?.(e.pointerId);
    pointers.set(e.pointerId, local(e));
    if (pointers.size === 1) {
      const p = local(e);
      start = { ...p, t: performance.now() };
      last = p;
      samples = [start];
      axis = "undecided";
    } else if (pointers.size === 2) {
      setPagerDx(0);
      setOut(null);
      axis = "pinch";
      const f = twoFingers();
      pinchDistance = f.distance;
      pinchMid = f.mid;
    }
  };

  const onPointerMove = (e: PointerEvent) => {
    if (!pointers.has(e.pointerId)) return;
    const p = local(e);
    pointers.set(e.pointerId, p);
    if (axis === "pinch") {
      if (pointers.size < 2) return;
      const f = twoFingers();
      if (pinchDistance > 0) apply({ op: "pinch", factor: f.distance / pinchDistance, x: f.mid.x, y: f.mid.y });
      apply({ op: "pan", dx: f.mid.x - pinchMid.x, dy: f.mid.y - pinchMid.y });
      pinchDistance = f.distance;
      pinchMid = f.mid;
      return;
    }
    if (!start) return;
    const now = performance.now();
    samples.push({ ...p, t: now });
    while (samples.length > 1 && samples[0]!.t < now - 100) samples.shift();
    const dx = p.x - start.x;
    const dy = p.y - start.y;
    if (axis === "undecided") {
      axis = dragAnswer(dx, dy)?.axis ?? "undecided";
      if (isVideo(current()) && axis === "pan") axis = "pager";
      if (axis === "pager" && !captured) {
        captured = true;
        stage?.setPointerCapture?.(e.pointerId);
      }
    }
    if (axis === "pan") {
      apply({ op: "pan", dx: p.x - last.x, dy: p.y - last.y });
    } else if (axis === "pager") {
      const edge = neighbour(dx < 0 ? 1 : -1) === null;
      setPagerDx(edge ? dx * 0.35 : dx);
    } else if (axis === "out") {
      const answer = dragAnswer(dx, dy);
      if (answer) setOut(answer.out);
    }
    last = p;
  };

  const velocity = () => {
    const first = samples[0];
    const end = samples[samples.length - 1];
    if (!first || !end || end.t - first.t < 1) return { vx: 0, vy: 0 };
    return { vx: (end.x - first.x) / (end.t - first.t), vy: (end.y - first.y) / (end.t - first.t) };
  };

  const dragAnswer = (dx: number, dy: number, ended = false) => {
    const { vx, vy } = velocity();
    const drag = { dx, dy, vx: vx * 1000, vy: vy * 1000, predicted_dy: dy + vy * 250, ended };
    const v = view();
    const answer = ask(v ? { view: v, drag } : { new: [1, 1, size().w, size().h], drag });
    if (answer?.drag) return answer.drag;
    const angle = Math.atan2(Math.abs(dx), Math.abs(dy)) * 180 / Math.PI;
    const axis = Math.hypot(dx, dy) < 10 ? "undecided" :
      angle <= 40 || (dy > 0 && angle <= 50 && Math.abs(vy) >= Math.abs(vx)) ? "out" : "pager";
    const p = dy > 0 ? Math.min(1, dy / (0.5 * (size().h || 1))) : 0;
    return {
      axis,
      out: { offset_x: dx, offset_y: dy > 0 ? dy : dy * 0.5, scale: 1 - 0.25 * p, backdrop: 1 - p },
      end: ended && (dy > size().h * 0.12 || (dy >= 16 && vy * 1000 >= 500)) ? "dismiss" : "back",
    } as const;
  };

  const tap = (p: Pointer) => {
    const now = performance.now();
    if (lastTap && now - lastTap.t < DOUBLE_TAP_MS && Math.hypot(p.x - lastTap.x, p.y - lastTap.y) < 30) {
      window.clearTimeout(tapTimer);
      lastTap = null;
      if (!isVideo(current())) apply({ op: "double_tap", x: p.x, y: p.y });
      return;
    }
    lastTap = { ...p, t: now };
    window.clearTimeout(tapTimer);
    tapTimer = window.setTimeout(() => setChrome((c) => !c), DOUBLE_TAP_MS);
  };

  const onPointerUp = (e: PointerEvent) => {
    if (!pointers.has(e.pointerId)) return;
    const p = local(e);
    pointers.delete(e.pointerId);
    if (axis === "pinch") {
      // The finger left behind does not start a new drag.
      if (pointers.size === 0) axis = "undecided";
      start = null;
      return;
    }
    if (!start) return;
    const dx = p.x - start.x;
    const dy = p.y - start.y;
    const now = performance.now();
    const quick = now - start.t < TAP_MS;
    samples.push({ ...p, t: now });
    while (samples.length > 1 && samples[0]!.t < now - 100) samples.shift();
    start = null;
    const { vx } = velocity();
    if (axis === "pager") {
      const w = size().w || 1;
      const flung = Math.abs(vx) > 0.5 && Math.sign(vx) === Math.sign(dx);
      if (Math.abs(dx) > w * 0.2 || flung) move(dx < 0 ? 1 : -1, true);
      else {
        settle(() => {});
        setPagerDx(0);
      }
    } else if (axis === "out") {
      const answer = dragAnswer(dx, dy, true);
      if (answer?.end === "dismiss") dismiss();
      else {
        settle(() => {});
        setOut(null);
      }
    } else if (axis === "undecided" && quick && Math.hypot(dx, dy) < TAP_SLOP && !onVideo) {
      tap(p);
    }
    axis = "undecided";
  };

  const onPointerCancel = (e: PointerEvent) => {
    pointers.delete(e.pointerId);
    start = null;
    axis = "undecided";
    setPagerDx(0);
    setOut(null);
  };

  // ---- wheel, trackpad and keys -------------------------------------------------------------

  let wheelSum = 0;
  let wheelQuietUntil = 0;
  let wheelReset: number | undefined;
  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    const p = local(e);
    if (e.ctrlKey) {
      // A trackpad pinch (Chromium and Firefox report it as a wheel with ctrl held).
      apply({ op: "pinch", factor: Math.exp(-e.deltaY * 0.01), x: p.x, y: p.y });
    } else if (zoomed()) {
      apply({ op: "pan", dx: -e.deltaX, dy: -e.deltaY });
    } else if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      // A two-finger swipe: one item per gesture.
      if (performance.now() < wheelQuietUntil) return;
      wheelSum += e.deltaX;
      window.clearTimeout(wheelReset);
      wheelReset = window.setTimeout(() => { wheelSum = 0; }, 200);
      if (Math.abs(wheelSum) > 60) {
        move(wheelSum > 0 ? 1 : -1, true);
        wheelSum = 0;
        wheelQuietUntil = performance.now() + 450;
      }
    }
  };

  // Safari (and WKWebView) report trackpad pinches as gesture events instead.
  let gestureScale = 1;
  const onGestureStart = (e: Event) => {
    e.preventDefault();
    gestureScale = 1;
  };
  const onGestureChange = (e: Event) => {
    e.preventDefault();
    const g = e as Event & { scale: number; clientX: number; clientY: number };
    if (!g.scale) return;
    const p = local(g);
    apply({ op: "pinch", factor: g.scale / gestureScale, x: p.x, y: p.y });
    gestureScale = g.scale;
  };

  const zoomBy = (factor: number) => {
    const s = size();
    apply({ op: "pinch", factor, x: s.w / 2, y: s.h / 2 });
  };

  onMount(() => {
    const previous = document.activeElement as HTMLElement | null;
    panel?.focus();
    measure();
    const onKey = (e: KeyboardEvent) => {
      if (props.onKeyDown?.(e) === true) return;
      if (e.key === "Escape") {
        e.preventDefault();
        props.onClose();
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        move(1);
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        move(-1);
      } else if (e.key === "+" || e.key === "=") {
        e.preventDefault();
        zoomBy(1.5);
      } else if (e.key === "-") {
        e.preventDefault();
        zoomBy(1 / 1.5);
      } else if (e.key === "0") {
        e.preventDefault();
        setView(null);
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", measure);
    const observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
    if (stage) observer?.observe(stage);
    stage?.addEventListener("wheel", onWheel, { passive: false });
    stage?.addEventListener("gesturestart", onGestureStart);
    stage?.addEventListener("gesturechange", onGestureChange);
    onCleanup(() => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", measure);
      observer?.disconnect();
      stage?.removeEventListener("wheel", onWheel);
      stage?.removeEventListener("gesturestart", onGestureStart);
      stage?.removeEventListener("gesturechange", onGestureChange);
      window.clearTimeout(tapTimer);
      window.clearTimeout(settleTimer);
      window.clearTimeout(wheelReset);
      previous?.focus();
    });
  });

  // ---- actions ------------------------------------------------------------------------------

  const share = async () => {
    const item = current();
    const action = props.actions?.share;
    if (!item || !action) return;
    if (action !== true) return void (await action(item));
    try {
      const file = item.blob ? new File([item.blob], item.name, { type: item.mime || item.blob.type }) : null;
      if (file && navigator.canShare?.({ files: [file] })) await navigator.share({ files: [file], title: item.name });
      else if (item.src && /^https?:|^\//.test(item.src)) await navigator.share({ title: item.name, url: new URL(item.src, location.href).href });
    } catch {
      // Cancelled, or nothing the browser can share.
    }
  };

  const saveToLucina = async () => {
    const item = current();
    const action = props.actions?.saveToLucina;
    if (!item || !action || save().phase === "saving") return;
    const index = props.index;
    setSave({ phase: "saving", percent: 0 });
    try {
      const result = await action(item, (sent, total) => {
        if (props.index === index) setSave({ phase: "saving", percent: total ? Math.round((sent / total) * 100) : 0 });
      });
      if (props.index === index) setSave({ phase: "saved", duplicate: result.duplicate });
    } catch {
      if (props.index === index) setSave({ phase: "error" });
    }
  };

  const saveLabel = () => {
    const s = save();
    if (s.phase === "saving") return t("viewer.saving", { percent: s.percent });
    if (s.phase === "saved") return s.duplicate ? t("viewer.already") : t("viewer.saved");
    if (s.phase === "error") return t("viewer.saveFailed");
    return t("viewer.save");
  };

  // ---- drawing ------------------------------------------------------------------------------

  const shownSrc = (item: ViewerItem | undefined, isCurrent: boolean) =>
    (isCurrent && useOriginal() && item?.original) || item?.src || null;

  const slots = createMemo(() => {
    // The current item first: it is the one the dialog is about.
    const list: { index: number; slot: number }[] = [{ index: props.index, slot: 0 }];
    for (const delta of [-1, 1]) {
      const i = neighbour(delta);
      if (i !== null && i !== props.index && !list.some((s) => s.index === i)) list.push({ index: i, slot: delta });
    }
    return list;
  }, undefined, {
    // The same pages again (an item's fields changed, not the pages): keep the pictures, don't reload them.
    equals: (a, b) => a.length === b.length && a.every((s, i) => s.index === b[i]?.index && s.slot === b[i]?.slot),
  });

  const slotStyle = (slot: number) => {
    const d = out();
    const base = `translate3d(${slot * (size().w + PAGE_GAP) + pagerDx()}px, 0, 0)`;
    if (slot !== 0 || !d) return { transform: base };
    return { transform: `${base} translate3d(${d.offset_x}px, ${d.offset_y}px, 0) scale(${d.scale})` };
  };

  const imageStyle = (item: ViewerItem | undefined, isCurrent: boolean) => {
    const r = isCurrent ? rect() : null;
    const turn = turnOf(item);
    const s = size();
    if (!turn) {
      if (!r) return undefined;
      return { left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px` };
    }
    // Rotated: the box the core placed is the turned picture's; draw the unturned image into its centre.
    const box = r ?? { x: 0, y: 0, w: s.w, h: s.h };
    const sideways = turn % 180 !== 0;
    const w = sideways ? box.h : box.w;
    const h = sideways ? box.w : box.h;
    if (!r && sideways) {
      // Not placed yet: fit the unturned picture to the stage's swapped sides.
      return { left: `${s.w / 2 - s.h / 2}px`, top: `${s.h / 2 - s.w / 2}px`, width: `${s.h}px`, height: `${s.w}px`, transform: `rotate(${turn}deg)` };
    }
    return { left: `${box.x + box.w / 2 - w / 2}px`, top: `${box.y + box.h / 2 - h / 2}px`, width: `${w}px`, height: `${h}px`, transform: `rotate(${turn}deg)` };
  };

  const toolbar = children(() => props.toolbar);
  const sidePanel = children(() => props.panel);

  return (
    <Portal>
      <div
        ref={panel}
        class="pv"
        classList={{ "pv--chrome": chrome(), "pv--animating": animating(), "pv--closing": closing(), "pv--zoomed": zoomed(), "pv--panel": !!sidePanel() }}
        role="dialog"
        aria-modal="true"
        aria-label={label()}
        tabindex="-1"
      >
        <div class="pv-backdrop" style={{ opacity: String(out()?.backdrop ?? 1) }} />
        <div
          ref={stage}
          class="pv-stage"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerCancel}
        >
          <For each={slots()}>
            {(s) => {
              const item = () => props.items[s.index];
              const isCurrent = () => s.index === props.index;
              return (
                <div class="pv-slot" style={slotStyle(s.slot)}>
                  <Show
                    when={!isVideo(item())}
                    fallback={
                      props.renderVideo ? (
                        props.renderVideo({ item: item()!, index: s.index, current: isCurrent() })
                      ) : (
                        <video class="pv-video" src={item()?.src ?? undefined} controls playsinline preload="metadata" />
                      )
                    }
                  >
                    <Show when={shownSrc(item(), isCurrent())}>
                      {(src) => (
                        <>
                          <img
                            class="pv-img"
                            classList={{ "pv-img--placed": isCurrent() && !!rect() }}
                            src={src()}
                            alt={item()?.name ?? ""}
                            draggable={false}
                            style={imageStyle(item(), isCurrent())}
                            onLoad={(e) => {
                              if (!isCurrent() || natural()) return;
                              const img = e.currentTarget;
                              if (img.naturalWidth && img.naturalHeight) setNatural({ w: img.naturalWidth, h: img.naturalHeight });
                            }}
                            onError={() => { if (isCurrent()) props.onImageError?.(s.index); }}
                          />
                          {untrack(() => props.renderOverlay?.({ item, index: s.index, current: isCurrent, style: () => imageStyle(item(), isCurrent()), placed: () => isCurrent() && !!rect() }))}
                        </>
                      )}
                    </Show>
                  </Show>
                </div>
              );
            }}
          </For>
        </div>
        <div class="pv-top">
          <button type="button" class="pv-btn pv-close" aria-label={t("common.close")} onClick={() => props.onClose()}>←</button>
          <span class="pv-title">
            <span class="pv-name">{current()?.name}</span>
            <Show when={count() > 0}><span class="pv-count">{props.index + 1} / {count()}</span></Show>
          </span>
          <div class="pv-actions" role="toolbar" aria-label={t("galleryInfo.toolbarLabel")}>
            <Show when={props.actions?.share}>
              <button type="button" class="pv-btn" aria-label={t("viewer.share")} onClick={() => void share()}><svg aria-hidden="true" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 16V3m-5 5 5-5 5 5M5 13v8h14v-8" /></svg></button>
            </Show>
            {toolbar()}
            <Show when={props.actions?.saveToLucina}>
              <button
                type="button"
                class="pv-btn pv-text pv-save"
                classList={{ "pv-save--done": save().phase === "saved", "pv-save--error": save().phase === "error" }}
                disabled={save().phase === "saving"}
                onClick={() => void saveToLucina()}
              >
                {saveLabel()}
              </button>
            </Show>
          </div>
        </div>
        <Show when={sidePanel()}>
          <aside class="pv-panel">{sidePanel()}</aside>
        </Show>
        <Show when={count() > 1}>
          <button type="button" class="pv-btn pv-prev" aria-label={t("viewer.previous")} disabled={neighbour(-1) === null} onClick={() => move(-1, true)}>‹</button>
          <button type="button" class="pv-btn pv-next" aria-label={t("viewer.next")} disabled={neighbour(1) === null} onClick={() => move(1, true)}>›</button>
        </Show>
      </div>
    </Portal>
  );
}
