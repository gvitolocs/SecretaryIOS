import { createMemo, For } from "solid-js";
import { linkify } from "../lib/linkify";

/**
 * Plain text with safe autolinks. Line breaks are preserved by CSS
 * (`white-space: pre-wrap`); nothing is ever parsed as HTML.
 */
export function LinkifiedText(props: { text: string; class?: string }) {
  const segments = createMemo(() => linkify(props.text));
  return (
    <span class={props.class ? `rich ${props.class}` : "rich"}>
      <For each={segments()}>
        {(seg) =>
          seg.kind === "link" ? (
            <a href={seg.href} target="_blank" rel="noopener noreferrer nofollow">
              {seg.text}
            </a>
          ) : (
            seg.text
          )
        }
      </For>
    </span>
  );
}
