import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, onMount, Show, untrack } from "solid-js";
import { Portal } from "solid-js/web";
import type { ItemRenditions } from "../../../api/gallery";
import { locale, t, type MessageKey } from "../../../i18n";
import { formatBytes } from "../../../lib/drive";
import {
  clampSplit,
  compareSources,
  defaultPair,
  dimensionsText,
  followRequest,
  formatName,
  SPLIT_STEP,
  splitFromPointer,
  splitPercent,
  type CompareSource,
  type SourceId,
} from "../../../lib/galleryCompare";
import { imageCore, type CoreView, type Gesture, type ImageCore, type Rect, type ViewRequest } from "../../../lib/imageCore";
import { supportsJxl } from "../../../lib/jxl";
import "../../../styles/gallery-compare.css";

export interface CompareViewProps {
  /** The item's name, for the dialog's label. */
  name: string;
  data: ItemRenditions;
  onClose: () => void;
}

type Mode = "side" | "split";
type Side = "a" | "b";

interface Pane {
  nat: { w: number; h: number } | null;
  view: CoreView | null;
  rect: Rect | null;
  failed: boolean;
}

const EMPTY: Pane = { nat: null, view: null, rect: null, failed: false };

export function sourceLabel(id: SourceId): string {
  if (id.startsWith("version:")) return t("galleryCompare.src_version", { version: id.slice("version:".length) });
  return t(`galleryCompare.src_${id}` as MessageKey);
}

/**
 * Two stored forms of one picture next to each other (or one over the other under a slider), zoomed and panned
 * together. Every pan and zoom goes through the Rust view core (image-core.wasm) once, on the first pane's view, and
 * the second pane follows it with the same relative zoom and offset (`followRequest`), so the two always show the
 * same part of the picture. Esc closes without closing the viewer under it.
 */
