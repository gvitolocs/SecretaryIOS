import { createResource, createSignal, For, Show } from "solid-js";
import { deleteStack, getStack, setStackPrimary, tileUrl, type Stack } from "../../../api/gallery";
import { t } from "../../../i18n";
import { itemName } from "../../../lib/galleryDedup";
import { StackIcon } from "./StackIcons";

export interface StackStripProps {
  stackId: string;
  /** The photo the viewer shows now. */
  currentId: string;
  jxl: boolean;
  /** Show another member of the stack (the viewer may not hold it in its list, so the stack comes along). */
  onPick: (stack: Stack, itemId: string) => void;
  /** The stack changed on the server: the grid has to reload. */
  onChanged: (change: "primary" | "unstack") => void;
}

/**
 * The strip of a stack's photos under the viewer, with "Make primary" (the shown photo stands for the stack in the
 * grid) and "Unstack" (every photo is a photo of its own again).
 */
export function StackStrip(props: StackStripProps) {
  const [stack] = createResource(() => props.stackId, (id) => getStack(id));
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);

  async function act(run: (s: Stack) => Promise<unknown>, change: "primary" | "unstack") {
    const s = stack();
    if (!s || busy()) return;
    setBusy(true);
    setFailed(false);
    try {
      await run(s);
      props.onChanged(change);
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="st-strip" role="group" aria-label={t("galleryDedup.stackStrip")}>
      <Show when={!stack.error} fallback={<p class="st-strip__error" role="alert">{t("galleryDedup.stackLoadError")}</p>}>
        <Show when={stack()}>
          {(s) => (
            <>
              <span class="st-strip__count">
                <StackIcon />
                {t("galleryDedup.stackStripCount", { count: s().count })}
              </span>
              <ul class="st-strip__list">
                <For each={s().items}>
                  {(item, index) => (
                    <li>
                      <button
                        type="button"
                        class="st-thumb"
                        classList={{ "st-thumb--current": item.id === props.currentId, "st-thumb--primary": item.id === s().primary }}
                        aria-label={t("galleryDedup.stackPhoto", { n: index() + 1, count: s().count })}
                        aria-current={item.id === props.currentId ? "true" : undefined}
                        title={itemName(item)}
                        onClick={() => props.onPick(s(), item.id)}
                      >
                        <Show when={tileUrl(item, props.jxl)}>{(src) => <img src={src()} alt="" draggable={false} />}</Show>
                        <Show when={item.id === s().primary}>
                          <span class="st-thumb__mark">{t("galleryDedup.primary")}</span>
                        </Show>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
              <Show when={s().primary !== props.currentId}>
                <button type="button" class="st-btn" disabled={busy()} onClick={() => void act((x) => setStackPrimary(x.id, props.currentId), "primary")}>
                  {t("galleryDedup.makePrimary")}
                </button>
              </Show>
              <button type="button" class="st-btn" disabled={busy()} onClick={() => void act((x) => deleteStack(x.id), "unstack")}>
                {t("galleryDedup.unstack")}
              </button>
            </>
          )}
        </Show>
      </Show>
      <Show when={failed()}>
        <span class="st-strip__error" role="alert">
          {t("galleryDedup.actionFailed")}
        </span>
      </Show>
    </div>
  );
}
