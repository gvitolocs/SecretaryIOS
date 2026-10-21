import { locale, t, type Locale, type Params } from "../../i18n";
import type { gym } from "../../i18n/dict/gym";

// Gym strings live in the shared i18n module as the `gym` dictionary
// (src/i18n/dict/gym.ts: en from the original Lucina Gym, plus it and da).
// `gt("startEmpty")` is `t("gym.startEmpty")` with the feature prefix typed.

export type GymKey = keyof typeof gym.en;
export type GymParams = Params;

export function gt(key: GymKey, params?: GymParams): string {
  return t(`gym.${key}`, params);
}

/** The language in use, for Intl number formatting in the Gym. */
export function gymLocale(): Locale {
  return locale();
}
