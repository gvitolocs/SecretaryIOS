import { For, Show } from "solid-js";
import { tileUrl, type StackSuggestion } from "../../../api/gallery";
import { t, type MessageKey } from "../../../i18n";
import { itemName, spanSeconds } from "../../../lib/galleryDedup";

export interface StackSuggestionListProps {
  /** Quiet page-system buttons (desktop). */
  wide?: boolean;
  suggestions: StackSuggestion[];
  jxl: boolean;
  busy: boolean;
  /** Some photos are not analysed yet: more suggestions may appear. */
  pending: boolean;
  onStack: (suggestion: StackSuggestion) => void;
  onDismiss: (suggestion: StackSuggestion) => void;
}

/**
 * The server's proposals to stack bursts (and RAW+JPEG pairs): the photos in a row with the proposed primary marked,
 * "Stack" to apply and "Not a stack" to stop proposing that set. Nothing is stacked without a click.
 */
export function StackSuggestionList(props: StackSuggestionListProps) {
  return (
    <section class="dd-bursts" aria-label={t("galleryDedup.burstsTitle")}>
      <Show when={props.pending}>
        <p class="dd-note">{t("galleryDedup.burstsPending")}</p>
      </Show>
      <ul class="dd-suggestions">
        <For each={props.suggestions}>
          {(s) => (
            <li class="dd-suggestion">
              <div class="dd-suggestion__head">
                <strong>{t(`galleryDedup.suggestion_${s.kind}` as MessageKey)}</strong>
                <span>{t("galleryDedup.suggestionCount", { count: s.items.length })}</span>
                <Show when={s.kind === "burst" && s.spanMs > 0}>
                  <span>{spanSeconds(s.spanMs)} s</span>
                </Show>
                <span class="dd-suggestion__why">{t(`galleryDedup.evidence_${s.evidence}` as MessageKey)}</span>
              </div>
              <ul class="dd-strip" aria-label={t("galleryDedup.suggestionCount", { count: s.items.length })}>
                <For each={s.items}>
                  {(item) => (
                    <li class="dd-strip__item" classList={{ "dd-strip__item--primary": item.id === s.primary }} title={itemName(item)}>
                      <Show when={tileUrl(item, props.jxl)}>{(src) => <img src={src()} alt={itemName(item)} draggable={false} loading="lazy" />}</Show>
                      <Show when={item.id === s.primary}>
                        <span class="dd-strip__mark">{t("galleryDedup.primary")}</span>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
              <div class="dd-suggestion__actions">
                <button type="button" class={props.wide ? "pg-btn" : "btn btn--primary"} disabled={props.busy} onClick={() => props.onStack(s)}>
                  {t("galleryDedup.stackIt")}
                </button>
                <button type="button" class={props.wide ? "pg-btn pg-btn--ghost" : "btn"} disabled={props.busy} onClick={() => props.onDismiss(s)}>
                  {t("galleryDedup.notAStack")}
                </button>
              </div>
            </li>
          )}
        </For>
      </ul>
    </section>
  );
}
