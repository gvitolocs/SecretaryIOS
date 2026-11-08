# Obsidian ingest (crate `lucina-vault-ingest`)

The owner's Obsidian vault stays current on its own. Ops records, commits on the Lucina repos and Claude
session reports become Zettelkasten notes. The local Qwen drafts them only while nobody else uses it, and
they are delivered to the canonical vault on the Raspberry Pi. Cost: GPU time that would otherwise sit
idle. No paid model is ever used.

## Where notes go

The canonical vault is `/home/nez/mnt/mybook/Obsidian` on nezopt's MyBook disk, written as a local folder (`dir:` store).
It moved there from the Pi (`/home/nes/Obsidian` on `pi-home`, reached as `pi-cursor`) on 2026-11-08 at the
owner's request; the Pi copy is a read-only backup, and the owner keeps no Obsidian copy on the Mac. Honcho session
`obsidian-pi-rule-20260828` was updated that day (new session `obsidian-nezopt-move-20261108`): never write
Obsidian notes anywhere else on nezopt or inside a git repo. The `ssh:` store below is kept for a remote vault.

Until 2026-11-08 a Mac copy followed by `~/bin/pi-obsidian-sync` (rsync, newer wins); that script is retired and
the owner removed the Mac copy. The nezopt folder `/home/nez/data/lucina-vault/Lucina` is
something else: Lucina's ops record (Log, Projects, Incidents, Map), synced to the Mac's `Lucina/` folder by
Syncthing. The ingest never writes there, and no folder it writes is called `Lucina`. A Pi `Lucina/`
would mix with that Syncthing folder through `pi-obsidian-sync`.

Notes go into the project's own folder, next to its `00 <Folder> hub`, `Permanent/`, `Events/` and
`MOC/`. The folder comes from the project slug through ordered rules (`LUCINA_VAULT_INGEST_FOLDERS`):

| Projects | Folder |
| --- | --- |
| `lucinaos`, `lucina-ink` | `LucinaOS` |
| `lucina*`, `secretary*`, `gallery*`, `ops` | `LucinaRS` |
| `pokoin*`, `tcgprices` | `Pokoin` |
| `nezopt`, `nespc*`, `hosts`, `p1`, `p2`, `pi`, `edge`, `infisical`, `honcho`, `homelab*`, `peer1*` | `Infra` |
| `orchestra`, `lanes`, `delegation-lanes`, `jev-agents*` | `Harness` |
| `hermes` | `Hermes` |

A project with no folder is skipped without asking Qwen.

## Behaviour reference

No earlier implementation exists to port. Searched on 2026-11-01:

- Paseo (`~/.paseo`, `@getpaseo/cli`): no Obsidian, vault or notes ingest in its code, config,
  schedules or logs.
- Node Lucina (`Archive/lucina-node`): only `scripts/sync-obsidian-from-pi.sh`, an rsync of the Pi
  vault.
- Hermes: a generic `obsidian` skill (file tools), no ingest.

The note format follows the owner's guidelines:

- `Templates/AI project vault notes.md` (2026-09-22);
- `Pokoin/Permanent/Pokoin Obsidian vault uses one claim per permanent note.md`;
- `Infra/Permanent/Knowledge owns Obsidian vault and BigBrain owns Honcho memory.md`;
- the Pokoin zettel and event templates;
- `cardvault/memory/obsidian-documentation.md`;
- the Honcho rule above.

