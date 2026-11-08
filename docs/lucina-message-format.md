# Lucina message format v1

Lucina talks like a person: a few short bubbles, typed widgets for anything
structured, and buttons instead of "you can also type …" footers. This is the
shared contract between Lucina (Rust `lucina-store::rich` + `lucina-server::rich`, which serve the
app and web chat; Node `src/lucina-message-format.js`, which renders Telegram and Discord), the web chat (`web/src/lib/rich.ts`) and the iOS chat
(`LucinaRich.swift`).

It is on when Lucina runs with `LUCINA_RICH_REPLIES=true`. Clients that do not
know the format keep working: every rich reply also has a plain-text `text`.

## Where it appears

| Place | Shape |
|---|---|
| `POST /v1/chat` response (proxied from Lucina `POST /api/secretary/chat`) | `reply` (plain fallback) plus `format: "v1"`, `lang`, `bubbles`, `blocks`, `actions` |
| Timeline item (`GET /v1/timeline`, the SSE stream) | `text` (plain fallback) plus `meta.format: "v1"`, `meta.lang`, `meta.bubbles`, `meta.blocks`, `meta.actions` |
| Telegram, Discord | No v1 on the wire: 1-3 short messages; actions become typed hints (`↪︎ rispondi: «sì» · «no»`) |
| Alexa, Honcho memory, MCP callers | The plain fallback |

Lucina validates the message before storing it and the timeline validates it
again on write. Clients must validate it once more (it is network data) and
drop anything that does not fit, never the whole message.

## Message

```json
{
  "format": "v1",
  "lang": "it",
  "bubbles": ["Ecco cosa supporto **ora** ✅", "Il resto è da costruire ⚠️"],
  "blocks": [
    { "type": "checklist", "title": "Live adesso", "items": [{ "text": "Notifiche app (push PWA, iOS, inbox)", "done": true }, { "text": "Morning brief" }] }
  ],
  "actions": [
    { "id": "details", "label": "Dettagli", "emoji": "🔎", "kind": "reply", "payload": { "text": "Dammi i dettagli" } }
  ]
}
```

| Field | Type | Rules |
|---|---|---|
| `format` | `"v1"` | Required in meta and responses. |
| `lang` | `it` \| `en` \| `da` | Optional. Language of the bubbles and the labels. |
| `bubbles` | string[] | 1-4 non-empty chunks, each about 280 characters at most (Lucina splits longer ones at sentence boundaries). Inline markdown only: `**bold**`, `*italic*`/`_italic_`, `~~strike~~`, `` `code` ``, `[label](https://…)`. A few emojis as signposts (✅ ⚠️ 📅 🛒 💪 📝). |
| `blocks` | Block[] | Optional, at most 8. Unknown or invalid blocks are dropped. |
| `actions` | Action[] | Optional, at most 4. Invalid actions are dropped. |

Markdown is rendered safely: no raw HTML ever (it shows as text), links only
for `http:`, `https:` and `mailto:`. Clients that cannot render markdown
strip the marks.

## Blocks

Text fields marked *md* allow inline markdown; the others are plain text.

| `type` | Fields | Limits |
|---|---|---|
| `checklist` | `title?`, `items: [{text` *md*`, done?}]` | 1-12 items. `done: true` shows a check; otherwise an empty box. Good for "present vs missing". |
| `card` | `emoji?`, `title`, `subtitle?`, `body?` *md*, `tone: info\|success\|warning` | One topic. `tone` defaults to `info`; `warning` for limits and problems. |
| `stat` | `items: [{label, value, delta?}]` | 1-4 numbers in a row. A single `{label, value, delta?}` is accepted and becomes one item. |
| `list` | `title?`, `items: [{emoji?, text` *md*`, meta?}]` | Up to 20 items; clients show 5 and collapse the rest behind "show more". |
| `table` | `title?`, `columns: string[]`, `rows: string[][]` | At most 4 columns and 6 rows (extra cells are cut). |
| `steps` | `title?`, `items: string[]` *md* | 1-8 numbered steps. |
| `quote` | `text` *md*, `cite?` | |
| `link_preview` | `url`, `title` | `url` must be `http(s)`. Opens in a new tab / Safari. |
| `place` | `title`, `address?`, `lat?`+`lng?`, `when?`, `end?` (RFC 3339), `eventId?`, `kind: event\|place`, `travelMin?`+`travelMode?` (`walk\|bike\|transit\|drive`), `travelApprox?` | A title plus an address or a valid coordinate pair (both or neither, not (0,0)); at most 3 per message. The card opens the in-app place map (`/map/place`). `travelMin` (1-1440) is Lucina's travel time from home, `travelApprox: true` when it is a straight-line estimate. |
| `image` | `src`, `alt?`, `caption?`, `width?`, `height?` | At most 2 per message. `src` is a path of the apps' API starting with `/v1/` (no `..`, no `//`, at most 300 characters, e.g. `/v1/au/map.png?building=1532&z=17`) or an `https://` URL on `getsecretary.tech`; anything else drops the block. `alt` and `caption` up to 200 characters; `width`/`height` in pixels (1-4096). Used for the AU campus map. | Tapping opens it full size.

Lengths: titles 120, item texts 400, card bodies 600, labels and stat values 40,
table cells 80 characters.

## Actions

```json
{ "id": "confirm-yes", "label": "Sì", "emoji": "✅", "kind": "confirm", "payload": { "answer": "yes", "text": "sì" } }
```

