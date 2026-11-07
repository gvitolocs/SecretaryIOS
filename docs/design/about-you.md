# About you: Lucina's memory of the owner

Owner, 2026-11-05: "she should have a memory about me that she can consult when she needs to, and she updates it, also
temporary things… when I say something or she knows something from emails or other things, she updates the memory about
me herself." Code: `crates/lucina-store/src/about.rs` (store), `crates/lucina-server/src/about.rs` and `about/extract.rs`,
`crates/lucina-server/src/chat/about_turn.rs`, `crates/lucina-tools/src/tools/about_me.rs`, `web/src/views/About.tsx`.
Tests: `crates/lucina-server/tests/about_you.rs`, `web/src/views/About.test.tsx`.

## The store

`<data>/about-you.json` (0600) is the source of truth about him; Honcho stays for conversation recall. A fact has its text,
a category, its source (chat, email, document, calendar, owner-edit, seed, with a reference), learned_at, last_confirmed,
valid_until (a date, a condition, or both) and a status. A fact that stops holding goes to history (`expired`, or
`retracted` when it was never true) and is never deleted; every change is logged on the fact. A text that looks like a
secret is refused (the redactor's detector, values said in words, known values): those go to Infisical.

## Always in context

Every owner turn gets the "About Giuseppe" block, whatever the intent policy: active facts by category, at most 3000
characters. `clip_memory_for_reply` keeps it whole, so the casual reply's 1600-character budget no longer cuts it.

## She keeps it true

- `about_me` (lookup, add, update, confirm, end, retract) on her tool turns, and a short tool turn when he talks about the
  memory itself: "that's wrong, …", "remember that …", "I no longer …", "what do you know about me".
- The extractor reads, on the local Qwen only (loopback, no fallback), his app and chat-app messages, the documents he
  shares and new calendar events. It reads them next to the facts she has and applies the changes it is at least 0.8 sure
  of, each with its source. A busy or stopped model leaves the queue (72 hours at most).
- Facts with a last day expire by themselves. Facts with a condition are in the extractor's view and end when an item
  shows the condition met.
- What never writes it: MCP and agent turns; Juno's chat (agents test there by typing as him); self-tests, which read his
  facts but write `selftest-about-you.json`.

## He sees and fixes it

Secretary web, Plugins → About me (`/about`): facts by category with source and date, edit, "No longer true", "Wrong",
add, history and each fact's change log. Routes: `GET/POST /v1/about`, `PATCH /v1/about/{id}`, `POST /v1/about/{id}/end`
and `/retract` (secretary-server forwards to lucina-rs `/api/secretary/about`).

## Not yet

- **Email:** Lucina reads no mailbox. The extractor takes `email` items, but nothing feeds them until a mail source exists
  (Gmail needs his OAuth approval).
- **iOS:** no native page yet; the web page works in the app's browser views.
