# Shopping items are labels (owner rule, 2026-11-08)

Owner: "items in the shopping list should just be labels, and gets associated; that should be a not
recognized item unless I add it manually."

## Rule 1: exact catalog match only

A typed item becomes a catalog item (its id, art, emoji and section) **only** when its folded text
equals a catalog name in any language (`names.it|en|da`, `name`) or one of its aliases (any
language, `all` included). Folding is the catalog's `fold()` (lowercase, æ→ae ø→oe å→aa œ→oe ß→ss
ð→d þ→th ł→l, strip diacritics, runs of non-letters/digits → one space, trim). Nothing else links:
no prefix, suffix, substring, word window, head/tail words, plural `s` stripping, article
stripping, product-alias substring, and no model (Jev or LLM) choice of catalog item or section.

Everything else is an **unrecognized item**: the typed text is its label, section `other`, no icon
(clients draw the neutral fallback: the first letter). The owner links it by hand (Rename to a
catalog name, Change section in the item sheet). The owner's own custom objects still match by
their exact folded names/aliases (he made them).

Catalog *search* (the suggestion list while typing, ranked by prefix/word-start/substring) stays: it
only offers items, the owner picks one.

## Rule 2: never one item made of a sentence

Every free-text entry point (web search box, iOS search box, Lucina's `shopping` tool, the
`shopping_add` MCP tool, the legacy `POST /v1/shopping`) runs the text through `split_items` first
and adds one item per part.

`split_items(text, is_known) -> [ItemPart { name, quantity: Option<u32>, unit: Option<String> }]`
where `is_known(s)` is "the folded `s` is an exact catalog term" (Rule 1). Steps:

1. Collapse whitespace runs (spaces, tabs, newlines count as whitespace only for collapsing *after*
   step 4 splits on newlines; implement by first treating `\n` and `\r` as `,`). Trim. Empty → `[]`.
2. If `is_known(text)` → one part, name = text, no quantity.
3. **Strip command words.** Compare words by `fold_word` (lowercase, the æøåœß table above, strip
   accents, strip leading/trailing `: , . ; ! ? " ' “ ” «»` from the word). Repeat until nothing
   changes:
   - at the start: a VERB (`add`, `please`, `put`, `buy`, `aggiungi`, `aggiungere`, `metti`,
     `compra`, `comprare`, `prendi`, `tilfoej`, `koeb`, `saet`, `husk`), or a LIST phrase;
   - at the end: a LIST phrase, or `please`, `grazie`, `tak`.
   A LIST phrase is `[PREP] [DET] LIST` where
   - PREP ∈ `to on onto in into for alla alle nella sulla sulle per a til paa i`,
   - DET ∈ `the my la mia min`,
   - LIST ∈ the word sequences `shopping list`, `grocery list`, `list`, `shopping`, `lista della
     spesa`, `lista spesa`, `lista`, `spesa`, `indkoebslisten`, `indkoebsliste`, `listen`,
     `indkoebssedlen`;
   - PREP may be absent only for the multi-word or unambiguous LISTs: `shopping list`, `grocery
     list`, `lista della spesa`, `lista spesa`, `indkoebslisten`, `indkoebsliste`.
   - Longest match wins.
   Never strip so that no word is left: if a strip would empty the text, stop and keep what is left
   before that strip.
4. **Split** on `,` `;` and line breaks. Then split each segment on a conjunction word (a whole
   word whose fold is `e`, `ed`, `and`, `og`, `&`, `+`) — unless `is_known(segment)`, which keeps
   the segment whole (`sale e pepe`, `fish and chips` when they are catalog terms). A conjunction
   splits only when both sides are non-empty; otherwise it stays part of the text.
5. **Each part**: trim; trim leading/trailing `. ; : ! ? " ' “ ” « »`; empty → dropped.
   If `is_known(part)` → name = part, no quantity. Else **quantity**:
   - leading: first word is `N`, `Nx`, `N×`, `xN` or `×N` (N = 1-4 ASCII digits, value 1..=9999),
     or `N` followed by the word `x`/`×`;
   - else trailing: last word is `xN` or `×N`.
   After a leading quantity, if the next word folds to a UNIT (`g gr hg kg l lt dl cl ml pz pcs pc
   stk`), it becomes `unit` (as the folded unit); after a unit, a word `di`, `d`, `of` is dropped.
   Only when the remaining name is non-empty are quantity/unit taken; otherwise the whole part is
   the name with no quantity. N = 0 or N > 9999 or a decimal (`1.5`) → no quantity, text unchanged.
6. Keep at most `MAX_PARTS = 50` parts, in order. Names keep the owner's original spelling and case
   (only trimmed); quantities never invent a name.

No singularising: `4 baguettes` is `{name: "baguettes", quantity: 4}`; it links to `baguette`
only because `baguettes` is a catalog alias. Plural forms belong in the catalog as aliases.

The shared cases are `tests/fixtures/shopping-split-cases.json`: `known` is the list of strings
`is_known` accepts (folded equality), `cases[].in` the text, `cases[].out` the parts. Rust, web
and iOS implementations must pass every case.

Implementations: Rust `crates/shopping-text` (pure, no deps; Verus model
`verification/verus/shopping_text.rs`), web `web/src/lib/shoppingText.ts`, iOS
`ios/Secretary/Features/Shopping/ShoppingText.swift`.
