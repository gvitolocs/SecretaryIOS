import { createMemo, Match, Show, Switch } from "solid-js";
import { itemVisual, sectionArtUrl } from "../../lib/itemArt";
import { catalogIndex, shop } from "../../state/shopping";

/** Emoji shown when an `svg:<key>` icon names art this build does not have. */
const MISSING_SVG = "🛒";

function emoji(icon: string): string {
  return icon.startsWith("svg:") ? MISSING_SVG : icon;
}

function Art(props: { src: string }) {
  return <img class="item-art" src={props.src} alt="" draggable={false} decoding="async" />;
}

/**
 * An item's picture (itemVisual): its illustration, the illustration its name or section
 * points to, a specific emoji, else its first letter. Sized by font-size.
 */
export function ItemIcon(props: {
  icon: string | null | undefined;
  catalogId?: string | null | undefined;
  /** The row's name, to find art for free text and custom items. */
  name?: string | undefined;
  section?: string | null | undefined;
}) {
  const visual = createMemo(() =>
    itemVisual(
      { icon: props.icon, catalogId: props.catalogId, name: props.name ?? "", section: props.section },
      catalogIndex(),
      shop.catalog.sections,
    ),
  );
  const art = () => {
    const v = visual();
    return v.kind === "art" ? v.url : null;
  };
  const glyph = () => {
    const v = visual();
    return v.kind === "emoji" ? v.emoji : null;
  };
  const letter = () => {
    const v = visual();
    return v.kind === "initial" ? v.letter : null;
  };
  return (
    <Switch>
      <Match when={art()}>{(src) => <Art src={src()} />}</Match>
      <Match when={glyph()}>{(e) => <span class="item-emoji">{e()}</span>}</Match>
      <Match when={letter()}>{(l) => <span class="tile-initial">{l()}</span>}</Match>
    </Switch>
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
