# lucina-vault: an Obsidian vault as plain files, on every platform

`crates/lucina-vault` is one Rust core for reading and carefully writing an Obsidian vault. Juno links it directly;
the iOS and Mac apps reach it through `secretary-ffi`, Android through JNI. A vault stays Markdown files and nothing
else: no database, no index and no state of ours inside it, so real Obsidian opens it as if we were never there.

**The original is real Obsidian** (`docs/porting-guidelines.md`). It is closed source, so its behaviour is captured
from the running program: `tools/vault-oracle/run.sh` runs Obsidian headless on a throwaway copy and dumps what its
metadata cache decided. `tests/fixture/obsidian-golden.json` and `rename-golden.json` are those dumps, from 1.15.0.

## Candidates

| Candidate | Licence | Verdict | Why |
|---|---|---|---|
| `crates/ink-obsidian` | ours | **adopt** the safe walk, frontmatter split and globs | Its link and tag rules are simpler than Obsidian's and serve book references, so they stay as they are; the book parts moved behind the default feature `books` |
| [Rustidian](https://github.com/MasihMoafi/rustidian) 6c212be | MIT | **reference** | Native egui, but eframe glow and winit on X11/Wayland, 517 crates and an 11 MB embedded font. No code taken. Its idea of refusing a save when the file changed underneath is here too |
| [Opensidian](https://github.com/jpzk/opensidian) 7be2132 | GPL-3.0 | **reference**, facts only | Tauri with a JavaScript UI, no webview on Juno, and GPL cannot enter this tree |
| `pulldown-cmark` 0.13 | MIT | **adopt** for the reading view | Never with `ENABLE_WIKILINKS` (RUSTSEC-2026-0414): wikilinks are ours |

Rustidian's audit (static only, nothing built or run) found no hidden network or payload. Note for anyone who does run it:
it fetches remote images when a note is rendered.

## What the core does

- `scan`: links, embeds, tags, headings, block ids and frontmatter links of one note, by Obsidian's rules.
- `resolve`: a link's target, by Obsidian's rules. An exact path from the root wins even over a sibling; `../` counts
  from the note's folder; otherwise any path ending in the target, those under the note's folder first, then the
  shortest path string. Aliases never resolve a link.
- `vault`: the open vault with no note text in memory: backlinks, unresolved links, tag counts, folders. `refresh`
  rereads only what changed (mtime and size).
- `search`: words, `"phrases"`, `-not`, `tag:`, `path:`, `file:`, ignoring case and accents; `quick_switch` for names and aliases.
- `graph`: nodes, edges and a repeatable force layout; whole vault, by tag, or around a note.
- `markdown`: a note as blocks and styled runs, so a native view never parses Markdown.
- `write`: see below.
- `protocol`: JSON in, JSON out, for Swift and Kotlin. Operations: `status`, `set_writable`, `refresh`, `list`, `note`,
  `resolve`, `file_path`, `search`, `quick_switch`, `tags`, `tag_notes`, `unresolved`, `graph`, `save`, `conflict_copy`,
  `create`, `rename`, `trash`. Each answer has `ok`; a failure adds `code` (`invalid`, `read_only`, `not_found`,
  `conflict`, `exists`, `io`) and `error`.

## Writes and the conflict policy

A vault opens **read-only**. `set_writable` or `Options::writable` must say otherwise.

- **Save is compare-and-swap.** It names the revision (sha256 of the bytes) it read. If the file is no longer that,
  nothing is written and the answer is `conflict`.
- **A conflict loses nothing.** The caller keeps its text as a new note beside the original,
  `Note (conflict 2026-11-02 phone).md`, an ordinary note in real Obsidian. The other side's file stays as it is.
- **Atomic and byte exact.** A temporary dot file in the same folder, flushed, renamed, folder flushed. A save writes what
  it was given: CRLF and BOM stay. Rename changes only the links.
- **Inside the vault.** No dot paths, no `..`, no symlink on the way, never `.obsidian`.
- **Nothing is deleted.** `trash` moves to `.trash/`, where Obsidian looks.
- **Rename updates links like Obsidian does**, with one deliberate difference: Obsidian rewrites a note's whole YAML
  when it touches a link there and drops a BOM; we change only the link.

The window that remains: a writer that replaces the file in the milliseconds between our last check and our rename
is overwritten. `tests/writes.rs` does not cover that race; a sync tool that keeps versions covers it in practice.

## What is proven

Numbers from nezopt (x86-64, release build, under other sessions' load), on a copy of the owner's vault taken 2026-11-02
without voice memos: 2,446 notes, 41 other files, 11 MB of Markdown.

| Check | Result |
|---|---|
| Per note, against real Obsidian 1.15.0: links, embeds, frontmatter links, headings, with targets and lines; tag counts for the vault | **10,195 of 10,195 agree** |
| Same, on the hand-made fixture of edge cases (`tests/fixture/vault`) | all agree (`cargo test -p lucina-vault`) |
| Five renames in a row: bodies byte for byte against what Obsidian itself wrote | agree (`renames_match_real_obsidian`) |
| Real Obsidian opens a copy after 98 saves, a create, a conflict copy, a rename with 10 links, and a trash | opens; 10,200 of 10,200 agree, unresolved links unchanged at 119 |
| Open | 110 ms |
| Search for a common word across all notes | 80 ms |
| Whole-vault graph with layout, 2,419 nodes, 12,963 edges | 35 ms |
| Reading view for every note, 34,772 blocks | 85 ms |

To repeat: copy a vault, then
`OBSIDIAN=<extracted AppImage>/obsidian tools/vault-oracle/run.sh <copy> <golden.json>` and
`cargo run --release -p lucina-vault --example real_copy -- <copy> <golden.json>`. `--example exercise_copy -- <copy>` makes the written copy.
A copy and its golden hold private notes: they stay out of the repo.

## Not proven in the core

- Only the checks above: the reading view (`markdown`) has unit tests and was not compared with Obsidian's rendering.
- Search is ours. It takes Obsidian's main operators, not all of them, and its ranking is not Obsidian's.
- Not built: unlinked mentions, canvas files, plugins, themes, Dataview and other community plugins' syntax.
- YAML is not fully parsed. Broken frontmatter is detected for the mistakes found in the fixture and the real vault; a rarer
  one would keep tags that Obsidian drops.
- A note with invalid UTF-8 is skipped with a warning. None in the real vault.

## The platforms

| Platform | How it reaches the core | Where the copy is | Proven on a copy of the real vault |
|---|---|---|---|
| Juno | `crates/lucina-app-vault` in LucinaOS, pinned git dependency | `/data/vault` | See LucinaOS `docs/research/juno-vault.md` |
| iOS | `secretary_vault_*` in `secretary-ffi`, `ios/Secretary/Features/Vault` | `Documents/Vault` in the app's container | See "Device results" |
| Android | `Native.vault` in `lucina-android-calls`, `android/secretary/src/VaultActivity.kt` | `Android/data/tech.getsecretary.secretary/files/Vault` | See "Device results" |
| Mac | `lucina-vault-stdio`, the same protocol over stdin and stdout, because Secretary Code is Electron and links no Rust | `~/Documents/Obsidian`, read-only | See "Device results" |

Every platform opens a vault read-only and asks before the first edit. The Mac page has no editing at all yet. No platform fetches a remote picture.

## Sync

Built 2026-11-08 for Juno (and later the phones), after the vault moved to nezopt.

### Where the copies are

- **nezopt:** `/home/nez/mnt/mybook/Obsidian` holds the canonical vault. The Pi's `/home/nes/Obsidian` is a read-only backup.
- **Mac:** the owner keeps no copy there (his word, 2026-11-08), so it has no Syncthing folder for the vault.
- **Juno:** its own plain copy in `/data/vault`, kept in step by the code below.

### How a device syncs

The device's folder and the server each list their files in a manifest: path, size and sha256. Then:

- **Each file is decided against the base.** The base is the revision both sides had at the last sync, kept in a state
  file outside the vault (Juno: `/data/lucina/vault/sync.json`). `crate::sync_rules::decide` makes the call.
- **Every write is compare-and-swap on both sides.** It names the revision the manifest showed, so a file that changed
  meanwhile is left alone and decided again.
- **Conflict copies.** A conflict copy is `Note (conflict 2026-11-08 juno).md`, an ordinary note, and the next round
  sends it to the server.
- **Code.**
  - `src/sync_rules.rs`: the decisions and the path rules, proved.
  - `src/sync.rs`: `Folder` for both ends, `plan`, `run`, and the `Remote` trait.
  - `src/sync_tests.rs`: two-folder runs, including a server change in the middle of a run.

| Here | Server | Base | Act |
|---|---|---|---|
| same as server | | any | record |
| changed | unchanged or gone | any | push (server copy expected) |
| unchanged or gone | changed | any | pull (local copy expected) |
| unchanged | gone | yes | to `.trash` here |
| gone | unchanged | yes | to `.trash` on the server |
| changed | changed differently | any, or none | keep this side's text as a conflict copy, then pull |

### Paths

- Only clean relative paths take part. These are skipped:
  - dot names: `.trash`, `.stversions`, `.lucina-vault-*.tmp`, `._*`;
  - control bytes and backslashes;
  - an empty name;
  - a name over 255 bytes or a path over 1024.
- `.obsidian/<name>.json` comes from the server and never goes back. The server refuses to write it.
- Files over 64 MB are left out.
- Symlinks are never listed or followed.

### Proof (Verus, D-011: the shipped file itself)

- `scripts/verify-shipped.sh`: 8 verified, 0 errors.
- `verification/vault-sync/mutants.sh`: 16 of 16 planted bugs refused. Those bugs are:
  - an edit trashed, overwritten or pushed over;
  - a deletion winning over an edit;
  - a conflict settled by one side;
  - dot names, backslashes, `//` or `.obsidian` folders let through;
  - settings pushed back.
- What the proof covers: an edit on either side is never lost, a deletion yields to an edit, and only an unchanged base
  copy goes to the trash. Each act has what it needs.
- What tests cover instead: that the facts match the revisions, and the compare-and-swap in `sync.rs`.

### Routes

- **Lucina (lucina-server `vault_sync.rs`).** `/api/secretary/vault/{manifest,file,trash}`, behind the Secretary bearer
  like every `/api/secretary` route.
  - `LUCINA_VAULT_SYNC_DIR` sets the folder, default `/home/nez/mnt/mybook/Obsidian`.
  - If the folder is missing (MyBook not mounted) the answer is 503, never an empty manifest.
- **secretary-server (`vault.rs`).** `/v1/vault/{manifest,file,trash}` for owner devices only (no MCP, no service
  tokens), counted on the media limiter. Calls go over the Link like `/v1/notebooks`.

### Syncthing

Syncthing no longer carries this vault anywhere; the owner removed the Mac folder. If a Syncthing folder comes back on
nezopt:
- the server's writes are atomic renames from `.lucina-vault-*.tmp` names, which `.stignore` must ignore;
- Syncthing's own trash versioning keeps what a millisecond race might replace.

The ingest (`lucina-vault-ingest`) and lucina-memo write the same folder locally. They also write by compare-and-swap or
create, so a device's push and their writes never overwrite each other silently.
