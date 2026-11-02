# Lucina saves secrets herself, through the local model

Owner, 2026-11-02: "she can do everything herself because it's fine if a local model handles the secret. It's just
local. And then you can clear the chats." Secrets in his notes and chats go to Infisical without him pasting anything.
Code: `crates/lucina-server/src/secret_keep.rs`, `secret_keep/local.rs`, `secret_keep/sweep.rs`,
`crates/lucina-secrets/src/labelled.rs`, `crates/lucina-store/src/scrub.rs`, `crates/lucina-tools/src/tools/secret_keep.rs`;
tests `crates/lucina-server/tests/secret_keep.rs`. Broker side: `~/Projects/AI/infisical-mcp`, op `deposit`.

## The path of a value

```
cloud model (DeepSeek)   secret_keep {source: "note:telegram", name, what}      -> {job, state, name, project, ...}
                                     | references and names only
queue worker in lucina-rs   1. read the note or chat item
                            2. local Qwen picks the value: POST 127.0.0.1:11436 (loopback or refused)
                            3. the value must stand verbatim in the source
                            4. broker op deposit (create-only); read-back sha256 compared here
                            5. only then: clear the note, re-read it; clear the chats and the copies
                            6. receipt in his chat: names, what was cleared, what remains
```

The worker has no handle on a cloud model. `local.rs` is a plain HTTP call, not a `ChatModel`, and refuses any address that
isn't loopback. When Qwen is busy, stopped or failing, the job waits and retries (1 to 30 minutes apart, up to 6 hours), then
fails with `local_model_unavailable` and says so in chat. Nothing is saved or cleared, and there is no fallback.

## What the broker allows

`deposit` grants (broker.toml) let one caller create **new** secrets with no phone approval:

- it can't read, change or delete anything;
- a name that exists with another value is `exists`; the same value again is a no-op, so a crash between saving and clearing
  can finish on the next run;
- the answer is names and a read-back sha256; the audit log has the ref.

So a prompt injection or the computer-use agent in the same unit can at most add a new name inside the granted patterns.
It can't overwrite `GOOGLE_PASSWORD`, which no pattern covers, and it can't read anything back.

## Auto add

- **In a chat message.** `mask_message` runs before the model and every sink, on the owner's own turns: token shapes as
  before, plus values said in words (`labelled.rs`: "la password del wifi è X", IT/EN/DA). The value is held in memory. After
  the turn a job asks the local model whether it is a real credential and what to call it, and saves it as `KEPT_<NAME>` in
  the default place (`LUCINA_SECRET_KEEP_PLACE`, `personal/prod`). If the local model can't answer within 6 minutes, the old
  "Save as secret?" card comes while the value is still held. If it says no, the value is dropped. Telegram: the bot deletes
  his message. Discord doesn't let a bot delete it in a DM, so the card's note still asks him to.
- **In a note.** A sweep (hourly) reads the changed notes, runs the same deterministic patterns plus a title check, and posts
  one offer per note with a button. The button is a chat message ("Move the password in note X to Infisical"), which starts
  `secret_keep`. Moving text out of a note starts with his tap; no model reads the note until then.
- **When the cloud model reads a note.** The `notes` tool and the notes context mask values said in words on the way out.

## Limits of the pattern

A password with no label in the sentence ("cavolfiore is what I use") isn't seen, and a plain lowercase word after a label counts
only at the end of the sentence and if it isn't an ordinary word. The local model doesn't gate every message: a GPU slot that
may be busy for minutes can't stand in front of each chat turn. Deterministic first was the owner's brief.

## A note's name can hold the secret

A slug is made from the note's title, or from its first line when it has none, so a note that starts with a token has that
token in its name, lowercased. On 2026-11-02 the first scan showed that: a slug with what looks like a real GitHub token. So
references are always `note:<id>`, the scan and the sweep never print a slug, the `notes` tool gives the model the id instead of a
slug with a token-shaped segment, and a clear renames a slug that holds part of the value to `kept-<id>`. secretary-server still
makes such slugs for new notes, and secretary-mcp still shows them to MCP agents.

## What is cleared, and what can't be

Cleared after a confirmed save (the value's encodings, in string values only, replaced by `[secret:NAME]`):

- the note (only the value; `whole_note` leaves a pointer), re-read to confirm;
- the timeline in every generation, the self-test timeline, recent turns;
- JSON and text files in `LUCINA_SECRET_KEEP_SCRUB_DIRS`;
- that note's file in each dated backup (`LUCINA_SECRET_KEEP_BACKUP_DIR`), each path rewritten by itself, because days share inodes.

A value under 8 characters could be part of anything, so only its own source is cleared, and the receipt says so.

Can't be undone, and not touched by this:

- **Text already sent to a cloud model.** Until this shipped a password in words reached DeepSeek in the turn, and a note reached it
  whenever Lucina read it. Providers keep what they got. Rotate anything that matters.
- **Honcho.** Masked text is all it gets now. What it got earlier stays in its sessions and derived conclusions, and its chain
  runs on cloud models. It has no way to delete a message by content; searching it for a value would send the value there.
- **The journal**, for anything logged before. Lucina doesn't log values.
- **Telegram and Discord** copies of older messages. **Google Keep** itself and any Takeout archive, for imported notes.
- **Apps offline at that moment.** A live client redraws the changed items; a device's cached copy of an older message stays
  until that chat is loaded again.
- **Local Qwen's RAM.** The prompt passes through the gateway and llama.cpp's memory (`cache_prompt: false`), not their logs. Unload
  frees it. That is what "it's just local" accepts.

## Using it

From a chat: "Move the password in note X to Infisical". From a shell, on nezopt:

```bash
lucina-rs secret-keep save --source note:telegram --name KEEP_TELEGRAM_BOT_TOKEN --project personal --environment prod \
  --what "the bot token" --wait 900
lucina-rs secret-keep status sk-…
lucina-rs secret-keep scan
```

`--request sr-…` closes the card that asked for it. `--whole` stores the whole note as one secret, with no model.

## Setup

1. Broker: `infisical-broker token new --out ~/.config/infisical-mcp/svc-lucina-keeper.token`, then in broker.toml a caller with
   `bind_unit = "lucina-rs.service"` and `deposit` patterns (`deploy/broker.example.toml`). A root step.
2. Lucina: `LUCINA_BROKER_KEEPER_TOKEN_FILE=…` in `mode.env`. Unset, the tools are off and the old cards stay.
3. `LUCINA_SECRET_KEEP_BACKUP_DIR=/home/nez/mnt/mybook/backups/secretary-notes`.
