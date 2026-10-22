import { Show } from "solid-js";
import { itemArtUrl, sectionArtUrl } from "../../lib/itemArt";
import { catalogIndex } from "../../state/shopping";

/** Emoji shown when an `svg:<key>` icon names art this build does not have. */
const MISSING_SVG = "🛒";

function emoji(icon: string): string {
  return icon.startsWith("svg:") ? MISSING_SVG : icon;
}

function Art(props: { src: string }) {
  return <img class="item-art" src={props.src} alt="" draggable={false} decoding="async" />;
}

/** An item's icon: its bundled illustration when there is one, else its emoji. Sized by font-size. */
export function ItemIcon(props: { icon: string; catalogId?: string | null | undefined }) {
  const url = () =>
    itemArtUrl(props.icon, props.catalogId, props.catalogId ? catalogIndex().byId.get(props.catalogId)?.icon : undefined);
  return (
    <Show when={url()} fallback={emoji(props.icon)}>
      {(src) => <Art src={src()} />}
    </Show>
  );
}

/** A catalog section's icon: its bundled illustration when there is one, else its emoji. */
export function SectionIcon(props: { id: string; icon: string }) {
  return (
    <Show when={sectionArtUrl(props.id, props.icon)} fallback={emoji(props.icon)}>
      {(src) => <Art src={src()} />}
    </Show>
  );
}
