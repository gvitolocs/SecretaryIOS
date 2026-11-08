import { t } from "../../../i18n";
import { ResetIcon } from "./EditorIcons";

/**
 * One labelled slider with its own reset. `onPreview` runs on every move (the picture follows, nothing is recorded);
 * `onCommit` when the thumb is let go or a key press settles (one undo step).
 */
export function Slider(props: {
  id: string;
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  /** The value that means "no change" (0 for most). */
  neutral?: number;
  format?: (value: number) => string;
  disabled?: boolean;
  onPreview: (value: number) => void;
  onCommit: (value: number) => void;
  onReset: () => void;
}) {
  const neutral = () => props.neutral ?? 0;
  const text = () => (props.format ? props.format(props.value) : String(Math.round(props.value * 100) / 100));
  return (
    <div class="ed-slider">
      <div class="ed-slider-head">
        <label for={props.id}>{props.label}</label>
        <output for={props.id}>{text()}</output>
        <button
          type="button"
          class="ed-mini"
          classList={{ "ed-mini--hidden": props.value === neutral() }}
          aria-label={t("editor.reset", { name: props.label })}
          title={t("editor.reset", { name: props.label })}
          tabindex={props.value === neutral() ? -1 : 0}
          disabled={props.disabled || props.value === neutral()}
          onClick={props.onReset}
        >
          <ResetIcon />
        </button>
      </div>
      <input
        id={props.id}
        type="range"
        min={props.min}
        max={props.max}
        step={props.step}
        value={props.value}
        disabled={props.disabled}
        onInput={(e) => props.onPreview(e.currentTarget.valueAsNumber)}
        onChange={(e) => props.onCommit(e.currentTarget.valueAsNumber)}
      />
    </div>
  );
}