export function CompareView(props: CompareViewProps) {
  const [jxl] = createResource(supportsJxl);
  const sources = createMemo(() => compareSources(props.data, jxl() ?? false));
  const [core, setCore] = createSignal<ImageCore | null>(null);
  void imageCore().then((c) => setCore(c));

  const [left, setLeft] = createSignal<SourceId | null>(null);
  const [right, setRight] = createSignal<SourceId | null>(null);
  // Chosen once the JPEG XL check is in, so the default pair counts a decodable preview.
  createEffect(() => {
    if (jxl.loading || left() !== null) return;
    const pair = defaultPair(sources());
    const all = sources();
    setLeft(pair?.[0] ?? all[0]?.id ?? null);
    setRight(pair?.[1] ?? all[1]?.id ?? all[0]?.id ?? null);
  });

  const [mode, setMode] = createSignal<Mode>("side");
  const [split, setSplit] = createSignal(0.5);
  const [a, setA] = createSignal<Pane>(EMPTY);
  const [b, setB] = createSignal<Pane>(EMPTY);
  const [size, setSize] = createSignal({ w: 0, h: 0 });

  let dialog: HTMLDivElement | undefined;
  let stage: HTMLDivElement | undefined;
  let firstPane: HTMLDivElement | undefined;
  let secondPane: HTMLDivElement | undefined;
  let observer: ResizeObserver | null = null;

  const source = (id: SourceId | null): CompareSource | undefined => sources().find((s) => s.id === id);
  const sourceA = () => source(left());
  const sourceB = () => source(right());
  const paneOf = (side: Side) => (side === "a" ? a() : b());
  const setPane = (side: Side, next: Pane | ((p: Pane) => Pane)) => (side === "a" ? setA(next) : setB(next));

  // ---- the view core -----------------------------------------------------------------------------------------------

  const ask = (req: ViewRequest) => {
    const c = core();
    if (!c) return null;
    const answer = c.view({ ...req, preview_px: 1_000_000, screen_scale: 1 });
    return answer.ok && answer.view && answer.rect ? { view: answer.view, rect: answer.rect } : null;
  };

  /** The view of `pane` that follows `master` (or a fresh fitted one when the pane has none yet). */
  const follow = (master: CoreView | null, pane: Pane): { view: CoreView; rect: Rect } | null => {
    const s = size();
    if (!pane.nat || !s.w || !s.h) return null;
    const fresh = ask({ new: [pane.nat.w, pane.nat.h, s.w, s.h] });
    if (!fresh) return null;
    return master ? ask(followRequest(master, fresh.view)) : fresh;
  };

  /** The pane whose view leads: the first one with a picture. */
  const master = (): CoreView | null => a().view ?? b().view;

  const refit = () => {
    // Pane A leads; B follows. A pane that has no picture of its own (failed to load) is left out.
    const lead = follow(null, a());
    if (lead) setA((p) => ({ ...p, view: lead.view, rect: lead.rect }));
    const other = follow(lead?.view ?? null, b());
    if (other) setB((p) => ({ ...p, view: other.view, rect: other.rect }));
  };

  // A fresh, fitted view once the core, the viewport and the pictures' sizes are known (and again when a picture changes).
  const sized = createMemo(() => size().w > 0 && size().h > 0);
  const natA = createMemo(() => (a().nat ? `${a().nat!.w}x${a().nat!.h}` : ""));
  const natB = createMemo(() => (b().nat ? `${b().nat!.w}x${b().nat!.h}` : ""));
  createEffect(() => {
    if (!core() || !sized()) return;
    natA();
    natB();
    untrack(refit);
  });

  // Another source in a pane: a new picture, so its size and view start over.
  createEffect(on(left, () => setA(EMPTY), { defer: true }));
  createEffect(on(right, () => setB(EMPTY), { defer: true }));

  /** Applies a gesture to the leading pane and makes the other follow. */
  const drive = (gesture: Gesture) => {
    const lead = master();
    if (!lead) return;
    const next = ask({ view: lead, gesture });
    if (!next) return;
    if (a().view) {
      setA((p) => ({ ...p, view: next.view, rect: next.rect }));
      const other = b().view ? ask(followRequest(next.view, b().view!)) : null;
      if (other) setB((p) => ({ ...p, view: other.view, rect: other.rect }));
    } else {
      setB((p) => ({ ...p, view: next.view, rect: next.rect }));
    }
  };

  const zoomed = () => (master()?.scale ?? 1) > 1.01;
  const zoomBy = (factor: number) => drive({ op: "pinch", factor, x: size().w / 2, y: size().h / 2 });
  const fit = () => refit();
  const percent = () => Math.round((master()?.scale ?? 1) * 100);

  // ---- measuring ---------------------------------------------------------------------------------------------------

  const measure = () => {
    const r = firstPane?.getBoundingClientRect();
    if (!r || !r.width || !r.height) return;
    const s = { w: r.width, h: r.height };
    const old = size();
    if (Math.abs(old.w - s.w) < 0.5 && Math.abs(old.h - s.h) < 0.5) return;
    setSize(s);
    // Keep the zoom and position through a resize (the core scales the offset with the viewport).
    for (const side of ["a", "b"] as const) {
      const pane = paneOf(side);
      if (!pane.view || !pane.nat) continue;
      const next = ask({ view: pane.view, gesture: { op: "resize", image_w: pane.nat.w, image_h: pane.nat.h, viewport_w: s.w, viewport_h: s.h } });
      if (next) setPane(side, (p) => ({ ...p, view: next.view, rect: next.rect }));
    }
  };

  // The two modes size the panes differently: measure again when the mode changes.
  createEffect(
    on(mode, () => {
      queueMicrotask(measure);
    }, { defer: true }),
  );

  // ---- pointers ----------------------------------------------------------------------------------------------------

  // Pointers are tracked in client coordinates: a pan is a difference of those, which stays right when the pointer
  // crosses from one pane into the other. Anchors (zoom about a point) are converted to the pane's own points.
  const pointers = new Map<number, { x: number; y: number }>();
  let last = { x: 0, y: 0 };
  let pinchDistance = 0;
  let pinchMid = { x: 0, y: 0 };

  const anchor = (clientX: number, clientY: number) => {
    const inside = (r: DOMRect | undefined) => !!r && clientX >= r.left && clientX <= r.right && clientY >= r.top && clientY <= r.bottom;
    const second = secondPane?.getBoundingClientRect();
    const r = mode() === "side" && inside(second) ? second : firstPane?.getBoundingClientRect();
    return r ? { x: clientX - r.left, y: clientY - r.top } : { x: clientX, y: clientY };
  };

  const fingers = () => {
    const [p, q] = [...pointers.values()];
    return { distance: Math.hypot(p!.x - q!.x, p!.y - q!.y), mid: { x: (p!.x + q!.x) / 2, y: (p!.y + q!.y) / 2 } };
  };

  const onPointerDown = (e: PointerEvent) => {
    if ((e.target as Element | null)?.closest?.("button, select, .cv-handle")) return;
    stage?.setPointerCapture?.(e.pointerId);
    const p = { x: e.clientX, y: e.clientY };
    pointers.set(e.pointerId, p);
    if (pointers.size === 1) last = p;
    if (pointers.size === 2) {
      const f = fingers();
      pinchDistance = f.distance;
      pinchMid = f.mid;
    }
  };

  const onPointerMove = (e: PointerEvent) => {
    if (!pointers.has(e.pointerId)) return;
    const p = { x: e.clientX, y: e.clientY };
    pointers.set(e.pointerId, p);
    if (pointers.size >= 2) {
      const f = fingers();
      if (pinchDistance > 0) {
        const at = anchor(f.mid.x, f.mid.y);
        drive({ op: "pinch", factor: f.distance / pinchDistance, x: at.x, y: at.y });
      }
      drive({ op: "pan", dx: f.mid.x - pinchMid.x, dy: f.mid.y - pinchMid.y });
      pinchDistance = f.distance;
      pinchMid = f.mid;
      return;
    }
    drive({ op: "pan", dx: p.x - last.x, dy: p.y - last.y });
    last = p;
  };

  const onPointerUp = (e: PointerEvent) => {
    pointers.delete(e.pointerId);
    const rest = [...pointers.values()][0];
    if (rest) last = rest;
  };

  const onDoubleClick = (e: MouseEvent) => {
    if ((e.target as Element | null)?.closest?.("button, select, .cv-handle")) return;
    const at = anchor(e.clientX, e.clientY);
    drive({ op: "double_tap", x: at.x, y: at.y });
  };

  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    const at = anchor(e.clientX, e.clientY);
    if (e.ctrlKey || e.metaKey || !zoomed()) {
      // A trackpad pinch (ctrl held) or a mouse wheel over a fitted picture zooms about the pointer.
      drive({ op: "pinch", factor: Math.exp(-e.deltaY * (e.ctrlKey ? 0.01 : 0.0025)), x: at.x, y: at.y });
    } else {
      drive({ op: "pan", dx: -e.deltaX, dy: -e.deltaY });
    }
  };

  // ---- the slider --------------------------------------------------------------------------------------------------

  let draggingSplit = false;
  const moveSplit = (clientX: number) => {
    const r = stage?.getBoundingClientRect();
    if (r) setSplit(splitFromPointer(clientX, r.left, r.width));
  };
  const onHandleDown = (e: PointerEvent) => {
    e.stopPropagation();
    draggingSplit = true;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    moveSplit(e.clientX);
  };
  const onHandleMove = (e: PointerEvent) => {
    if (draggingSplit) moveSplit(e.clientX);
  };
  const onHandleUp = () => {
    draggingSplit = false;
  };
  const onHandleKey = (e: KeyboardEvent) => {
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      e.stopPropagation();
      setSplit((s) => clampSplit(s + (e.key === "ArrowRight" ? SPLIT_STEP : -SPLIT_STEP)));
    } else if (e.key === "Home" || e.key === "End") {
      e.preventDefault();
      e.stopPropagation();
      setSplit(e.key === "Home" ? 0.02 : 0.98);
    }
  };

  // ---- keys and focus ----------------------------------------------------------------------------------------------

  onMount(() => {
    const previous = document.activeElement as HTMLElement | null;
    dialog?.focus();
    measure();
    // Captured on the window, before the viewer's own listener: Esc closes only the compare view.
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as Element | null;
      const inField = !!target?.closest?.("select, input, textarea");
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        props.onClose();
        return;
      }
      // Tab stays inside the dialog.
      if (e.key === "Tab") {
        const list = [...(dialog?.querySelectorAll<HTMLElement>("button:not([disabled]), select, [tabindex='0']") ?? [])];
        const first = list[0];
        const last = list[list.length - 1];
        const active = document.activeElement;
        if (!first || !last) e.preventDefault();
        else if (e.shiftKey && (active === first || active === dialog)) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && active === last) {
          e.preventDefault();
          first.focus();
        } else if (!dialog?.contains(active)) {
          e.preventDefault();
          first.focus();
        }
        return;
      }
      // Everything else belongs to this view while it is open: the viewer under it must not page or rotate.
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (inField && (e.key === "ArrowLeft" || e.key === "ArrowRight" || e.key === "ArrowUp" || e.key === "ArrowDown")) {
        e.stopPropagation();
        return;
      }
      switch (e.key) {
        case "+":
        case "=":
          zoomBy(1.5);
          break;
        case "-":
          zoomBy(1 / 1.5);
          break;
        case "0":
          fit();
          break;
        case "s":
        case "S":
          if (inField) return;
          setMode((m) => (m === "side" ? "split" : "side"));
          break;
        case "ArrowLeft":
        case "ArrowRight":
          if (mode() === "split") setSplit((s) => clampSplit(s + (e.key === "ArrowRight" ? SPLIT_STEP : -SPLIT_STEP)));
          else if (zoomed()) drive({ op: "pan", dx: e.key === "ArrowRight" ? -40 : 40, dy: 0 });
          break;
        case "ArrowUp":
        case "ArrowDown":
          if (zoomed()) drive({ op: "pan", dx: 0, dy: e.key === "ArrowDown" ? -40 : 40 });
          break;
        default:
          if (e.key.length === 1 || e.key === "Delete" || e.key === "Backspace") e.stopPropagation();
          return;
      }
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("resize", measure);
    observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
    if (firstPane) observer?.observe(firstPane);
    stage?.addEventListener("wheel", onWheel, { passive: false });
    onCleanup(() => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("resize", measure);
      observer?.disconnect();
      observer = null;
      stage?.removeEventListener("wheel", onWheel);
      previous?.focus();
    });
  });

  // ---- drawing -----------------------------------------------------------------------------------------------------

  const imageStyle = (side: Side) => {
    const r = paneOf(side).rect;
    return r ? { left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px` } : undefined;
  };

  const caption = (s: CompareSource | undefined) => {
    if (!s) return "";
    const parts = [formatName(s.file.format || s.file.mime), s.file.bytes !== null ? formatBytes(s.file.bytes, locale()) : "", dimensionsText(s.file)];
    return parts.filter(Boolean).join(" · ");
  };

  const Picker = (p: { side: Side; label: string }) => {
    const value = () => (p.side === "a" ? left() : right());
    return (
      <label class="cv-pick">
        <span class="cv-pick-label">{p.label}</span>
        <select
          class="cv-select"
          aria-label={p.label}
          value={value() ?? ""}
          onChange={(e) => (p.side === "a" ? setLeft : setRight)(e.currentTarget.value as SourceId)}
        >
          <For each={sources()}>{(s) => <option value={s.id}>{sourceLabel(s.id)}</option>}</For>
        </select>
      </label>
    );
  };

  /** The pane elements are remade when the mode changes: follow them with the size observer. */
  const paneRef = (side: Side, el: HTMLDivElement) => {
    if (side === "a") {
      if (firstPane) observer?.unobserve(firstPane);
      firstPane = el;
      observer?.observe(el);
    } else {
      secondPane = el;
    }
  };

  const PaneImage = (p: { side: Side; ref?: (el: HTMLDivElement) => void; clip?: boolean }) => {
    const s = () => (p.side === "a" ? sourceA() : sourceB());
    const pane = () => paneOf(p.side);
    return (
      <div
        ref={p.ref}
        class="cv-pane"
        classList={{ "cv-pane--over": !!p.clip }}
        style={p.clip ? { "clip-path": `inset(0 0 0 ${splitPercent(split())}%)` } : undefined}
        data-side={p.side}
      >
        <Show
          when={s()?.decodable && !pane().failed}
          fallback={
            <p class="cv-cant" role="status">
              {t("galleryCompare.cantShow", { format: formatName(s()?.file.format || s()?.file.mime || "") })}
            </p>
          }
        >
          <img
            class="cv-img"
            classList={{ "cv-img--placed": !!pane().rect }}
            src={s()!.file.url}
            alt={`${props.name} — ${sourceLabel(s()!.id)}`}
            draggable={false}
            style={imageStyle(p.side)}
            onLoad={(e) => {
              const img = e.currentTarget;
              if (img.naturalWidth && img.naturalHeight) setPane(p.side, (x) => ({ ...x, nat: { w: img.naturalWidth, h: img.naturalHeight }, failed: false }));
            }}
            onError={() => setPane(p.side, (x) => ({ ...x, failed: true }))}
          />
        </Show>
        <Show when={mode() === "side"}>
          <span class="cv-caption">{caption(s())}</span>
        </Show>
      </div>
    );
  };

  return (
    <Portal>
      <div ref={dialog} class="cv" role="dialog" aria-modal="true" aria-label={t("galleryCompare.title", { name: props.name })} tabindex="-1">
        <div class="cv-bar">
          <button type="button" class="cv-btn cv-close" aria-label={t("common.close")} onClick={() => props.onClose()}>
            ×
          </button>
          <Picker side="a" label={t("galleryCompare.left")} />
          <Picker side="b" label={t("galleryCompare.right")} />
          <div class="cv-seg" role="group" aria-label={t("galleryCompare.mode")}>
            <button type="button" class="cv-btn" classList={{ "cv-btn--on": mode() === "side" }} aria-pressed={mode() === "side"} onClick={() => setMode("side")}>
              {t("galleryCompare.mode_side")}
            </button>
            <button type="button" class="cv-btn" classList={{ "cv-btn--on": mode() === "split" }} aria-pressed={mode() === "split"} onClick={() => setMode("split")}>
              {t("galleryCompare.mode_split")}
            </button>
          </div>
          <div class="cv-zoom" role="group" aria-label={t("galleryCompare.zoom")}>
            <button type="button" class="cv-btn" aria-label={t("galleryCompare.zoomOut")} onClick={() => zoomBy(1 / 1.5)}>
              −
            </button>
            <button type="button" class="cv-btn cv-percent" aria-label={t("galleryCompare.fit")} title={t("galleryCompare.fit")} onClick={fit}>
              {percent()}%
            </button>
            <button type="button" class="cv-btn" aria-label={t("galleryCompare.zoomIn")} onClick={() => zoomBy(1.5)}>
              +
            </button>
          </div>
        </div>

        <div
          ref={stage}
          class="cv-stage"
          classList={{ "cv-stage--split": mode() === "split" }}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onDblClick={onDoubleClick}
        >
          <Show
            when={mode() === "side"}
            fallback={
              <>
                <PaneImage side="a" ref={(el) => paneRef("a", el)} />
                <PaneImage side="b" clip ref={(el) => paneRef("b", el)} />
                <div
                  class="cv-handle"
                  role="slider"
                  tabindex="0"
                  aria-label={t("galleryCompare.splitLabel")}
                  aria-orientation="horizontal"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={Math.round(splitPercent(split()))}
                  style={{ left: `${splitPercent(split())}%` }}
                  onPointerDown={onHandleDown}
                  onPointerMove={onHandleMove}
                  onPointerUp={onHandleUp}
                  onPointerCancel={onHandleUp}
                  onKeyDown={onHandleKey}
                >
                  <span class="cv-knob" aria-hidden="true">
                    ⇆
                  </span>
                </div>
                <span class="cv-caption cv-caption--a">{sourceA() ? `${sourceLabel(sourceA()!.id)} · ${caption(sourceA())}` : ""}</span>
                <span class="cv-caption cv-caption--b">{sourceB() ? `${sourceLabel(sourceB()!.id)} · ${caption(sourceB())}` : ""}</span>
              </>
            }
          >
            <PaneImage side="a" ref={(el) => paneRef("a", el)} />
            <PaneImage side="b" ref={(el) => paneRef("b", el)} />
          </Show>
        </div>
        <p class="cv-hint">{t("galleryCompare.hint")}</p>
      </div>
    </Portal>
  );
}
