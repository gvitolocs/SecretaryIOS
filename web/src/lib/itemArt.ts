/**
 * Bundled item and section illustrations (assets/icons, docs/design/item-icons.md).
 *
 * Vite emits every SVG as its own hashed file under /assets/ (`?no-inline`, so nothing is
 * inlined into the bundle as a data: URL). Pages show them with a same-origin <img>, which the
 * CSP already allows (img-src 'self'), which cannot run script, and which the service worker
 * caches cache-first like every other /assets/ file.
 */

import {
  DEFAULT_ICON,
  resolveByName,
  OTHER_SECTION,
  type CatalogItem,
  type CatalogSection,
  type SearchIndex,
} from "./shoppingCatalog";

const itemFiles = import.meta.glob<string>("../../../assets/icons/items/*.svg", {
  query: "?no-inline",
  import: "default",
  eager: true,
});
const sectionFiles = import.meta.glob<string>("../../../assets/icons/sections/*.svg", {
  query: "?no-inline",
  import: "default",
  eager: true,
});

function byKey(files: Record<string, string>): Map<string, string> {
  const out = new Map<string, string>();
  for (const [path, url] of Object.entries(files)) out.set(path.slice(path.lastIndexOf("/") + 1, -".svg".length), url);
  return out;
}

const ITEM_ART = byKey(itemFiles);
const SECTION_ART = byKey(sectionFiles);

/** URL of a `svg:<key>` icon, or null for an emoji or an unknown key. */
export function svgIconUrl(icon: string | null | undefined): string | null {
  if (!icon?.startsWith("svg:")) return null;
  const key = icon.slice("svg:".length);
  return ITEM_ART.get(key) ?? SECTION_ART.get(key) ?? null;
}

/**
 * Illustration for an item: an explicit `svg:<key>` icon, else the bundled art of its catalog
 * entry. A row whose emoji differs from the catalog's (picked by the user) keeps that emoji.
 */
export function itemArtUrl(icon: string | null | undefined, catalogId: string | null | undefined, catalogIcon?: string): string | null {
  const explicit = svgIconUrl(icon);
  if (explicit) return explicit;
  if (!catalogId) return null;
  if (icon && catalogIcon !== undefined && icon !== catalogIcon) return null;
  return ITEM_ART.get(catalogId) ?? null;
}

/** Illustration for a catalog section, or null. */
export function sectionArtUrl(sectionId: string, icon?: string | null): string | null {
  return svgIconUrl(icon) ?? SECTION_ART.get(sectionId) ?? null;
}

/** What a tile shows: a bundled illustration, an emoji, or the name's first letter. */
export type ItemVisual = { kind: "art"; url: string } | { kind: "emoji"; emoji: string } | { kind: "initial"; letter: string };

export interface VisualRow {
  /** The row's icon: an emoji, `svg:<key>`, or none. */
  icon: string | null | undefined;
  catalogId?: string | null | undefined;
  name: string;
  /** The row's own section (a custom item's, or a section override). */
  section?: string | null | undefined;
  /** `icon` was hand-picked by the owner (server `iconPicked`); only then does it beat the catalog art. */
  iconPicked?: boolean | undefined;
}

function initialOf(name: string): string {
  return Array.from(name.trim())[0]?.toUpperCase() ?? "?";
}

/**
 * The picture of a row, most specific first, so a list never mixes illustrations with stray
 * platform emoji when art exists:
 * 1. an explicit `svg:<key>` icon;
 * 2. the art of its catalog item, unless the owner hand-picked the row's emoji (`iconPicked`):
 *    a catalog-bound row's stored emoji is the catalog's, possibly from another catalog version;
 * 3. no catalog id (free text, a custom object): the art of the catalog item its name names
 *    exactly (resolveByName: a name or alias, never a part of the text), when that item
 *    belongs to the row's section, if the row has one (docs/design/shopping-labels.md);
 * 4. a specific emoji of the row (not the cart and not just its section's emoji);
 * 5. the art of its section ("other" excluded);
 * 6. any remaining emoji other than the cart, else the first letter of the name.
 */
export function itemVisual(row: VisualRow, index: SearchIndex | null, sections: readonly CatalogSection[]): ItemVisual {
  const explicit = svgIconUrl(row.icon);
  if (explicit) return { kind: "art", url: explicit };
  const icon = row.icon && !row.icon.startsWith("svg:") ? row.icon : null;
  let item: CatalogItem | null = null;
  if (row.catalogId) {
    item = index?.byId.get(row.catalogId) ?? null;
    const url = ITEM_ART.get(row.catalogId);
    if (url && !(row.iconPicked === true && icon && icon !== item?.icon)) return { kind: "art", url };
  } else if (index) {
    const guess = resolveByName(index, row.name)?.item ?? null;
    const fits = !row.section || row.section === OTHER_SECTION || !guess?.section || guess.section === row.section;
    const url = guess && fits ? ITEM_ART.get(guess.id) : undefined;
    if (url) return { kind: "art", url };
  }
  const section = row.section ?? item?.section ?? null;
  const sectionEmoji = section ? sections.find((s) => s.id === section)?.icon : undefined;
  if (icon && icon !== DEFAULT_ICON && icon !== sectionEmoji) return { kind: "emoji", emoji: icon };
  const sectionUrl = section && section !== OTHER_SECTION ? SECTION_ART.get(section) : undefined;
  if (sectionUrl) return { kind: "art", url: sectionUrl };
  if (icon && icon !== DEFAULT_ICON) return { kind: "emoji", emoji: icon };
  return { kind: "initial", letter: initialOf(row.name) };
}
