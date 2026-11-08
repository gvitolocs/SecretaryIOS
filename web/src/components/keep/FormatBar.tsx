import { For } from "solid-js";
import { t, type MessageKey } from "../../i18n";
import type { FormatCommand } from "../../lib/keepText";
import { activeFormats, execFormat } from "./RichEditor";

const BUTTONS: { cmd: FormatCommand; key: MessageKey; glyph: () => string; cls: string }[] = [
  { cmd: "h1", key: "keep.h1", glyph: () => "T", cls: "kp-fmt-glyph--h1" },
  { cmd: "h2", key: "keep.h2", glyph: () => "T", cls: "kp-fmt-glyph--h2" },
  { cmd: "bold", key: "keep.bold", glyph: () => "B", cls: "kp-fmt-glyph--b" },
  { cmd: "italic", key: "keep.italic", glyph: () => "I", cls: "kp-fmt-glyph--i" },
  { cmd: "underline", key: "keep.underline", glyph: () => "U", cls: "kp-fmt-glyph--u" },
  { cmd: "clear", key: "keep.clearFormat", glyph: () => "Tx", cls: "kp-fmt-glyph--clear" },
];

/** Keep's formatting row (opened by the "A" button): heading, bold, italic, underline, clear. */
export function FormatBar() {
  return (
    <div class="kp-format" role="toolbar" aria-label={t("keep.formatting")} onMouseDown={(e) => e.preventDefault()}>
      <For each={BUTTONS}>
        {(b) => (
          <button
            type="button"
            class="kp-fmt-btn"
            classList={{ "kp-fmt-btn--on": activeFormats().has(b.cmd) }}
            data-format={b.cmd}
            aria-pressed={activeFormats().has(b.cmd)}
            aria-label={t(b.key)}
            title={t(b.key)}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => execFormat(b.cmd)}
          >
            <span class={`kp-fmt-glyph ${b.cls}`}>{b.glyph()}</span>
          </button>
        )}
      </For>
    </div>
  );
}
