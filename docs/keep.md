# Lucina Keep

A Google-Keep-style notes plugin for Lucina: colored notes, checklists, labels,
archive, trash, pinned notes, reminders, images/audio/PDF attachments, and an
import of the owner's Google Takeout. This file is the design decision and the
contract as built in `secretary-server` (`crates/secretary-server/src/notes/`).
The HTTP reference is in [server-api.md](server-api.md#notes).

## Decision: one store

Keep **extends the existing note store**. It does not add a parallel one.

- Lucina's `notes` tool (lucina-tools, over `/v1/notes`), the MCP
  `note_save` / `note_get` / `note_append` / `notes_search` and the web
  `/notes` view already use it. Every Keep note is therefore visible to Lucina
  and the MCP without any sync between two stores, and every note Lucina or an
  AI client saves shows up in Keep.
- Every new field is optional with a serde default, so old note files and old
  clients keep working. Nothing is migrated on disk except a one-time revision
  stamp (see Revisions).
- Keep **labels are the existing `tags`** (same field). A small registry keeps
  their display case.

Owner-only: the owner's devices and Lucina's service token. Files are 0600,
directories 0700, under `$SECRETARY_DATA_DIR/notes` (p1: `/var/lib/secretary/notes`).
No third-party calls anywhere (no link previews, no Google APIs).

## Files

```text
notes/<id>.json              live notes, archived ones too
notes/trash/<id>.json        notes in the trash (30 days)
notes/state.json             {rev, floor, tombstones[{id, rev, at}]}
notes/labels.json            [{tag, name, createdAt}]
notes/import-deleted.json    ["gkeep:<usec>", ...] keys of notes deleted forever
notes/attachments/<sha256>.<ext>
notes/.lock                  flock, one writer at a time (server and CLI)
```

The store keeps every note (live and trash) in memory and reloads when the
mtime of `notes/` or `notes/trash/` changes, so the CLI importer and the
server can share a directory. A write of our own never causes a reload.
Memory is the notes' JSON only (attachments are served from disk): about 5,000
small notes are a few MB.

## Note model

JSON is camelCase. Additions to `Note`:

| field | type | default | meaning |
|---|---|---|---|
| `title` | string | `""` | may be **empty** when the note has content, items or attachments |
| `content` | string | `""` | markdown (`**bold**`, `*italic*`, `<u>`, `# H1`, `## H2`, lists, links). For a list note: derived, see Checklists |
| `color` | string | `"default"` | `default red orange yellow green teal blue cerulean purple pink brown gray` |
| `archived` | bool | false | |
| `items` | `[ChecklistItem]` | absent | present (possibly empty) = list note |
| `attachments` | `[Attachment]` | `[]` | |
| `annotations` | `[{url, title?, description?, source?}]` | `[]` | web links (http/https only, at most 50) |
| `collaborators` | `[string]` | `[]` | emails, metadata only, never contacted |
| `reminder` | `{at, recurrence?, lucinaId?}` | absent | `at` RFC 3339; `recurrence` `daily weekly monthly yearly` (metadata, Lucina's reminder fires once); `lucinaId` server-set |
| `position` | number | createdAt in epoch ms | manual order: higher = earlier in the grid. A stored 0 means "not set" |
| `rev` | integer | stamped on load | store revision of the last write (server-set) |
| `import` | `{source, key, editedUsec, importedAt}` | absent | importer bookkeeping |
| `tags` | `[string]` | `[]` | the labels, normalized |

`ChecklistItem`: `{id: "i_" + 12 hex, text, checked, indent: 0|1}`; array order is
display order; at most 1000 items of 1000 characters, no control characters.
`Attachment`: `{id: sha256 hex, file: "<sha256>.<ext>", mimetype, size, name?, createdAt}`.

Request bodies (`POST`, `PATCH`, batch) accept every field except the
server-set ones. `rev` and `import` (and `reminder.lucinaId`) are **accepted and
ignored**, so a client can send a note back as it got it. `createdAt` on POST is
ignored. `attachments` in a body refers to bytes that are already stored (see
Attachments); the upload route is the way to add new ones.

### Checklists

- On every write of a list note, `content` is **derived** from `items`: one line
  per item, `- [ ] text` / `- [x] text`, indent 1 = two leading spaces. So
  `note_get` and old clients see a markdown checklist.
- `PATCH {content}` on a list note (no `items` in the body) parses the lines
  back: `- [ ]`, `- [x]`, `* [ ]`, `* [x]` keep their state, a plain `- ` / `* `
  bullet loses its marker, any other non-empty line becomes an unchecked item.
  Two or more leading spaces (or a tab) = indent 1. An item whose text matches
  an unused old item keeps that item's id, so ids are stable when a client saves
  the derived text again.
- `POST /append` on a list note parses the appended lines the same way and adds
  the items at the end (`separator` is ignored).
- `items` and `content` in one body: `items` wins. `items` on a text note turns
  it into a list note.
- `PATCH {items: null}` is refused (`400`); convert with `{"convert": "text"}`
  (items become plain lines) or `{"convert": "list"}` (the content lines become
  items, with the same parser, so `- [x]` lines keep their state). `convert`
  cannot be combined with `items` or `content`.
- Search matches item text (it is in the derived content).

### Titles and slugs

- An empty title is allowed when `content` (non-blank), `items` or `attachments`
  is non-empty; otherwise `400 title: must not be empty`.
- Slug when none is given: from the title; else from the first six words of the
  first non-empty line of the content (or the first item); else `note`; then
  `-2`, `-3`, … as before. The slug is not recomputed when the title changes.
- Reserved words (routes): `trash new search labels changes attachments import batch`.
  `slugify("labels")` is `labels-note`.
- `MAX_NOTES` is 20000 live notes (archived ones count, the trash does not).
  Title limit stays 200 characters; content 256 KiB.

## Labels (= tags)

- Normalization: trim, collapse inner whitespace, strip a leading `#`,
  lowercase. Allowed: any characters except control characters and `,`; at most
  **50** characters; at most **50** labels per note. Duplicates (by normalized
  form) collapse. The stored tag is the normalized form. Every tag that was valid
  before is still valid.
- In bodies, `tags` and `labels` (alias, display names) are merged, tags first;
  either one replaces the note's labels.
- Registry `notes/labels.json` keeps the display case. A note saved with an
  unknown label adds `{tag, name: as typed}` (trimmed, `#` stripped, whitespace
  collapsed); the first spelling wins.
- `GET /v1/notes/labels` → `{labels: [{tag, name, count}]}`: registry ∪ tags in
  use, sorted by name case-insensitively, `count` = live non-trashed notes.
- `POST /v1/notes/labels {name}` → `201 {tag, name, count}`; `409 label_exists`.
- `PATCH /v1/notes/labels/{tag} {name}` → `{tag, name, count}`. Same normalized
  form: display name only (no note is touched). Different: the tag is rewritten
  in every note (live and trash; each gets a new `rev`, `updatedAt` stays), and
  merges into the label that already has that name (that label's display name
  is kept). `404 label_not_found`. `{tag}` is percent-encoded in the path.
- `DELETE /v1/notes/labels/{tag}` → `{ok: true, removed: <notes that lost it>}`.

## Revisions and sync

Every write to a note (create, patch, append, trash, restore, batch, attachment
add/remove, label rename/delete touching it, import) takes the next **store
revision** and stamps `note.rev`. Deleting forever leaves a **tombstone** with
its own revision.

- The counter is recovered at load as the highest revision found in any note,
  any tombstone or `state.json`, so it never runs backwards and does not need
  a write per note. `state.json` is written when the tombstones change.
- Notes written before revisions existed get one at the first load, oldest
  `updatedAt` first (trash included), and the files are rewritten with it.
- `GET /v1/notes/changes?since=<cursor>&limit=<1..500, default 200>` →
  `{notes: [Note], removed: [id], cursor: "<rev>", hasMore}`.
  - `notes`: full notes (live, archived and trashed; `deletedAt` set for trash)
    with `rev` > since. `removed`: ids deleted forever with tombstone rev > since.
    Both come from one stream ordered by rev and cut at `limit`.
  - `cursor`: the highest rev returned, or the store rev when nothing is
    returned. Pass it as the next `since`; loop while `hasMore`.
  - No `since`, empty or `0`: everything.
  - `410 cursor_expired`: `since` is below the highest pruned tombstone
    (tombstones live 60 days) or **above the store's rev** (a cursor from
    another store, e.g. after a restore from backup). The client drops its cache
    and loads from 0.
  - A non-number `since` is `400`.
- Trash is purged (and tombstoned) when a call finds entries older than 30 days,
  at most every 5 minutes and at load, so `changes` reports purges too.
- A write from another process (the CLI) is picked up at the next request; the
  two processes are serialized with `notes/.lock`.

## Trash

`DELETE /v1/notes/{key}` moves to the trash (30 days, unchanged). `GET
/v1/notes/trash`, `POST /v1/notes/trash/{id}/restore` unchanged (answers carry
the new fields). New: `DELETE /v1/notes/trash/{id}` deletes one trashed note
forever (`404` for a note that is not in the trash) and `DELETE /v1/notes/trash`
empties the trash; both answer `{ok: true, removed: n}`. Deleting forever removes
the files, adds a tombstone, remembers an import key (see Import) and collects
attachment files nobody else uses.

## Batch

`POST /v1/notes/batch {ids: [≤500], set?: {color?, pinned?, archived?},
addLabels?: [name], removeLabels?: [name], trash?, restore?, deleteForever?}` →
`{notes: [Note]}` (the notes that changed; plus `removed: [id]` after
`deleteForever`). `ids` are note ids or slugs; unknown ids and duplicates are
skipped; notes that would not change are not written and not returned.
`trash`, `restore` and `deleteForever` exclude each other and `set` / labels
(`400`); `restore` and `deleteForever` apply to trashed notes only, `trash` to
live ones. Batch cannot set reminders (trashing cancels them, below).

## Attachments

- `POST /v1/notes/{key}/attachments` (raw body, `Content-Type` = mimetype,
  optional `?name=`) → `201` the note. Body limit 25 MB (+ 64 KB), unlike the
  768 KiB of the JSON routes. Allowed: `image/{png,jpeg,gif,webp,heic,heif}`,
  `audio/{3gpp,mp4,aac,mpeg,ogg,webm,x-m4a}`, `application/pdf`
  (`415 unsupported_media_type`); empty `400`; over 25 MB `413
  attachment_too_large`; 50 per note `409 attachments_full`. Mimetype parameters
  are ignored. `name` keeps the last path component, no control characters.
- Content-addressed: `notes/attachments/<sha256>.<ext>` (0600), deduplicated
  across notes; the same bytes twice on one note are one attachment.
- `DELETE /v1/notes/{key}/attachments/{attId}` → the note. The file is removed
  when no note (live or trash) refers to it any more, which also happens when
  the last note using it is deleted forever, or when a PATCH `attachments` list
  drops it.
- `GET /v1/notes/attachments/{file}` → the bytes. `file` must match
  `^[0-9a-f]{64}\.[a-z0-9]{1,5}$` (else `404 attachment_not_found`).
  `Cache-Control: private, max-age=31536000, immutable`, `ETag: "<sha256>"` (and
  `304` on `If-None-Match`), `Accept-Ranges: bytes` with single `Range` (`206`;
  `416` when unsatisfiable), `X-Content-Type-Options: nosniff`. The
  `Content-Type` comes from the extension (`m4a` is `audio/mp4`).
- Bodies of POST/PATCH may carry `attachments: [{id, file | mimetype, name?}]`
  for files already in the store (make a copy of a note without re-uploading);
  unknown ids are `400`. In PATCH the list **replaces** the note's attachments.

## Reminders

Setting `reminder` in `POST` or `PATCH` makes the server create a Lucina
reminder through the existing upstream (`Call::create_reminder`, the call behind
`POST /v1/reminders`) with the text `"Keep: <title or first line>"`, and stores
its id in `reminder.lucinaId`.

- The request is validated first (a dry run of the whole write), so Lucina is
  only asked for a change that will be saved.
- Changing or clearing (`reminder: null`) cancels the old Lucina reminder. As
  built, the new reminder is **created first**, then the old one cancelled; if
  that cancel fails, the new one is cancelled again. This is the reverse of the
  order in the first draft of the contract, chosen so that a failure leaves the
  note and Lucina as they were. Clearing cancels before saving.
- The same moment (`at` parsed equal) on a note that already has a Lucina
  reminder changes nothing upstream (`recurrence` is just stored). If the note
  has a reminder but no live Lucina id (after trash and restore), setting it
  again re-arms it.
- Lucina failing: `502 lucina_error` / `lucina_unreachable` (`504
  lucina_timeout`), her own refusals pass through (`400 due_in_past`), `503
  lucina_not_configured` without a Lucina on the server; in every case the note
  is unchanged.
- A Lucina reminder that is gone (`404 reminder_not_found`) or already sent
  (`409 already_sent`) counts as cancelled.
- Trash (single or batch) cancels it too, **best effort**: a Lucina that cannot
  be reached must not keep the owner from deleting a note. The trashed note keeps
  `reminder.at` and drops `lucinaId`; restoring does not re-arm it.

## Import (store API)

Piece (c) reads a Takeout and calls these `NoteStore` methods (exported from
`crate::notes`): `has_attachment`, `put_attachment`, `ensure_labels`,
`import_note`, with `ImportedNote`, `ImportOutcome`, `Annotation`, `Attachment`,
`Caller`. `ImportOutcome` also converts to `&'static str` (`new`, `updated`,
`unchanged`, `keptLocalEdits`, `skippedDeleted`) for reports.

- `put_attachment(bytes, mimetype, name)` writes the file (0600) unless it is
  there and returns the `Attachment`. Same limits as uploads.
- `ensure_labels(names, dry_run)` registers display names and returns the ones
  that were new (not in the registry or in use). Names the label rules refuse
  (`,`, control characters, over 50 characters) are cleaned up, not refused.
- `import_note(n, dry_run, caller)` is idempotent on `n.key`
  (`gkeep:<createdTimestampUsec>[-n]`):

  | situation | outcome |
  |---|---|
  | key in `import-deleted.json` (the note was deleted forever here) | `SkippedDeleted` |
  | the note with that key is in the trash here | `KeptLocalEdits` |
  | `editedUsec` not newer than the stored one | `Unchanged` |
  | newer, and the note was not edited here since (its `updatedAt` equals `import.importedAt`) | `Updated` (in place: same id and slug) |
  | newer, but edited here | `KeptLocalEdits` |
  | unknown key | `New` |

  `dry_run` decides the outcome and writes nothing (no labels either). A note
  edited here and re-imported from the same export is `Unchanged`, not
  `KeptLocalEdits`: there is nothing newer to keep it over.
- `import.importedAt` holds the `updatedAt` the import wrote (the export's edit
  time at millisecond precision), which is how "not edited here since" is
  detected: every edit here sets `updatedAt` to now. It is not the wall-clock
  time of the import.
- `createdAt` / `updatedAt` come from the export; `position` = createdAt in ms;
  `source` = `google-keep`; a trashed note lands in the trash with `deletedAt` =
  import time. Notes deleted forever (also by the 30-day purge) keep their key in
  `import-deleted.json`.
- Leniency: unknown colors become `default`; control characters are stripped;
  titles over 200 characters are clipped (the importer keeps the full title in
  the body); bad annotations, collaborators and attachment files are dropped;
  blank notes are still imported. Content over 256 KiB is `note_too_large` (the
  importer reports it). A full store is `409 notes_full`.

The Takeout parser, `POST /v1/notes/import/google-keep` and `notes import-keep`
are not part of this piece; the route word `import` is reserved.

## List and summaries

`GET /v1/notes?q=&tag=&limit=&archived=include|exclude|only&color=&type=list|text|image|audio|link|reminder`:
`archived` defaults to `include` (back compatible), `trashed` notes are never
listed (use `/trash`). Order unchanged: pinned first, then most recently updated
(`position` is for the clients' grid). A `tag` or `color` filter that cannot
match (a `,` in the tag) matches nothing; an unknown `archived`, `color` or
`type` is `400`. `NoteSummary` adds `color, archived, isList, itemsTotal,
itemsDone, attachmentCount, hasImage, reminderAt` (absent without a reminder),
`position, rev, labels` (display names in tag order).

## Performance

The store may hold about 5,000 notes on p1 (1 GB RAM). List, `changes` and
`labels` work on the in-memory index; a request costs a `stat` of two
directories and a flock, no file reads, unless something outside changed the
directories. `changes` filters in memory and serializes at most `limit` notes.
A full reload (start, or after a CLI import) reads every file once.

## Tests

`crates/secretary-server/tests/notes.rs` (the router and a mock Lucina) and the
unit tests in `src/notes/` (`keep_tests.rs`, `tests.rs`). The differential tests
in `tests/verus_notes.rs` are unchanged.
