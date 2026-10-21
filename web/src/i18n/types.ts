// Shared types for the dictionaries. English is the source of truth: every
// other language must define exactly the same keys (checked by `satisfies`).

export const LOCALES = ["en", "it", "da"] as const;
export type Locale = (typeof LOCALES)[number];

/** A plural message: Intl.PluralRules categories, plus an optional exact zero. */
export interface Plural {
  zero?: string;
  one: string;
  two?: string;
  few?: string;
  many?: string;
  other: string;
}

export type Message = string | Plural;

/** The same keys as the English dictionary `E`, each with a message. */
export type Messages<E> = { [K in keyof E]: Message };

/** One feature's dictionary in every language. */
export type FeatureDictionary<E> = { en: E } & { [L in Exclude<Locale, "en">]: Messages<E> };
