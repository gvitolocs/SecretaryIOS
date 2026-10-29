// The automatic albums in the Gallery, with no DOM: where their pages live, the names to show, and which corrections the
// info panel offers for a photo.

import { GALLERY_CATEGORIES, isGalleryCategory, type GalleryCategory, type ItemCategory } from "../api/gallery";
import type { MessageKey } from "../i18n";

export const categoryPath = (category: GalleryCategory): string => `/gallery/category/${category}`;

/** The category a `/gallery/category/:cat` address names; null for anything that is not one. */
export function categoryFromParam(param: string | undefined): GalleryCategory | null {
  return isGalleryCategory(param) ? param : null;
}

const cap = (c: GalleryCategory): string => c.charAt(0).toUpperCase() + c.slice(1);

/** The album's name ("Receipts"). */
export const albumKey = (c: GalleryCategory): MessageKey => `galleryCategories.album${cap(c)}` as MessageKey;
/** What one photo is ("Receipt"). */
export const kindKey = (c: GalleryCategory): MessageKey => `galleryCategories.kind${cap(c)}` as MessageKey;
/** "Not a receipt". */
export const notKey = (c: GalleryCategory): MessageKey => `galleryCategories.not${cap(c)}` as MessageKey;
/** "Move to Receipts". */
export const moveKey = (c: GalleryCategory): MessageKey => `galleryCategories.move${cap(c)}` as MessageKey;

/** One entry of the correction picker. */
export type Correction =
  | { kind: "not"; from: GalleryCategory; to: "photo" }
  | { kind: "move"; to: GalleryCategory }
  | { kind: "auto" };

/**
 * The corrections offered for a photo: "Not a …" (it is an ordinary photo) first unless it already is one, then "Move to …"
 * for each other category, then "Back to automatic" when the category is the owner's own choice.
 */
export function corrections(current: ItemCategory): Correction[] {
  const out: Correction[] = [];
  if (current.category !== "photo") out.push({ kind: "not", from: current.category, to: "photo" });
  for (const c of GALLERY_CATEGORIES) {
    if (c === current.category || c === "photo") continue;
    out.push({ kind: "move", to: c });
  }
  if (current.decidedBy === "owner") out.push({ kind: "auto" });
  return out;
}
