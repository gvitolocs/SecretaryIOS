import { createMemo, For, Show } from "solid-js";
import { t } from "../i18n";

// A colour wheel after Custom Cast's palette window (hue ring + saturation/value square, plus
// preset swatches), in the Nebula/Pearl look. Three labelled range inputs under it do the same
// with the keyboard and screen readers. `onInput` fires live while dragging, `onChange` once at
// the end of a gesture (one undo step in the editor).

export function hexToHsv(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16);
  const r = ((n >> 16) & 255) / 255, g = ((n >> 8) & 255) / 255, b = (n & 255) / 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b), d = max - min;
  let h = 0;
  if (d > 1e-9) {
    if (max === r) h = ((g - b) / d) % 6;
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
    if (h < 0) h += 360;
  }
  return [h, max === 0 ? 0 : d / max, max];
}

export function hsvToHex(h: number, s: number, v: number): string {
  const hh = (((h % 360) + 360) % 360) / 60;
  const c = v * s, x = c * (1 - Math.abs((hh % 2) - 1)), m = v - c;
  const [r, g, b] = hh < 1 ? [c, x, 0] : hh < 2 ? [x, c, 0] : hh < 3 ? [0, c, x] : hh < 4 ? [0, x, c] : hh < 5 ? [x, 0, c] : [c, 0, x];
  const to = (u: number) => Math.round(Math.min(1, Math.max(0, u + m)) * 255).toString(16).padStart(2, "0");
  return `#${to(r)}${to(g)}${to(b)}`;
}

export function ColorWheel(props: {
  label: string;
  /** `#rrggbb`, or null for "natural" (the wheel then shows `fallback`). */
  value: string | null;
  fallback?: string;
  swatches?: readonly string[];
  /** Offer a "Natural" chip that sets null. */
  natural?: boolean;
  onInput: (hex: string | null) => void;
  onChange: () => void;
}) {
  const hsv = createMemo(() => hexToHsv(props.value ?? props.fallback ?? "#8080c0"));
  let ring: HTMLDivElement | undefined;
  let square: HTMLDivElement | undefined;
  let drag: "ring" | "square" | null = null;

  const set = (h: number, s: number, v: number) => props.onInput(hsvToHex(h, s, v));

  function track(e: PointerEvent) {
    if (!drag) return;
    if (drag === "ring" && ring) {
      const r = ring.getBoundingClientRect();
      const a = Math.atan2(e.clientY - (r.top + r.height / 2), e.clientX - (r.left + r.width / 2));
      const [, s, v] = hsv();
      set(((a * 180) / Math.PI + 90 + 360) % 360, Math.max(s, 0.25), Math.max(v, 0.35));
    } else if (drag === "square" && square) {
      const r = square.getBoundingClientRect();
      const s = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
      const v = Math.min(1, Math.max(0, 1 - (e.clientY - r.top) / r.height));
      set(hsv()[0], s, v);
    }
  }

  function start(kind: "ring" | "square", e: PointerEvent) {
    e.preventDefault();
    e.stopPropagation();
    drag = kind;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    track(e);
  }

  function end() {
    if (!drag) return;
    drag = null;
    props.onChange();
  }

  const knobRing = () => {
    const a = ((hsv()[0] - 90) * Math.PI) / 180;
    return { left: `${50 + 43 * Math.cos(a)}%`, top: `${50 + 43 * Math.sin(a)}%` };
  };

  return (
    <div class="cwheel" role="group" aria-label={props.label}>
      <div class="cwheel-top">
        <div
          ref={ring}
          class="cwheel-ring"
          onPointerDown={(e) => start("ring", e)}
          onPointerMove={track}
          onPointerUp={end}
          onPointerCancel={end}
          aria-hidden="true"
        >
          <div
            ref={square}
            class="cwheel-square"
            style={{ "--cw-hue": `${hsv()[0]}` }}
            onPointerDown={(e) => start("square", e)}
            onPointerMove={track}
            onPointerUp={end}
            onPointerCancel={end}
          >
            <span class="cwheel-knob" style={{ left: `${hsv()[1] * 100}%`, top: `${(1 - hsv()[2]) * 100}%`, background: props.value ?? props.fallback }} />
          </div>
          <span class="cwheel-knob cwheel-knob--ring" style={knobRing()} />
        </div>
        <div class="cwheel-swatches">
          <Show when={props.natural}>
            <button
              type="button"
              class="cwheel-swatch cwheel-swatch--natural"
              classList={{ "is-on": props.value === null }}
              aria-pressed={props.value === null ? "true" : "false"}
              onClick={() => {
                props.onInput(null);
                props.onChange();
              }}
            >
              {t("avatarEditor.natural")}
            </button>
          </Show>
          <For each={props.swatches ?? []}>
            {(hex) => (
              <button
                type="button"
                class="cwheel-swatch"
                classList={{ "is-on": props.value === hex }}
                style={{ background: hex }}
                aria-label={hex}
                aria-pressed={props.value === hex ? "true" : "false"}
                onClick={() => {
                  props.onInput(hex);
                  props.onChange();
                }}
              />
            )}
          </For>
        </div>
      </div>
      <div class="cwheel-sliders">
        <label>
          <span>{t("avatarEditor.hue")}</span>
          <input
            type="range"
            min="0"
            max="359"
            value={Math.round(hsv()[0])}
            onInput={(e) => set(+e.currentTarget.value, hsv()[1], hsv()[2])}
            onChange={() => props.onChange()}
          />
        </label>
        <label>
          <span>{t("avatarEditor.saturation")}</span>
          <input
            type="range"
            min="0"
            max="100"
            value={Math.round(hsv()[1] * 100)}
            onInput={(e) => set(hsv()[0], +e.currentTarget.value / 100, hsv()[2])}
            onChange={() => props.onChange()}
          />
        </label>
        <label>
          <span>{t("avatarEditor.brightness")}</span>
          <input
            type="range"
            min="0"
            max="100"
            value={Math.round(hsv()[2] * 100)}
            onInput={(e) => set(hsv()[0], hsv()[1], +e.currentTarget.value / 100)}
            onChange={() => props.onChange()}
          />
        </label>
      </div>
    </div>
  );
}
