/**
 * Bundled item and section illustrations (assets/icons, docs/design/item-icons.md).
 *
 * Vite emits every SVG as its own hashed file under /assets/ (`?no-inline`, so nothing is
 * inlined into the bundle as a data: URL). Pages show them with a same-origin <img>, which the
 * CSP already allows (img-src 'self'), which cannot run script, and which the service worker
 * caches cache-first like every other /assets/ file.
 */

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
