# Shopping v2 contract

This contract is shared by the catalog, Lucina, server, web and iOS work. It is based on the parity audit of
`lucina_shoppinglist` and the documented Bring! UX.

## Principles
- **Source of truth: Lucina's grocery Postgres.** It is already written by Telegram/Discord chat intents, dashboards
  and the price snapshot. The Secretary server is a thin authenticated proxy.
- **Write protocol: idempotent ops** (`opId`) with a list `revision`, using the same semantics as `secretary-core::shopping`
  (op-id dedup; deleted stays deleted). Clients keep an outbox and replay it.
- **Status semantics:**
  - `needed`: on the list, shown as tiles.
  - `recent`: bought recently, shown as quick re-add tiles.
  - `removed`: deleted, never shown.
- **Every row is an item object** (below) with names in every language; clients show the name in their UI language.
- **One active row per (list, item object):** adding something that is already on the list in any language ("latte" when
  "milk" is there) returns that row; if it was `recent`, it moves back to `needed`.
- **No proprietary Bring assets.** Use our own catalog (`assets/catalog/catalog.v1.json`) and open-licence icons only.

## Catalog: `GET /v1/shopping/catalog?lang=it|en|da`
Static file served by secretary-server; no Lucina call.
```json
{ "version": 1, "lang": "it",
  "sections": [ { "id": "produce", "name": "Frutta e verdura", "icon": "🥦", "order": 1 } ],
  "items": [ { "id": "banana", "name": "Banane", "names": {"en":"Bananas","it":"Banane","da":"Bananer"},
               "aliases": ["banana"], "section": "produce", "icon": "🍌" } ] }
```
- `icon` is a Unicode emoji (rendered with the platform font) or `"svg:<key>"` for a bundled open-licence SVG.
- Clients search locally: diacritic-folded, word-start first, then aliases, then frequency or recent boost.

## Item objects
Every list row points to an item object with a unique id:
- **Catalog item**: `{"id": "milk", "kind": "catalog", "names": {"it":"Latte","en":"Milk","da":"Mælk"}, "section", "icon"}`.
- **Custom object** for things the catalog lacks, owned by one user and stored in Lucina's grocery Postgres
  (`grocery.shopping_custom_items`): `{"id": "u_<ULID>", "kind": "custom", "names": {it, en, da}, "aliases": [...],
  "section", "icon", "needsTranslation", "createdBy", "createdAt", "updatedAt"}`.

Resolution of typed text (on every add, from any client, Telegram, Discord or Lucina's chat): the text is folded as in
[`catalog.md`](catalog.md) (a leading article and a plural *s* are tolerated) and matched against catalog names and
aliases in every language, then the user's custom names and aliases. Text that matches nothing creates a custom object;
its other languages, section and icon come from ONE DeepSeek JSON call (cached). If DeepSeek is unavailable the typed
text fills every language and the object has `needsTranslation: true`. The typed text is kept on the row as
`typedName` and on a custom object as an alias.

Display: `name` on a row is the object's name in the requested language (`?lang=it|en|da`, default the owner's).
Clients that keep the whole state use `item.names[uiLang]`; `spec` stays as typed. The owner edits a custom object's
names, section and icon with `edit_item` (below); the edit shows on every list that has it.

`GET /v1/shopping/items` → the owner's custom objects, so search shows them next to the catalog.

## Lists
- `GET  /v1/shopping/lists` → `[ {id, name, theme, needed, recent, updatedAt} ]`
- `POST /v1/shopping/lists {name, theme}` → list. Themes: `home`, `grocery`, `work`, `holiday`, `garden`, `hardware`, `baby`, `other`.
- `PATCH /v1/shopping/lists/{id} {name?, theme?}` and `DELETE /v1/shopping/lists/{id}`. The default list can't be deleted.

## List state: `GET /v1/shopping/lists/{id}?lang=it|en|da`
```json
{ "list": {"id":"default","name":"Casa","theme":"home"}, "revision": 42, "lang": "it",
  "items": [ { "id":"uuid", "itemId":"banana", "catalogId":"banana",
               "item": {"id":"banana","kind":"catalog","names":{"it":"Banane","en":"Bananas","da":"Bananer"},
                        "section":"produce","icon":"🍌"},
               "name":"Banane", "typedName":"banana", "spec":"6, mature", "quantity": null, "unit": null,
               "section":"produce", "icon":"🍌", "status":"needed", "position": 3,
               "updatedAt":"…", "updatedBy":"telegram|discord|web|ios|alexa|lucina" } ] }
```
- `catalogId` is `itemId` for catalog objects and null for custom ones. `section`/`icon` are the row's override, else
  the object's.

## Ops: `POST /v1/shopping/lists/{id}/ops`
```json
{ "expectedRevision": 42, "lang": "it",
  "ops": [ { "opId":"uuid", "type":"add", "itemId":"banana", "spec":"6" },
           { "opId":"uuid", "type":"add", "text":"detersivo piatti" },
           { "opId":"uuid", "type":"purchase", "itemId":"uuid" } ] }
```
- **Op types:**
  - `add {itemId?|text, spec?, quantity?, unit?, section?}`: resolve the object (`itemId`, else the text; new text creates
    a custom object), then create the row, or return the list's active row of that object (listed in `merged`) and move
    it back to `needed`. Legacy field names `catalogId` and `name` still work. Unknown `itemId` and no text: rejected
    `unknown_item`.
  - `edit_item {itemId, names?: {it?,en?,da?}, section?, icon?}`: edit a custom object everywhere (`itemId` = the
    object id; `icon: ""` clears). Rejected: `not_custom`, `item_not_found`, `name_taken`, `invalid_section`,
    `invalid_icon`. Other lists that show the object get a revision bump.
  - `need {itemId}`, `purchase {itemId}`, `remove {itemId}`
  - `set_spec {itemId, spec}`, `set_quantity {itemId, quantity, unit?}`, `set_section {itemId, section}`
  - `rename {itemId, name}`: on a custom row, the object's name in `lang`; a name of another object moves the row to it;
    a catalog row renamed to new text moves to a new custom object.
  - `move {itemId, toListId}`, `clear_recent {}`, `reorder {itemId, position}`
- **Response:** `{ revision, applied: [opId], duplicates: [opId], rejected: [{opId, error}], merged: [opId], lang,
  items: [...full state...] }`.
- **Idempotent:** an `opId` that was already applied is reported in `duplicates` and changes nothing.
- **409** when `expectedRevision` is stale *and* an op conflicts. Clients refetch and retry with new opIds. Non-conflicting
  ops are applied even when the revision is stale; Lucina decides.

## Legacy (kept for chat and old clients)
`GET/POST /v1/shopping` and `PATCH /v1/shopping/{id}` keep working against the default list. Their `product`/`name`
is the object's name in `?lang=`, `rawName` the typed text.

## Migration
Lucina `migrations/003_shopping_items.sql` adds the custom object table and `item_id` (rows with a catalog id get it
there), then `scripts/grocery-migrate-items.mjs` (dry run by default; `--apply` backs up with `pg_dump` first) converts the
rest: catalog matches become catalog ids, other text becomes custom objects with translations, and active rows of one
list that turn out to be the same object are merged.
