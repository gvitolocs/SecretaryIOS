# Shopping catalog

Secretary's own open shopping catalog: about 400 grocery and household items in Italian, English and Danish, grouped into 17
sections, each with an emoji icon. It backs `GET /v1/shopping/catalog?lang=it|en|da` (see `docs/shopping-v2.md`) and client-side
search/autocomplete.

The content is original to this project, written from general knowledge. It isn't derived from any third-party catalog,
names list or icon set. In particular, nothing comes from Bring! or from `bring-apk-analysis`. Keep it that way: don't paste
lists from other apps or shops.

## Files

| File | What | Edited by |
| --- | --- | --- |
| `assets/catalog/catalog.v1.json` | Master: all languages, per-language aliases | hand |
| `assets/catalog/catalog.{it,en,da}.json` | Localised files in the contract shape, served statically | generated |
| `tools/catalog-check/` | Rust validator and generator (standalone crate, not in the workspace) | hand |

Never edit the localised files by hand. Change the master, then regenerate:

```sh
cd tools/catalog-check
cargo run -- --write     # validate the master, then rewrite catalog.{it,en,da}.json
cargo run                # validate only; fails if the localised files are stale (CI / pre-commit)
cargo test               # normalisation and icon unit tests
```

The validator exits 1 on any error and prints per-section stats: items, items using the section fallback icon, and alias
counts per language.

## Master format (`catalog.v1.json`)

```json
{ "version": 1, "note": "…",
  "sections": [ { "id": "produce", "names": {"it":"Frutta e verdura","en":"Fruit & vegetables","da":"Frugt og grønt"},
                  "icon": "🥦", "order": 1 } ],
  "items": [ { "id": "banana", "names": {"it":"Banane","en":"Bananas","da":"Bananer"},
               "aliases": {"it":["banana"], "en":["banana"], "da":["banan"], "all":[]},
               "section": "produce", "icon": "🍌" } ] }
```

- **`id`**: stable kebab-case English, unique. It's stored as `catalogId` on list rows, so **never rename or reuse an id**.
  Remove an item only if no lists use it.
- **`names`**: the display name in each language. All three are required. Use the plural that a shopping list would show
  ("Banane", "Bananas", "Bananer"), or a mass noun ("Latte", "Milk", "Mælk").
- **`aliases`**: what people type besides the name: singular/plural forms, common variants and sub-types ("basmati" →
  rice), regional words, and brands used as generic names ("nutella" → hazelnut spread, "lurpak" → butter). Aliases
  contain words only, never logos or trademark symbols.
  - `it`, `en`, `da`: per language, all optional.
  - `all`: language-neutral terms (loanwords, international brands, pasta shapes); merged into every language.
  - Variants that are really the same product belong here, not in a new item. For example, spaghetti and penne are
    aliases of `pasta`.
- **`section`**: a section `id`.
- **`icon`**: see the icon policy below.
- **Sections**: `id`, `names` (it/en/da), `icon`, `order` (unique). The order follows a typical walk through the shop.

**Lucina compatibility.** Lucina's canonical grocery products (`Lucina/src/grocery/products.js`) map 1:1 to catalog ids,
with `_` replaced by `-`. For example, `chicken_breast` ↔ `chicken-breast` and `rye_bread` ↔ `rye-bread`. The validator
checks that all 11 exist and that every Lucina alias resolves to the same item. If Lucina gains a product, add it to the
`LUCINA` table in `tools/catalog-check/src/main.rs` and to the catalog.

## Localised format (`catalog.<lang>.json`)

This is exactly the contract shape: `{version, lang, sections:[{id,name,icon,order}], items:[{id,name,names{en,it,da},aliases,section,icon}]}`.

- Sections are sorted by `order`. Items keep master order, which is grouped by section.
- `aliases` for a language are that language's aliases followed by the `all` aliases. They're de-duplicated by normalised
  form, and any alias equal to the item's own name is dropped.
- `names` carries all three languages, so clients can also match a name in another language. This matters for an Italian
  speaker in Denmark typing "rugbrød" in the Italian UI.
- The file is written with one section or item per line, so diffs stay small. It's still a single JSON document.

## Adding items

1. Check that the item doesn't already exist, under any name or alias in any language. Search the master for the
   normalised word. If it does exist, add an alias instead.
2. Add one line in the right section block of `catalog.v1.json`, with an id, all three names, aliases, the section and
   an icon.
