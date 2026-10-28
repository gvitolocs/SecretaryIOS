import { createMemo, For, Match, Show, Switch, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";
import { inlineText, parseInline, parseMarkdown, type Block, type Inline } from "../lib/markdown";
import { InAppLink } from "./InAppLink";

// Renders lib/markdown's tree as elements with text children only: no
// innerHTML anywhere, links open in a new tab without a referrer.

function Inlines(props: { nodes: Inline[] }): JSX.Element {
  return (
    <For each={props.nodes}>
      {(n) => (
        <Switch>
          <Match when={n.t === "text" && n}>{(x) => x().text}</Match>
          <Match when={n.t === "br"}>
            <br />
          </Match>
          <Match when={n.t === "code" && n}>{(x) => <code>{x().text}</code>}</Match>
          <Match when={n.t === "strong" && n}>
            {(x) => (
              <strong>
                <Inlines nodes={x().children} />
              </strong>
            )}
          </Match>
          <Match when={n.t === "em" && n}>
            {(x) => (
              <em>
                <Inlines nodes={x().children} />
              </em>
            )}
          </Match>
          <Match when={n.t === "del" && n}>
            {(x) => (
              <del>
                <Inlines nodes={x().children} />
              </del>
            )}
          </Match>
          <Match when={n.t === "link" && n}>
            {(x) => (
              <Show
                when={x().route}
                fallback={
                  <a href={x().href} target="_blank" rel="noopener noreferrer nofollow">
                    <Inlines nodes={x().children} />
                  </a>
                }
              >
                <InAppLink href={x().href} class="md-note-link">
                  <Inlines nodes={x().children} />
                </InAppLink>
              </Show>
            )}
          </Match>
        </Switch>
      )}
    </For>
  );
}

function Blocks(props: { blocks: Block[] }): JSX.Element {
  return (
    <For each={props.blocks}>
      {(b) => (
        <Switch>
          <Match when={b.t === "heading" && b}>
            {(x) => (
              <Dynamic component={`h${Math.min(6, x().level + 1)}`} class="md-h" data-level={x().level}>
                <Inlines nodes={x().children} />
              </Dynamic>
            )}
          </Match>
          <Match when={b.t === "paragraph" && b}>
            {(x) => (
              <p>
                <Inlines nodes={x().children} />
              </p>
            )}
          </Match>
          <Match when={b.t === "list" && b}>
            {(x) => (
              <Dynamic component={x().ordered ? "ol" : "ul"} start={x().ordered && x().start !== 1 ? x().start : undefined}>
                <For each={x().items}>
                  {(item) => (
                    <li class={item.checked === undefined ? undefined : "md-task"}>
                      <Show when={item.checked !== undefined}>
                        <input type="checkbox" checked={item.checked} disabled aria-label={item.checked ? "done" : "to do"} />
                      </Show>
                      <Blocks blocks={item.children} />
                    </li>
                  )}
                </For>
              </Dynamic>
            )}
          </Match>
          <Match when={b.t === "quote" && b}>
            {(x) => (
              <blockquote>
                <Blocks blocks={x().children} />
              </blockquote>
            )}
          </Match>
          <Match when={b.t === "code" && b}>
            {(x) => (
              <pre class="md-pre" data-lang={x().lang || undefined}>
                <code>{x().text}</code>
              </pre>
            )}
          </Match>
          <Match when={b.t === "hr"}>
            <hr />
          </Match>
          <Match when={b.t === "table" && b}>
            {(x) => (
              <div class="md-table-wrap">
                <table class="md-table">
                  <thead>
                    <tr>
                      <For each={x().head}>
                        {(cell, k) => (
                          <th class={x().align[k()] ? `md-${x().align[k()]}` : undefined}>
                            <Inlines nodes={cell} />
                          </th>
                        )}
                      </For>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={x().rows}>
                      {(row) => (
                        <tr>
                          <For each={row}>
                            {(cell, k) => (
                              <td class={x().align[k()] ? `md-${x().align[k()]}` : undefined}>
                                <Inlines nodes={cell} />
                              </td>
                            )}
                          </For>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
            )}
          </Match>
        </Switch>
      )}
    </For>
  );
}

/** One line of inline markdown (bold, italic, code, links) as safe elements, no block wrapper. */
export function InlineMarkdown(props: { text: string }) {
  const nodes = createMemo(() => parseInline(props.text));
  return <Inlines nodes={nodes()} />;
}

/**
 * Markdown text as safe elements. Headings start at h2 (the page owns h1).
 * `skipHeading`: drop a first heading with this text (the page shows the title).
 */
export function Markdown(props: { text: string; class?: string; skipHeading?: string }) {
  const blocks = createMemo(() => {
    const all = parseMarkdown(props.text);
    const first = all[0];
    const skip = props.skipHeading?.trim().toLowerCase();
    if (skip && first?.t === "heading" && inlineText(first.children).trim().toLowerCase() === skip) return all.slice(1);
    return all;
  });
  return (
    <div class={props.class ? `md ${props.class}` : "md"}>
      <Blocks blocks={blocks()} />
    </div>
  );
}
