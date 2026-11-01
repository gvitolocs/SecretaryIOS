import { For, Show } from "solid-js";
import type { MessageKey } from "../../i18n";
import { t } from "../../i18n";
import { formatMs } from "../../lib/voice";

// The Jev layers that decided a voice turn (docs/lucinavoice-protocol.md section 9) as small chips:
// "Intent: chat", "Vocabulary: Skibigårdsvej". A layer that was computed but not acted on is outlined;
// the title carries the confidence and the time. Used by the device page and the Lucina Voice chat.

export interface LayerLike {
  layer: string;
  verdict?: string | null;
  confidence?: number | null;
  ms?: number | null;
  used?: boolean;
}

const NAMES: Record<string, MessageKey> = {
  intent: "voice.layerIntent",
  slots: "voice.layerSlots",
  confirm: "voice.layerConfirm",
  vocab: "voice.layerVocab",
};

export function layerName(layer: string): string {
  const key = NAMES[layer];
  return key ? t(key) : layer;
}

function detail(l: LayerLike): string {
  const parts = [layerName(l.layer)];
  if (l.verdict) parts.push(l.verdict);
  if (typeof l.confidence === "number") parts.push(t("voice.layerConfidence", { percent: Math.round(l.confidence * 100) }));
  if (typeof l.ms === "number") parts.push(formatMs(l.ms));
  parts.push(l.used === false ? t("voice.layerNotUsed") : t("voice.layerUsed"));
  return parts.join(" · ");
}

export function LayerChips(props: { layers: readonly LayerLike[] | undefined }) {
  return (
    <Show when={props.layers?.length}>
      <ul class="vd-layers" aria-label={t("voice.layers")}>
        <For each={props.layers}>
          {(l) => (
            <li class="vd-layer" data-layer={l.layer} data-used={l.used === false ? "false" : "true"} title={detail(l)}>
              <span class="vd-layer-name">{layerName(l.layer)}</span>
              <Show when={l.verdict}>
                <span class="vd-layer-verdict">{l.verdict}</span>
              </Show>
              <span class="sr-only">{detail(l)}</span>
            </li>
          )}
        </For>
      </ul>
    </Show>
  );
}