3. Run `cargo run -- --write` in `tools/catalog-check`. Fix any errors it reports:
   - **collision**: the same normalised term is used by two items in one language. Decide which item owns the word, or
     make the alias more specific.
   - **missing name**, **unknown section** or **bad icon**.
   - **redundant alias** (a warning): the alias equals the name, or another alias, after normalisation. Remove it.
4. Commit the master and the three regenerated files together.

Priorities for content: Danish supermarket staples (rugbrød, leverpostej, skyr, remoulade, ristede løg, frikadeller,
pålægschokolade, …), Italian staples (passata, parmigiano, guanciale, pecorino, …), then universal items.

## Icon policy

- `icon` is either **one Unicode emoji** (exactly one extended grapheme cluster; ZWJ sequences and VS16 are fine) or
  **`svg:<kebab-key>`** for a bundled open-licence SVG.
- Pick the **most specific** emoji available (🥦 broccoli, 🍋 lemon, 🧈 butter). If no emoji fits, use a close relative
  that is not misleading (🍓 for raspberries, 🧅 for leeks). Otherwise fall back to the **section emoji**. The validator
  reports the fallback count per section.
- Stay within **Emoji 15.0** (iOS 16.4+, Android 14+). Newer emoji (15.1+ such as 🍋‍🟩 lime, or 16.0 such as 🫜 root
  vegetable) render as two glyphs or as tofu on older devices.
- **Emoji render from the platform font, so nothing is bundled.** That keeps the app and repo licence-clean, because no
  third-party artwork ships with us. The trade-off is that the look varies by platform (Apple, Google, Microsoft), which
  is fine.
- **Bundled illustrations** (pilot, branch `item-icons`): original SVGs in `assets/icons/items/<catalogId>.svg` and
  `assets/icons/sections/<sectionId>.svg`, drawn to `docs/design/item-icons.md`. The web shows them in place of the
  catalog emoji; the catalog keeps its emoji for every other client. An explicit `svg:<key>` icon names one directly.
- An optional open SVG set can come later, via `svg:<key>` icons. Candidates are **Fluent Emoji** (Microsoft, MIT) and
  **Noto Emoji** (Google, Apache-2.0). Bundle the chosen files with their licence text, and keep an emoji fallback for
  clients without the set. Never use proprietary icon sets (Bring!, shop apps, Apple's emoji images as files).

## Search normalisation (clients must match this)

Apply the same `normalise()` to the query and to every name and alias. This is the function in
`tools/catalog-check/src/main.rs`, which the uniqueness check uses:

1. **Lowercase** (Unicode-aware).
2. **Fold special letters**: `æ→ae`, `ø→oe`, `å→aa`, `œ→oe`, `ß→ss` (also `ð→d`, `þ→th`, `ł→l`).
3. **Strip diacritics**: decompose to NFD and drop the combining marks. So `è é ê ë → e`, `à → a`, `ü → u`,
   `ñ → n`, `ç → c`, and so on.
4. **Separators**: every run of non-letters/digits (spaces, `-`, `'`, `’`, `/`, `&`, `.`, `%`) becomes one space. Then
   trim.

For example, `"Rugbrød"` → `rugbroed`, `"Caffè"` → `caffe`, `"olio d’oliva"` → `olio d oliva`, `"Coca-Cola"` →
`coca cola`. Folding å→aa means a user typing `blabaer` won't match "blåbær" (`blaabaer`). Clients may additionally try
a fold of `å→a` on the query only, as a fallback.

Ranking for a query `q` (normalised), within the UI language first, then the other languages' `names`:

1. **Exact** match on the name.
2. **Name, word-start**: the name starts with `q`, or a word of the name starts with `q` (`"tom"` → *Tomatoes*,
   *Cherry tomatoes*). A whole-name prefix ranks above a later word.
3. **Alias**: the same exact and word-start tests against `aliases`.
4. **Substring** anywhere (optional, lowest).
5. Within a tier, boost by the user's own frequency or recency (items on `recent`), then by shorter names.

Don't match inside a word at tiers 1–3, so "ham" must not match "shampoo" (Lucina had this exact bug). When the hit came
from an alias that is a sub-type (for example "basmati" → *Rice*), clients may put the typed alias into the row's `spec`,
so the list shows *Rice · basmati*.

Free text that matches nothing is still a valid item: `add {name}` with no `catalogId`. The server and clients file it under
`other` unless the user picks a section.
