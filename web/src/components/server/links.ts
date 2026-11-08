import { attentionFor, type AttentionItem } from "../../api/systemOverview";

// The Server page's sheets live in the URL (?item=, ?host=, ?component=) so they survive a reload
// and the back button closes them. `base` is the current section's path.

export interface Links {
  base: string;
  attention: AttentionItem[];
  /** Component names by id, for ids the endpoint gives without a name. */
  names: Map<string, string>;
}

export const SERVICES_PATH = "/server-health/services";

export const itemHref = (l: Links, key: string, base = l.base) => `${base}?item=${encodeURIComponent(key)}`;
export const hostHref = (l: Links, id: string) => `${l.base}?host=${encodeURIComponent(id)}`;

/** A component opens its attention item when it has one, else its own small sheet. */
export function componentHref(l: Links, id: string, base = l.base): string {
  const item = attentionFor(l.attention, id);
  return item ? itemHref(l, item.key, base) : `${base}?component=${encodeURIComponent(id)}`;
}
