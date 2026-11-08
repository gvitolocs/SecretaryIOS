// People and pets share the Gallery's grouping pages: which one a page or component is about, its texts and its paths.

import { createContext, useContext } from "solid-js";
import type { GroupKind } from "../api/gallery";
import { t } from "../i18n";
import type { galleryPeople } from "../i18n/dict/galleryPeople";

const Kind = createContext<GroupKind>("people");

/** Wraps a page about pets; everything inside reads it with [`useGroupKind`]. Without it, a page is about people. */
export const GroupKindProvider = Kind.Provider;
export const useGroupKind = (): GroupKind => useContext(Kind);

type Key = Extract<keyof (typeof galleryPeople)["en"], string>;
type Params = Parameters<typeof t>[1];

/** `t` for one kind: the same keys in `galleryPeople` and `galleryPets`. */
export const groupText =
  (kind: GroupKind) =>
  (key: Key, params?: Params): string =>
    t(`${kind === "pets" ? "galleryPets" : "galleryPeople"}.${key}` as Parameters<typeof t>[0], params);

export const groupPath = (kind: GroupKind): string => (kind === "pets" ? "/gallery/pets" : "/gallery/people");
export const memberPath = (kind: GroupKind, id: string): string =>
  `${kind === "pets" ? "/gallery/pet/" : "/gallery/person/"}${encodeURIComponent(id)}`;
