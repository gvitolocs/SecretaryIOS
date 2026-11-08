import { Show } from "solid-js";
import { t } from "../../../i18n";
import type { SearchMode } from "../../../lib/gallerySmart";
import "../../../styles/gallery-smart.css";

export interface SmartHeaderProps {
  /** The words searched (null in the "Similar photos" view). */
  query: string | null;
  /** The picture the results look like (null for a search). */
  similarTo: string | null;
  mode: SearchMode;
  /** The results are ordered by meaning right now (so the hint applies). */
  smart: boolean;
  /** How many photos the index has looked at, when it has not reached all of them; else null. */
  looked: number | null;
  onMode: (mode: SearchMode) => void;
  onClearSimilar: () => void;
}

/**
 * Above the results of a search or of "Similar photos": what they are for, the Words / Smart switch, and (while the
 * index is still catching up) how many photos smart search has looked at so far.
 */
export function SmartHeader(props: SmartHeaderProps) {
  const seg = (value: SearchMode, label: string) => (
    <button type="button" class="g-seg__btn" aria-pressed={props.mode === value} onClick={() => props.onMode(value)}>
      {label}
    </button>
  );
  return (
    <div class="gs-head">
      <p class="gs-title" role="status">
        <Show when={props.similarTo !== null} fallback={t("gallerySmart.resultsFor", { query: props.query ?? "" })}>
          {props.similarTo ? t("gallerySmart.similarTo", { name: props.similarTo }) : t("gallerySmart.similarToThis")}
        </Show>
      </p>
      <Show
        when={props.similarTo === null}
        fallback={
          <button type="button" class="g-chip" onClick={props.onClearSimilar}>
            {t("gallerySmart.showAll")}
          </button>
        }
      >
        <div class="g-seg" role="group" aria-label={t("gallerySmart.modeLabel")}>
          {seg("words", t("gallerySmart.modeWords"))}
          {seg("smart", t("gallerySmart.modeSmart"))}
        </div>
      </Show>
      <Show when={props.looked !== null}>
        <p class="gs-hint">{t("gallerySmart.hint", { count: props.looked ?? 0 })}</p>
      </Show>
    </div>
  );
}
