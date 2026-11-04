# Lucina saves secrets herself, through the local model

Owner, 2026-11-02: "she can do everything herself because it's fine if a local model handles the secret. It's just
local. And then you can clear the chats." Secrets in his notes and chats go to Infisical without him pasting anything.
Code: `crates/lucina-server/src/secret_keep.rs`, `secret_keep/local.rs`, `secret_keep/sweep.rs`, `secret_keep/files.rs`,
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

## He wants to give her one: the card opens

"Let me give you the maps key": `secret_talk` (owner_secrets.rs) sees the offer, typos and Danish included, before any model,
and the short secret turn calls `secret_tile` (lucina-tools; a tool name with "card" in it is refused as a payment tool). It
opens the Add secret card at the bottom of his chat now (`SecretRequests::show_now`):

- an open card for the same secret (an agent's request) comes down instead of a new one, in the place it named;
- otherwise she names it and the place is where that name lived before, else his own place (`LUCINA_SECRET_KEEP_PLACE`,
  `personal/prod`); her system text lists the open cards and the places in use, names only;
- his card goes ahead of an agent's, which waits and comes back after his is saved or dismissed;
- his own card sends no separate push: it is in the chat, which his phone shows too.

The reveal guard (`lucina_secrets::reveal`) no longer reads "let me give you the API key" as a request to show one. A self-test
turn gets its own queue and the self-test conversation, so a test never shows in his chat. Her persona says it too: secrets
only ever go to Infisical, never to an env file, a config file, a note or the chat.

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

## Files and env files

Owner, 2026-11-03: she can handle secrets "also from files or from venv, so she can also set up project easily and safely".
Code: `secret_keep/files.rs`, `crates/lucina-secrets/src/source_path.rs`, `envfile.rs`; Verus `verification/verus/secret_source_path.rs`.

```bash
lucina-rs secret-keep save --source file:~/.config/app/login.key --name KEEP_APP_LOGIN --wait 60
lucina-rs secret-keep save --source env:~/Projects/Work/app/.env --project personal --environment prod --path /secret-keep-canary
lucina-rs secret-keep project ~/Projects/Work/app [--deposit]
```

- `file:<path>`: one line or one PEM block is the value, with no model. A file with more in it needs `--what`, and then the local model picks,
  under the same rule as a note: what it picks must stand verbatim in the file. No words: `not_one_value`, nothing guessed.
- `env:<path>`: plain parsing, no model. Every key gets a state in the job (`saved`, `left`, `empty`, `placeholder`, `reference`,
  `unclear`, `refused_by_rule`, `exists`, `not_allowed`, `not_in_file`). Without `--keys`, only keys that look like secrets move, by name or
  value shape, so `PORT=8080` is `left`. A line that doesn't parse is reported as `line N` and nothing more: it may be the secret itself.
  An opening quote that never closes stops the parse there.
- `secret-keep project <dir>`: names from `.env.example` and its kin, and `${NAME}` in compose files (a default after `:-` is dropped);
  states from `.env`, `.env.local`, `.env.production`, the first real value winning. Answers `needs`, `missing`, `not_in_a_list`, `unclear_lines`,
  `files_refused`. `--deposit` queues what's found. The files stay.
- In a chat: the tools `secret_keep` (`file:`, `env:`, `keys`, `prefix`, `clear`) and `secret_project`, on the owner's own message only: a turn she
  started herself or an MCP caller is refused. Names and states go back, never a value.
- **Names.** With a project named, each key keeps its own name. In the default place the grant only takes `KEEP_*` and `KEPT_*`, so
  a key becomes `KEEP_<FOLDER>_<KEY>` (or `--prefix`), and the job maps key to stored name. Own names in the default place would need a
  wider grant, which is the owner's to give.
- **New Infisical projects are not hers to make.** The keeper only has `deposit` grants, and `create-project` needs an org role the broker's identity
  lacks (`upstream_unauthorized` earlier). A place with no grant is `not_allowed`, by key, and the project answer says so.
- **Using what she stored** takes a read grant for that service's caller. She has none to give.

No store-wide clear for these sources: a config value can be an ordinary word, and `postgres` must not disappear from his chats. For the same
reason a value under 16 characters isn't registered with the redactor.

### What may be read

She also reads untrusted chats and web pages, so a hidden instruction can name any path. In order:

1. `~` expands, and the pure check runs on the path as asked. Refused with the same words for every reason, before a job exists.
2. The links are resolved and the check runs again, against the roots as they really are.
3. The file opens with `openat2(RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)`, so a link swapped in after step 2 fails.
4. On the open file: regular, the user's, exactly one link, at most 64 KiB, then no NUL and valid UTF-8.

The pure check (`source_path::allowed`): clean, at least one folder below a root (`LUCINA_SECRET_KEEP_ROOTS`, default `~/.config`, `~/Projects`,
`~/mnt/mybook/Projects`), and no hit on the fixed deny lists: trees (`/etc`, `/proc`, `/run`, ...), component names in any case (`.ssh`, `.gnupg`, browser
profiles, cloud credentials, `.zcode`, Infisical, `lucina-rs`, `secrets`, `.git`, ...), name starts (`id_ed25519*`, ...) and `google_password` anywhere.
`LUCINA_SECRET_KEEP_DENY`, the keeper's token and the broker's socket folder add to them. Config can't shorten them.

A hard link is refused because it carries `~/.ssh/id_ed25519` into a project folder with no path to catch. `GOOGLE_PASSWORD` is refused as a job name, a
stored name, a prefix and an env key. Its line is in memory while the file is parsed, as every line is, but no value object is made and nothing else
sees it.

Verus proves the check equals its spec and what the spec means (`secret_source_path.rs`). `crates/lucina-secrets/tests/verus_source_path.rs` runs the real
code against a copy of the kernel on 60,000 paths and plants 17 bugs, each of which the corpus must catch.

### Clearing a source

A running service may read that file, so nothing is removed unless `--clear` is asked. Then, in order: the read-back matches; nothing is seen using it;
the file is the same one that was read (device, inode, mtime, size, mode); then `file:` is overwritten and removed, and `env:` gets a new file of the same mode, with each
moved key's lines replaced by `# KEY moved to Infisical: secret://…` and every other byte kept. What didn't save stays. If the file changed, it stays.

Seen using it (reported on every job, clear or not): an open descriptor, a working directory in its folder, the path or folder in a
process's arguments or environment, in a unit file (`LUCINA_SECRET_KEEP_UNIT_DIRS`, written `%h/`, `$HOME/`, `~/` too), in his crontab, or a compose container
started from that folder. If the check can't run, the file counts as in use. The request's own command line (`secret-keep save`, also inside a `sh -c`) doesn't count.

A clear through the chat tool always takes the owner's answer (`decide::create`, which goes to his phone for anything that handles a credential, in `settle_clears`), because a
tool call can't prove whether the words were his or came from a page. Only his own choice of "Clear it" clears. At that moment the file is read and deposited again: another
value there answers `exists`, and it stays. The CLI and the local API are the direct route, for him and for coordinators.

### Not proven

- A process that read the file at start, closed it, and names it nowhere can't be seen. Neither can another user's process (root's, a container's), which the job says.
- A bind mount of the file into a container not started by compose isn't seen.
- A shell that once sat in that folder keeps `PWD`/`OLDPWD` in its environment and counts as a user. That errs towards keeping the file.
- Overwriting doesn't reach blocks an SSD or a filesystem journal kept.
- The `.env` dialect is the one dotenv readers agree on. Tested against written cases, not against each reader. A value readers would disagree on is
  `unclear` and doesn't move.
- `not_owner` is tested on the pure check only: a test can't make a file of another owner without root.

## Setup

1. Broker: `infisical-broker token new --out ~/.config/infisical-mcp/svc-lucina-keeper.token`, then in broker.toml a caller with
   `bind_unit = "lucina-rs.service"` and `deposit` patterns (`deploy/broker.example.toml`). A root step.
2. Lucina: `LUCINA_BROKER_KEEPER_TOKEN_FILE=…` in `mode.env`. Unset, the tools are off and the old cards stay.
3. `LUCINA_SECRET_KEEP_BACKUP_DIR=/home/nez/mnt/mybook/backups/secretary-notes`.
