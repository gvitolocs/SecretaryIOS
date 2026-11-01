# Obsidian ingest (crate `lucina-vault-ingest`)

The vault folder `Lucina/` (nezopt `/home/nez/data/lucina-vault/Lucina`, synced to the Mac vault by
Syncthing) stays current on its own: ops records, commits on the Lucina repos and Claude session reports
become Zettelkasten notes, written by the local Qwen only while nobody else uses it. Cost: GPU time
that would otherwise sit idle; no paid model, ever.

## Behaviour reference

No earlier implementation exists to port. Searched on 2026-11-01:

- Paseo (`~/.paseo`, `@getpaseo/cli`): no Obsidian, vault or notes ingest in its code, config,
  schedules or logs.
- Node Lucina (`Archive/lucina-node`): only `scripts/sync-obsidian-from-pi.sh` (rsync of the Pi vault)
  and mentions of `pi-home` hosting Obsidian. No note writer.
- Hermes: a generic `obsidian` skill (read/search/create notes with file tools), no ingest.
- `Homelab/catalog export obsidian`: writes the `Map/` pages; `lucina-ops` writes `Log/`, `Projects/`,
  `Incidents/`. Neither writes Zettelkasten notes.

The note format follows the owner's own guidelines instead: `Templates/AI project vault notes.md`,
`Pokoin/Permanent/Pokoin Obsidian vault uses one claim per permanent note.md`, the Pokoin zettel and
event templates, and `cardvault/memory/obsidian-documentation.md`. They are encoded in
`crates/lucina-vault-ingest/guidelines/zettelkasten.md` (the model's system prompt) and
`templates/{permanent,event,moc,hub}.md` (the page shapes). Two of their rules shape the design:
"no Obsidian→Honcho auto-ingest" (the worker writes the vault only, never Honcho) and "prefer UPDATE over
a duplicate claim" (amending, below).

## Flow

1. **Scan** (every 5 minutes, cheap, no model): new ops records (`records.jsonl`, made after the first
   start minus `BACKFILL_DAYS`, never the ingest's own), new first-parent commits and merges on each
   followed ref (`git rev-list --first-parent cursor..ref`, no fetch), and session reports
   (`lucina-rs ops ingest session` from stdin). Each becomes an item with a dedup key (`ops:<id>`,
   `git:<repo>:<sha>`, `session:<hash>`), redacted and clipped, and is appended to `queue.jsonl` unless
   its key is already there.
2. **Gate** (before every model call): `GET /gpu/status` on the gateway and a non-blocking probe of
   `qcode-run`'s flock. Go only when no Qwen Code task runs, nothing is running or queued on the
   gateway, nobody waits for the GPU lease, no other engine holds the card, Qwen has been idle for
   `QUIET_SECS` (the gateway's `last-used` stamp, so back-to-back batches are not cut into), and the
   model is loaded.
   An unloaded model may be loaded only by a manual run (`--cold`, `ops_ingest_now`) or when the oldest
   item has waited `COLD_AFTER_HOURS` (default 6), and only when the card is entirely free. Anything else
   is "wait" with a backoff (3 to 15 minutes); a 429/503 from the gateway is a wait too, not a failure.
3. **Plan**: one chat completion per item (thinking off, JSON out, 1800 tokens). The answer is checked
   (`plan.rs`): redacted, no markers or wikilinks in prose, a claim title must be one sentence that is a
   valid file name and carries no secret, `files` only from the item's candidates (paths that exist),
   links only to existing claims or claims of the same answer. The checked plan is stored in the queue
   before any note is written.
4. **Write** (`notes.rs`): per claim, `Permanent/<claim>.md` from the template, or, when a claim with the
   same normalized title exists (the owner's or an earlier item's), an amendment: a dated `## Updates`
   line, missing files and links, a `## Sources` line, `updated:`; the owner's text is never rewritten.
   Then `Events/YYYY-MM-DD <source title>.md` (name from the source, not the model), `MOC/MOC <project>.md`
   and `00 Lucina hub.md` (newest events first, capped). Every added line ends in
   `<!-- ingest:<key> -->`.

## Idempotence

Keys are deduplicated at enqueue; a done or skipped key is final (`retry --any` re-arms one on purpose).
A restart after the plan was stored writes the same notes from it without asking the model again; a page
that already holds the item's marker is left as it is, and identical content is not rewritten (no
Syncthing churn). Tests: `tests/ingest.rs`.

## Failure handling

Model or write errors count attempts (backoff 5 min × 2ⁿ, at most 6 h); after 6 the item waits for
`retry`. A torn last queue line is skipped. The journal compacts itself past 4 MiB.

## Controls

- CLI on nezopt: `lucina-rs ops ingest status | scan | run [--cold] [--limit N] [--no-scan] | session < report.json | retry [keys…] [--any]`
  (`--ingest-state`, `--git` override the settings; `--vault`, `--state` as for `ops`).
- HTTP on lucina-rs: `GET /api/secretary/ops/ingest` (status), `POST /api/secretary/ops/ingest {cold}`
  (run now in the background: 202, 409 when one runs). secretary-server forwards `/v1/ops/ingest`.
- MCP (secretary-mcp): `ops_ingest_status` (read), `ops_ingest_now` (write; agents too, like the other
  `ops_` tools).

## Settings (`~/.config/lucina-rs/*.env`)

| Variable | Default |
| --- | --- |
| `LUCINA_VAULT_INGEST` | off; `on` starts the loop (needs `LUCINA_OPS_VAULT_DIR`) |
| `LUCINA_VAULT_INGEST_STATE_DIR` | `<ops state>/ingest` |
| `LUCINA_VAULT_INGEST_QWEN_URL` | `http://127.0.0.1:11436` (loopback only; anything else is refused) |
| `LUCINA_VAULT_INGEST_QWEN_KEY_FILE` | `~/.config/llama-server-api.key` |
| `LUCINA_VAULT_INGEST_MODEL` | `qwen3.8:27b-q4-72k` |
| `LUCINA_VAULT_INGEST_QCODE_LOCK` | `~/.cache/qcode/lock` |
| `LUCINA_VAULT_INGEST_LAST_USED_FILE` | `~/.local/state/llama-server-qwen/last-used` |
| `LUCINA_VAULT_INGEST_QUIET_SECS` | 120 (Qwen idle at least this long before a request) |
| `LUCINA_VAULT_INGEST_GIT` | `lucina-rs=…/SecretaryIOS@origin/main#lucina;lucinaos=…/lucinaos@usb-dev-loop#lucinaos` |
| `LUCINA_VAULT_INGEST_COLD_AFTER_HOURS` | 6 (0: only manual runs load the model) |
| `LUCINA_VAULT_INGEST_BACKFILL_DAYS` | 2 |
| `LUCINA_VAULT_INGEST_BACKFILL_COMMITS` | 10 |
| `LUCINA_VAULT_INGEST_BATCH` | 6 model calls per run |
| `LUCINA_VAULT_INGEST_TEMPLATES` | a directory overriding `guidelines/` and `templates/` files |
