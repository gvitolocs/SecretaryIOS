import { createSignal } from "solid-js";
import { splitFromPointer } from "../../../lib/lab";

export interface CompareSliderProps {
  before: string;
  after: string;
  beforeLabel: string;
  afterLabel: string;
  /** Names the slider for assistive technology. */
  label: string;
  /** Width over height of the pictures, so the box does not jump while they load. */
  ratio?: number | null;
}

/**
 * Before / after: the "before" picture is laid over the "after" and cut at the divider. Drag anywhere on the picture, or
 * use the arrow keys, Home and End on the divider.
 */
export function CompareSlider(props: CompareSliderProps) {
  const [split, setSplit] = createSignal(50);
  let box: HTMLDivElement | undefined;

  const move = (clientX: number) => {
    if (!box) return;
    const r = box.getBoundingClientRect();
    setSplit(splitFromPointer(clientX, r.left, r.width));
  };

  const onPointerDown = (e: PointerEvent) => {
    if (e.button !== 0 && e.pointerType === "mouse") return;
    box?.setPointerCapture(e.pointerId);
    move(e.clientX);
  };

  const onPointerMove = (e: PointerEvent) => {
    if (box?.hasPointerCapture(e.pointerId)) move(e.clientX);
  };

  return (
    <div
      ref={box}
      class="lab-compare"
      style={{ "aspect-ratio": props.ratio && props.ratio > 0 ? String(props.ratio) : "4 / 3" }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
    >
      <img class="lab-compare-img" src={props.after} alt={props.afterLabel} draggable={false} />
      <img class="lab-compare-img lab-compare-before" style={{ "clip-path": `inset(0 ${100 - split()}% 0 0)` }} src={props.before} alt={props.beforeLabel} draggable={false} />
      <span class="lab-tag lab-tag--before" aria-hidden="true">
        {props.beforeLabel}
      </span>
      <span class="lab-tag lab-tag--after" aria-hidden="true">
        {props.afterLabel}
      </span>
      <div class="lab-split-line" style={{ left: `${split()}%` }} aria-hidden="true">
        <span class="lab-split-knob" />
      </div>
      <input
        class="lab-split-input"
        type="range"
        min="0"
        max="100"
        step="1"
        value={split()}
        aria-label={props.label}
        aria-valuetext={`${props.beforeLabel} ${Math.round(split())}%`}
        onInput={(e) => setSplit(Number(e.currentTarget.value))}
        style={{ "--lab-split": `${split()}%` }}
      />
    </div>
  );
}
