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
- **One active row per (list, catalogId-or-normalised-name):** adding something that is already `recent` moves it back to `needed`.
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

## Lists
- `GET  /v1/shopping/lists` → `[ {id, name, theme, needed, recent, updatedAt} ]`
- `POST /v1/shopping/lists {name, theme}` → list. Themes: `home`, `grocery`, `work`, `holiday`, `garden`, `hardware`, `baby`, `other`.
- `PATCH /v1/shopping/lists/{id} {name?, theme?}` and `DELETE /v1/shopping/lists/{id}`. The default list can't be deleted.

## List state: `GET /v1/shopping/lists/{id}`
```json
{ "list": {"id":"default","name":"Casa","theme":"home"}, "revision": 42,
  "items": [ { "id":"uuid", "catalogId":"banana", "name":"Banane", "spec":"6, mature", "quantity": null, "unit": null,
               "section":"produce", "icon":"🍌", "status":"needed", "position": 3,
               "updatedAt":"…", "updatedBy":"telegram|discord|web|ios|alexa|lucina" } ] }
```

## Ops: `POST /v1/shopping/lists/{id}/ops`
```json
{ "expectedRevision": 42,
  "ops": [ { "opId":"uuid", "type":"add", "catalogId":"banana", "name":"Banane", "spec":"6" },
           { "opId":"uuid", "type":"purchase", "itemId":"uuid" } ] }
```
- **Op types:**
  - `add {catalogId?|name, spec?, quantity?, unit?, section?}`: create, or move `recent` back to `needed`.
  - `need {itemId}`, `purchase {itemId}`, `remove {itemId}`
  - `set_spec {itemId, spec}`, `set_quantity {itemId, quantity, unit?}`, `set_section {itemId, section}`, `rename {itemId, name}`
  - `move {itemId, toListId}`, `clear_recent {}`, `reorder {itemId, position}`
- **Response:** `{ revision, applied: [opId], duplicates: [opId], items: [...full state...] }`.
- **Idempotent:** an `opId` that was already applied is reported in `duplicates` and changes nothing.
- **409** when `expectedRevision` is stale *and* an op conflicts. Clients refetch and retry with new opIds. Non-conflicting
  ops are applied even when the revision is stale; Lucina decides.

## Legacy (kept for chat and old clients)
`GET/POST /v1/shopping` and `PATCH /v1/shopping/{id}` keep working against the default list.
