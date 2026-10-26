// Tiny i18n for the Secretary web app. See README.md.
//
//   import { t } from "../i18n";
//   t("chat.send")                       -> "Send"
//   t("chat.newMessages", { count: 3 })  -> "3 new messages" (plural)
//
// The language follows the device (navigator.languages) unless the owner
// picks one in the account panel. `t` reads a signal, so JSX re-renders when
// the language changes.

import { createSignal } from "solid-js";
import { readString, removeKey, writeString } from "../lib/storage";
import { dictionaries, type Dictionaries } from "./dictionaries";
import { LOCALES, type Locale, type Message, type Plural } from "./types";

export { LOCALES, type Locale } from "./types";

export const DEFAULT_LOCALE: Locale = "en";
/** Each language's own name, for the language picker. */
export const LOCALE_NAMES: Record<Locale, string> = { en: "English", it: "Italiano", da: "Dansk" };
/** BCP 47 tags for speech: TTS `lang` and speech recognition. */
export const SPEECH_LANGS: Record<Locale, string> = { en: "en-US", it: "it-IT", da: "da-DK" };
export const LOCALE_KEY = "secretary.locale";

type Feature = keyof Dictionaries;
/** Every valid key, e.g. "chat.send". */
export type MessageKey = {
  [F in Feature]: `${F}.${Extract<keyof Dictionaries[F]["en"], string>}`;
}[Feature];
export type Params = Record<string, string | number>;

function isLocale(v: unknown): v is Locale {
  return typeof v === "string" && (LOCALES as readonly string[]).includes(v);
}

/** "it-IT" -> "it"; null for languages we don't have. Norwegian/Swedish are not Danish. */
export function matchLocale(tag: string): Locale | null {
  const base = tag.trim().toLowerCase().split(/[-_]/)[0] ?? "";
  return isLocale(base) ? base : null;
}

function browserLanguages(): readonly string[] {
  try {
    if (typeof navigator === "undefined") return [];
    if (navigator.languages?.length) return navigator.languages;
    return navigator.language ? [navigator.language] : [];
  } catch {
    return [];
  }
}

/** The first supported language in the user's preference list, else English. */
export function detectLocale(languages: readonly string[] = browserLanguages()): Locale {
  for (const tag of languages) {
    const l = matchLocale(tag);
    if (l) return l;
  }
  return DEFAULT_LOCALE;
}

function storedOverride(): Locale | null {
  const v = readString("local", LOCALE_KEY);
  return isLocale(v) ? v : null;
}

const [override, setOverride] = createSignal<Locale | null>(storedOverride());
const [detected, setDetected] = createSignal<Locale>(detectLocale());

/** The language in use. */
export function locale(): Locale {
  return override() ?? detected();
}

/** The owner's explicit choice, or null when following the device. */
export function localeOverride(): Locale | null {
  return override();
}

/** The device's language (what "Automatic" means). */
export function detectedLocale(): Locale {
  return detected();
}

/** Speech tag for the current language, e.g. "it-IT". */
export function speechLang(): string {
  return SPEECH_LANGS[locale()];
}

function syncHtmlLang(): void {
  try {
    if (typeof document !== "undefined") document.documentElement.lang = locale();
  } catch {
    // Ignore.
  }
}

/** Pick a language (stored in localStorage), or null to follow the device again. */
export function setLocale(next: Locale | null): void {
  setOverride(next);
  if (next) writeString("local", LOCALE_KEY, next);
  else removeKey("local", LOCALE_KEY);
  syncHtmlLang();
}

/** Re-read the device languages and the stored choice (tests; `languagechange`). */
export function reloadLocale(languages?: readonly string[]): void {
  setDetected(detectLocale(languages));
  setOverride(storedOverride());
  syncHtmlLang();
}

if (typeof window !== "undefined") {
  try {
    window.addEventListener("languagechange", () => reloadLocale());
  } catch {
    // Ignore.
  }
}
syncHtmlLang();

const pluralRules = new Map<Locale, Intl.PluralRules>();

function plural(msg: Plural, loc: Locale, count: number): string {
  if (count === 0 && msg.zero !== undefined) return msg.zero;
  let rules = pluralRules.get(loc);
  if (!rules) {
    rules = new Intl.PluralRules(loc);
    pluralRules.set(loc, rules);
  }
  const cat = rules.select(count) as keyof Plural;
  return msg[cat] ?? msg.other;
}

function interpolate(text: string, loc: Locale, params: Params | undefined): string {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => {
    const v = params[name];
    if (v === undefined) return whole;
    return typeof v === "number" ? new Intl.NumberFormat(loc).format(v) : v;
  });
}

/**
 * The message for `key` in the current language (English if missing), with
 * `{name}` placeholders filled from `params`. Plural messages use
 * `params.count`.
 */
export function t(key: MessageKey, params?: Params): string {
  const loc = locale();
  const dot = key.indexOf(".");
  const dict = dictionaries[key.slice(0, dot) as Feature] as Record<Locale, Record<string, Message | undefined>>;
  const name = key.slice(dot + 1);
  const msg = dict[loc]?.[name] ?? dict.en[name];
  if (msg === undefined) return key;
  const text = typeof msg === "string" ? msg : plural(msg, loc, Number(params?.count ?? 0));
  return interpolate(text, loc, params);
}
