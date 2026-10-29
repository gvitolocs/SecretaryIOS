import { createEffect, createSignal, on, onCleanup, onMount } from "solid-js";
import { pastedImages } from "../../lib/keepPaste";
import { domToMd, mdToDom, type FormatCommand } from "../../lib/keepText";

// Active formats of the focused editor, for the formatting bar's pressed states.
const [activeFormats, setActiveFormats] = createSignal<ReadonlySet<FormatCommand>>(new Set());
export { activeFormats };

let current: HTMLElement | null = null;

function blockTag(): string {
  const sel = window.getSelection();
  let n: Node | null = sel?.anchorNode ?? null;
  while (n && n !== current) {
    if (n.nodeType === 1 && /^H[1-6]$/.test((n as HTMLElement).tagName)) return (n as HTMLElement).tagName;
    n = n.parentNode;
  }
  return "DIV";
}

function readFormats(): void {
  if (!current || document.activeElement !== current) return;
  const next = new Set<FormatCommand>();
  try {
    if (document.queryCommandState("bold")) next.add("bold");
    if (document.queryCommandState("italic")) next.add("italic");
    if (document.queryCommandState("underline")) next.add("underline");
  } catch {
    // queryCommandState is missing in some embedded webviews.
  }
  const b = blockTag();
  if (b === "H1") next.add("h1");
  if (b === "H2") next.add("h2");
  setActiveFormats(next);
}

/** Apply a formatting-bar command to the focused editor (the bar keeps the selection with preventDefault on mousedown). */
export function execFormat(cmd: FormatCommand): void {
  if (!current) return;
  current.focus();
  try {
    switch (cmd) {
      case "bold":
        document.execCommand("bold");
        break;
      case "italic":
        document.execCommand("italic");
        break;
      case "underline":
        document.execCommand("underline");
        break;
      case "h1":
      case "h2": {
        const tag = cmd === "h1" ? "H1" : "H2";
        document.execCommand("formatBlock", false, blockTag() === tag ? "div" : tag.toLowerCase());
        break;
      }
      case "clear":
        document.execCommand("removeFormat");
        document.execCommand("formatBlock", false, "div");
        break;
    }
  } catch {
    // execCommand unavailable: nothing to do.
  }
  current.dispatchEvent(new Event("input", { bubbles: true }));
  readFormats();
}

/**
 * The note body: a contenteditable that shows Keep's rich text basics and hands back markdown.
 * It reads `value` when it mounts and when `resetKey` changes, never while typing.
 */
export function RichEditor(props: {
  value: string;
  resetKey?: unknown;
  placeholder: string;
  label: string;
  readOnly?: boolean;
  autofocus?: boolean;
  onChange: (md: string) => void;
  onExit?: () => void;
}) {
  let el: HTMLDivElement | undefined;

  function fill() {
    if (!el) return;
    el.replaceChildren(mdToDom(props.value, document));
    el.toggleAttribute("data-empty", props.value.trim() === "");
  }

  onMount(() => {
    fill();
    if (props.autofocus) {
      el?.focus();
      const r = document.createRange();
      if (el) {
        r.selectNodeContents(el);
        r.collapse(false);
        const s = window.getSelection();
        s?.removeAllRanges();
        s?.addRange(r);
      }
    }
    document.addEventListener("selectionchange", readFormats);
    onCleanup(() => {
      document.removeEventListener("selectionchange", readFormats);
      if (current === el) current = null;
    });
  });
  createEffect(on(() => props.resetKey, fill, { defer: true }));

  return (
    <div
      ref={el}
      class="kp-rich"
      contenteditable={props.readOnly ? "false" : "true"}
      role="textbox"
      aria-multiline="true"
      aria-label={props.label}
      data-placeholder={props.placeholder}
      data-empty=""
      spellcheck={true}
      onFocus={() => {
        current = el ?? null;
        readFormats();
      }}
      onInput={() => {
        if (!el) return;
        const md = domToMd(el);
        el.toggleAttribute("data-empty", md.trim() === "" && !el.querySelector("h1, h2"));
        props.onChange(md);
      }}
      onPaste={(e) => {
        // An image paste is the note's (it attaches the image): insert nothing here and let it bubble up.
        const img = pastedImages(e.clipboardData);
        if (img.files.length || img.rejected) {
          e.preventDefault();
          return;
        }
        // Plain text only: pasted HTML would bring styles the markdown cannot keep.
        const text = e.clipboardData?.getData("text/plain");
        if (text === undefined) return;
        e.preventDefault();
        try {
          document.execCommand("insertText", false, text);
        } catch {
          // Ignore.
        }
      }}
      onKeyDown={(e) => {
        const mod = e.ctrlKey || e.metaKey;
        if (mod && !e.shiftKey && !e.altKey) {
          const k = e.key.toLowerCase();
          if (k === "b" || k === "i" || k === "u") {
            e.preventDefault();
            execFormat(k === "b" ? "bold" : k === "i" ? "italic" : "underline");
          }
        }
      }}
    />
  );
}