They are encoded in `crates/lucina-vault-ingest/guidelines/zettelkasten.md` (the model's system prompt) and
`templates/{permanent,event,moc,hub}.md` (the page shapes). Two of these rules shape the design. First, no
Obsidian→Honcho auto-ingest: the worker writes the vault only. Second, prefer UPDATE over a duplicate
claim: claims are amended, as described below.

## Flow

1. **Scan** (every 5 minutes, cheap, no model, no vault). Sources:
   - new ops records (`records.jsonl`, made after the first start minus `BACKFILL_DAYS`, never the
     ingest's own);
   - new first-parent commits and merges on each followed ref (`git rev-list --first-parent
     cursor..ref`, no fetch);
   - session reports (`lucina-rs ops ingest session` from stdin).

   Each becomes an item with a dedup key (`ops:<id>`, `git:<repo>:<sha>`, `session:<hash>`). It is
   redacted, clipped and appended to `queue.jsonl` on nezopt, unless its key is already there.
2. **Draft** (needs Qwen). Before every call the gate reads the gateway's `GET /gpu/status`, probes
   `qcode-run`'s flock, and reads the gateway's `last-used` stamp. It goes ahead only when all of these
   hold:
   - no Qwen Code task runs;
   - nothing runs or is queued on the gateway;
   - nobody waits for the GPU lease;
   - no other engine holds the card;
   - Qwen has been idle for `QUIET_SECS`;
   - the model is loaded.

   An unloaded model may be loaded only by a manual run (`--cold`, `ops_ingest_now`) or when the oldest
   item has waited `COLD_AFTER_HOURS`, and only when the card is entirely free. Anything else is "wait"
   with a backoff. A 429 or 503 from the gateway is a wait too.

   The answer (one JSON chat completion, thinking off) is checked (`plan.rs`):
   - every text is redacted, with no markers or wikilinks in prose;
   - a claim title must be one sentence, a valid file name, and free of secrets;
   - `files` may only come from the item's candidates;
   - links may only point to existing claims or to claims of the same answer.

   The checked draft is stored in the queue. A readable preview goes to `<state>/staging/<hash>.md`,
   which is neither a vault nor a repo.
3. **Deliver** (needs the vault). Each page is read, amended and written back with compare-and-swap. On
   the Pi this is one `ssh pi-cursor python3 -` call per operation over a shared control connection. A
   small helper confines paths to the vault root, refuses symlinks, writes through a temp file and
   `os.replace`, and refuses the write when the page changed since it was read.

   Per item, delivery touches these pages:
   - **Claims:** `<Folder>/Permanent/<claim>.md` comes from the template. If a claim with the same
     normalized title exists (the owner's or an earlier item's), it is amended instead: a dated
     `## Updates` line, any missing files and links, a `## Sources` line and `updated:` are added. The
     owner's text is never rewritten.
   - **Event:** `<Folder>/Events/YYYY-MM-DD <source title>.md`. The name comes from the source, not the
     model.
   - **Change log:** `<Folder>/MOC/MOC <Folder> change log.md`.
   - **Hub:** `<Folder>/00 <Folder> hub.md`. Its own `Events` or `Recent events` section is used,
     newest first. A new hub is created from the template and linked from `00 Home.md`: a row in its
     Projects table, or a bullet. `00 Home.md` itself is never created.

   Every added line ends in `<!-- ingest:<key> -->`.

## Pi offline

A failed SSH connection (exit 255, timeout) is "offline". The hosts in the target are tried in order:
`pi-cursor`, then a tunnel alias if configured. While offline:

- drafting continues while Qwen is free;
- drafts stay in the queue and in `staging/`;
- delivery backs off 10 minutes;
- no attempt is counted;
- status shows `vault.offline_since` and `undelivered`.

When the Pi is back, the stored drafts are delivered without asking Qwen again. A compare-and-swap refusal
(someone edited the page meanwhile) is retried on the next run against the fresh text.

## Idempotence

Keys are deduplicated at enqueue. A done or skipped key is final; `retry --any` re-arms one on purpose. A
restart after the draft was stored delivers the same draft. A page that already holds the item's marker is
left as it is, and identical content is never rewritten.

Tests are in `tests/ingest.rs` and `src/*`. The real SSH channel has an opt-in test:
`VAULT_INGEST_SSH_TEST=pi-cursor:/tmp/x cargo test -p lucina-vault-ingest -- --ignored`.

## Failure handling

Model errors and vault errors other than offline or conflict count as attempts, with a backoff of
5 min × 2ⁿ, at most 6 h. After 6 attempts the item waits for `retry`. A torn last queue line is skipped. The
journal compacts itself past 4 MiB.

## Controls

- CLI on nezopt: `lucina-rs ops ingest status | scan | run [--cold] [--limit N] [--no-scan] | session < report.json | retry [keys…] [--any]`.
  `--ingest-state` and `--git` override the settings; `--vault` and `--state` work as for `ops`.
- HTTP on lucina-rs: `GET /api/secretary/ops/ingest` returns the status. `POST /api/secretary/ops/ingest
  {cold}` runs now in the background and answers 202, or 409 when a run is already going. secretary-server
  forwards `/v1/ops/ingest`.
- MCP (secretary-mcp): `ops_ingest_status` (read) and `ops_ingest_now` (write). Agent tokens may call
  both, like the other `ops_` tools.

## Settings (`~/.config/lucina-rs/*.env`)

| Variable | Default |
| --- | --- |
| `LUCINA_VAULT_INGEST` | off; `on` starts the loop (needs the ops record on) |
| `LUCINA_VAULT_INGEST_TARGET` | `dir:/home/nez/mnt/mybook/Obsidian` (`ssh:<host>[,<host>]:<root>` or `dir:<path>`; a dir inside a git repo is refused) |
| `LUCINA_VAULT_INGEST_FOLDERS` | the table above, as `slug=Folder;prefix*=Folder;…` |
| `LUCINA_VAULT_INGEST_STATE_DIR` | `<ops state>/ingest` (queue, state, staging; the SSH control socket is in `$XDG_RUNTIME_DIR`) |
| `LUCINA_VAULT_INGEST_QWEN_URL` | `http://127.0.0.1:11436` (loopback only; anything else is refused) |
| `LUCINA_VAULT_INGEST_QWEN_KEY_FILE` | `~/.config/llama-server-api.key` |
| `LUCINA_VAULT_INGEST_MODEL` | `qwen3.8:27b-q4-72k` |
| `LUCINA_VAULT_INGEST_QCODE_LOCK` | `~/.cache/qcode/lock` |
| `LUCINA_VAULT_INGEST_LAST_USED_FILE` | `~/.local/state/llama-server-qwen/last-used` |
| `LUCINA_VAULT_INGEST_QUIET_SECS` | 120 |
| `LUCINA_VAULT_INGEST_GIT` | `lucina-rs=…/SecretaryIOS@origin/main#lucina;lucinaos=…/LucinaOS@main#lucinaos` |
| `LUCINA_VAULT_INGEST_COLD_AFTER_HOURS` | 6 (0: only manual runs load the model) |
| `LUCINA_VAULT_INGEST_BACKFILL_DAYS` | 2 |
| `LUCINA_VAULT_INGEST_BACKFILL_COMMITS` | 10 |
| `LUCINA_VAULT_INGEST_BATCH` | 6 drafts per run |
| `LUCINA_VAULT_INGEST_TEMPLATES` | a directory overriding `guidelines/` and `templates/` files |