| `kind` | `payload` | What the client does |
|---|---|---|
| `reply` | `{text, prefill?}` | Sends `text` as the owner's next message. With `prefill: true` it only puts `text` (plus a space) in the composer and focuses it; the owner finishes and sends. Lucina uses `prefill` for anything that would start real work or post publicly. |
| `route` | `{path}` | Navigates in the app: `/notes/<slug>`, `/notes`, `/shopping`, `/links`, `/gym`, `/weight`, `/tasks` (when the app has it). Must start with `/`; no `..`, no scheme, at most 121 characters. One path may carry a query: `/map/place?<query>` (the in-app place map; `title`, `address`, `lat`, `lng`, `when`, `end`, `eventId`, percent-encoded with `%20` for spaces, at most 600 characters), which iOS also opens as `secretary://open/map/place?…`. Unknown paths fall back to the chat. |
| `confirm` | `{answer: yes\|no\|edit, text}` | Answers Lucina's pending confirmation (today: calendar events). `yes`/`no` send `text` (`sì`/`no`, which her yes/no flow understands). `edit` prefills the composer with `text` instead; the pending action is dropped when the next message is not a yes or no. |
| `dismiss` | `{}` | Nothing is sent; the buttons are just marked as answered. |
| `secret` | `{mode: save\|project, name?, project?, environment?, path?, target?, held?, projectName?}` | Opens the "Add secret" sheet with those names filled in (`project` is an Infisical id, `target` the `/secret` alias form, `held` the id of a value Lucina holds in memory for 10 minutes). Never carries a value: clients and servers drop any other field. The owner still confirms in the sheet. With `request` (the id of a request in Lucina's queue, `crates/lucina-server/src/secret_requests.rs`) current clients draw an inline paste-and-OK card instead of the bubble and buttons, from `GET /v1/secrets/requests`; older clients ignore it and open the sheet. |

`id`: 1-48 characters `[A-Za-z0-9_.:-]`, unique within the message. `label`: up to
40 characters. `emoji`: optional, one emoji.

After one button of a message is used, all buttons of that message are disabled
and the chosen one is marked. Clients remember the choice locally and also treat
a later owner message equal to a button's `text` as that choice (so it survives
reloads and other devices).

## Agenda answers

When Lucina answers a calendar question from the calendar (today, tomorrow, the next days, "the
next lecture", where or when one event is), the reply is not a wall of text:

- one short bubble: "Domani hai 3 impegni 📅", or the answer itself for one event ("Hack Night:
  Rust meets AI è domani alle 17:00, presso Folkehuset Møllestien.");
- one block per event in calendar order: a `place` (kind `event`) for the first three with a
  location, with the travel time from home when known; a `card` (`📅`, time and location in the
  subtitle) for the others; past 8 blocks the rest is one `list`;
- one `route` button per place, labelled "Naviga" / "Navigate" / "Navigér", to `/map/place?…`:
  the in-app map, never Apple or Google Maps;
- titles without calendar noise ("(Aarhus Universitet - Personal timetable: 810677)", account
  emails);
- the plain fallback (Telegram, MCP, the timeline text) is the bubble and one line per event:
  `• 17:00–20:00 Hack Night: Rust meets AI — Folkehuset Møllestien · 🚲 ≈ 16 min`.

Lucina (`lucina-server/src/agenda.rs`, branch `lucina-rs`) builds it without a model call.

## Rendering rules

- Bubbles appear one after another with a short typing indicator between them
  (about 350-900 ms, longer for longer bubbles) only for replies that arrive
  while the chat is open. History and reduced motion show everything at once.
- Widgets and buttons appear after the last bubble.
- Her voice (TTS) reads only the bubbles, with the markdown marks removed. The
  avatar's speech bubble outside the chat shows the same bubble text.
- Old plain-text messages are rendered with the same safe markdown renderer
  (Notes), so `**bold**` no longer shows its asterisks.

## Plain-text fallback

Bubbles, then each widget as compact text, then the action hints:

```
Ecco cosa supporto **ora** ✅

**Live adesso**
✅ Notifiche app (push PWA, iOS, inbox)
⬜ Morning brief

↪︎ rispondi: «Dammi i dettagli»
```

Checklist `✅`/`⬜`, card `emoji **title** — subtitle` + body, stat
`label: **value** (delta)`, list `• text — meta` (or the item's emoji), table
`a · b · c` rows, steps `1. …`, quote `> …`, link preview `🔗 [title](url)`,
image `🗺️ caption` (followed by ` — <LUCINA_PUBLIC_URL><src>` when the public URL is configured).
A prefill or `edit` action is shown as `«text …»`, a route as `nell'app: label`.

## How Lucina produces it

- The planner's reply is the draft. A short conversational draft is split into
  bubbles without a model call. A long or structured one is restyled by
  DeepSeek in JSON output mode (`response_format: {type: "json_object"}`)
  under the schema above, with one retry on invalid JSON and the
  deterministic splitter as the fallback (headings and lists become widgets,
  at most 3 bubbles).
- Her old `💡 Posso anche:` footer becomes `reply` buttons.
- A pending calendar confirmation gets `Sì` / `No` / `Modifica` confirm buttons.
- The style rules (short first, details on request, no headings, 3-5 bullets
  at most, emojis as signposts, the owner's language) are in every prompt.
