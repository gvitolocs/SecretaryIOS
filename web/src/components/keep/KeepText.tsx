import { createEffect, createMemo, For, Match, on, Switch } from "solid-js";
import { parseKeepText, type KBlock, type KInline } from "../../lib/keepText";

function Inline(props: { nodes: KInline[] }) {
  return (
    <For each={props.nodes}>
      {(n) => (
        <Switch>
          <Match when={n.t === "text" && n}>{(x) => x().text}</Match>
          <Match when={n.t === "b" && n}>
            {(x) => (
              <strong>
                <Inline nodes={x().c} />
              </strong>
            )}
          </Match>
          <Match when={n.t === "i" && n}>
            {(x) => (
              <em>
                <Inline nodes={x().c} />
              </em>
            )}
          </Match>
          <Match when={n.t === "u" && n}>
            {(x) => (
              <u>
                <Inline nodes={x().c} />
              </u>
            )}
          </Match>
          <Match when={n.t === "a" && n}>
            {(x) => (
              <a href={x().href} target="_blank" rel="noopener noreferrer nofollow" onClick={(e) => e.stopPropagation()}>
                <Inline nodes={x().c} />
              </a>
            )}
          </Match>
        </Switch>
      )}
    </For>
  );
}

/** A note's text as safe elements: a block per line, no innerHTML. */
export function KeepText(props: { text: string; class?: string }) {
  const blocks = createMemo<KBlock[]>(() => parseKeepText(props.text));
  let el: HTMLDivElement | undefined;
  // A card clips long text and fades its last lines; a short text keeps its full colour.
  createEffect(
    on(blocks, () =>
      requestAnimationFrame(() => {
        if (el) el.toggleAttribute("data-clipped", el.scrollHeight > el.clientHeight + 1 && getComputedStyle(el).overflow === "hidden");
      }),
    ),
  );
  return (
    <div ref={el} class={props.class ? `kp-text ${props.class}` : "kp-text"}>
      <For each={blocks()}>
        {(b) => (
          <Switch>
            <Match when={b.t === "blank"}>
              <div class="kp-line kp-line--blank">
                <br />
              </div>
            </Match>
            <Match when={b.t === "h" && b}>
              {(x) => (
                <div class={`kp-line kp-h${x().level}`}>
                  <Inline nodes={x().c} />
                </div>
              )}
            </Match>
            <Match when={b.t === "li" && b}>
              {(x) => (
                <div class="kp-line kp-li" data-indent={x().indent}>
                  <span class="kp-li-mark" aria-hidden="true">
                    {x().marker}
                  </span>
                  <span>
                    <Inline nodes={x().c} />
                  </span>
                </div>
              )}
            </Match>
            <Match when={b.t === "task" && b}>
              {(x) => (
                <div class="kp-line kp-li" classList={{ "kp-li--done": x().checked }}>
                  <span class="kp-li-mark" aria-hidden="true">
                    {x().checked ? "☑" : "☐"}
                  </span>
                  <span>
                    <Inline nodes={x().c} />
                  </span>
                </div>
              )}
            </Match>
            <Match when={b.t === "p" && b}>
              {(x) => (
                <div class="kp-line">
                  <Inline nodes={x().c} />
                </div>
              )}
            </Match>
          </Switch>
        )}
      </For>
    </div>
  );
}
