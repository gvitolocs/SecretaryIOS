# i18n

Languages: English (`en`), Italian (`it`), Danish (`da`).

**How the language is chosen**
- By default it follows the device: the first supported entry of `navigator.languages`, falling back to English.
- The owner can pick a language in the account panel. The choice is stored in localStorage (`secretary.locale`), and "Automatic" removes it.
- `<html lang>` follows the language in use.

## Use

```tsx
import { t } from "../i18n";

<button aria-label={t("chat.send")}>…</button>
t("login.tryAgainIn", { time: "0:42" })        // "Try again in 0:42."
t("shell.passwordHint", { count: 8 })          // plural: "At least 8 characters."
```

- Keys are typed (`feature.key`): a typo is a type error.
- `t` reads a signal, so text in JSX updates when the language changes. Outside JSX, call `t` when the text is needed rather than once at module load.
- `{name}` placeholders are filled from the params. Numbers are formatted for the language, so 4200 shows as "4.200" in Danish.
- If a translation is missing at runtime, `t` falls back to English, and then to the key itself.

## Add strings for a feature

1. Create `dict/<feature>.ts`. English comes first and is the source of truth. `it` and `da` must have exactly the same keys, which `FeatureDictionary<typeof en>` checks:

   ```ts
   import type { FeatureDictionary } from "../types";

   const en = {
     title: "Gym",
     sets: { one: "{count} set", other: "{count} sets" },
   };

   export const gym: FeatureDictionary<typeof en> = {
     en,
     it: { title: "Palestra", sets: { one: "{count} serie", other: "{count} serie" } },
     da: { title: "Træning", sets: { one: "{count} sæt", other: "{count} sæt" } },
   };
   ```

2. List it in `dictionaries.ts`.

Plurals use `Intl.PluralRules` categories (`one`, `other`, plus `few`/`many` where a language needs them). An optional `zero` is used for exactly 0.

## Other helpers

- `locale()`: the current language.
- `setLocale(l | null)`: pick a language, or `null` to follow the device.
- `localeOverride()` and `detectedLocale()`: for pickers.
- `speechLang()`: the speech tag (`en-US` / `it-IT` / `da-DK`) for TTS `lang` and speech recognition.
- `LOCALE_NAMES`: each language's name in that language.
- For dates and numbers, pass `locale()` to `Intl` / `toLocale*String`. `lib/units` already does this.

## Tests

`src/test/setup.ts` calls `reloadLocale()` after each test, so every test starts in English (jsdom's `en-US`) with nothing stored. Call `setLocale("it")` inside a test to check a translation.
