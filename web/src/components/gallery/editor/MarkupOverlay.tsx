import { Show, createSignal } from "solid-js";
import { t } from "../../../i18n";
import type { Mark, Point } from "../../../lib/editList";
import { extendMark, room, sizeOf, startMark, textMark, toFrame } from "../../../lib/markupDraw";
import type { EditorModel } from "./editorModel";
import type { MarkupSettings } from "./MarkupPanel";

/** Points closer than this many pixels on the shown picture are dropped from a stroke. */
const MIN_PX = 2;

/**
 * A layer over the picture that turns pointer moves into marks. The mark in progress is part of the previewed list, so
 * the Rust renderer draws it exactly as the saved picture will show it; letting go commits it (one undo step).
 * `width` is the shown picture's width in CSS pixels and `cropW` the crop box's width as a fraction of the frame.
 */
export function MarkupOverlay(props: { model: EditorModel; settings: MarkupSettings; width: number; cropW: number }) {
  const { model } = props;
  const [typing, setTyping] = createSignal<{ u: number; v: number } | null>(null);
  let el!: HTMLDivElement;
  let mark: Mark | null = null;
  let textAt: { u: number; v: number } | null = null;

  const where = (e: PointerEvent): { u: number; v: number } => {
    const r = el.getBoundingClientRect();
    return { u: (e.clientX - r.left) / r.width, v: (e.clientY - r.top) / r.height };
  };
  const frame = (p: { u: number; v: number }): Point => toFrame(p.u, p.v, model.committed().crop);
  const withMark = (m: Mark) => ({ ...model.committed(), markup: [...model.committed().markup, m] });
  // Marks use the frame's width as their unit; a shown pixel is this many of those.
  const minDistance = () => (props.width > 0 ? (MIN_PX / props.width) * props.cropW : 0);

  const down = (e: PointerEvent) => {
    const free = room(model.committed().markup);
    if (!free.marks || !free.points) return;
    const p = where(e);
    if (props.settings.tool === "text") {
      // The field opens on release: a press moves focus, and a field opened now would lose it at once.
      e.preventDefault();
      textAt = p;
      return;
    }
    e.preventDefault();
    el.setPointerCapture(e.pointerId);
    mark = startMark(props.settings.tool, props.settings.color, props.settings.size, frame(p));
    model.preview(withMark(mark));
  };

  const move = (e: PointerEvent) => {
    if (!mark) return;
    mark = extendMark(mark, frame(where(e)), minDistance());
    model.preview(withMark(mark));
  };

  const up = (e: PointerEvent) => {
    if (textAt) {
      setTyping(textAt);
      textAt = null;
      return;
    }
    if (!mark) return;
    // The release has its own position (a quick flick may end without a last move).
    const done = extendMark(mark, frame(where(e)), minDistance());
    mark = null;
    if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
    model.commit(withMark(done));
  };

  const finishText = (value: string) => {
    const at = typing();
    setTyping(null);
    if (!at) return;
    const m = textMark(props.settings.color, props.settings.size, frame(at), value);
    if (m) model.commit(withMark(m));
  };

  // The text field shows the text about as big as it will be drawn: size is a fraction of the frame width.
  const fontPx = () => sizeOf("text", props.settings.size) * (props.width / props.cropW);

  return (
    <div ref={el} class="ed-markup" onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={up}>
      <Show when={typing()}>
        {(at) => (
          <input
            class="ed-text-input"
            type="text"
            maxLength={300}
            aria-label={t("editor.textLabel")}
            placeholder={t("editor.textPlaceholder")}
            style={{ left: `${at().u * 100}%`, top: `${at().v * 100}%`, "font-size": `${Math.max(12, fontPx() * 0.82)}px`, color: props.settings.color }}
            ref={(input) => queueMicrotask(() => input.focus())}
            onPointerDown={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                finishText(e.currentTarget.value);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setTyping(null);
              }
            }}
            onBlur={(e) => finishText(e.currentTarget.value)}
          />
        )}
      </Show>
    </div>
  );
}
