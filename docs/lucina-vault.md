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

## Sync: the proposal, not built

Nothing here syncs yet: each device holds a copy put there by hand for the proof. What exists today:

- The Pi is canonical. Syncthing is installed on it (1.29.5) but disabled, so the Syncthing mesh is only nezopt and the Mac, for Lucina's separate ops record.
- The Mac's `~/Documents/Obsidian` follows by `~/bin/pi-obsidian-sync`, a rsync by hand where the newer file wins and nothing warns. It has fallen behind: 1,110 notes against 2,446 on the Pi.
- `lucina-media` (Drive) is built around hashes and dedupe, not paths, and the notes service is its own store. Neither keeps a vault's folders, so neither fits.

Proposal, using what is there:

1. **Backbone: Syncthing on the Pi, turned on.** One folder between the Pi and nezopt, with staggered versions on the Pi, ignoring `.obsidian/workspace*` and `._*`. Syncthing's conflict files stay plain Markdown that Obsidian opens. nezopt holds a replica only, so the rule that notes live on the Pi stands. This needs the owner's OK, because it puts the vault on nezopt.
2. **Mac: the same Syncthing folder**, replacing `pi-obsidian-sync` and its silent newer-wins. Ignore `Lucina/`, already a separate Syncthing folder inside it.
3. **Phones and Juno: routes, not a service.** Syncthing has no official iOS client and none on Juno. Add `GET /v1/vault/manifest` (path, size, sha256), `GET /v1/vault/file` and `PUT /v1/vault/file` with the revision read, to the existing server, behind the device token, on nezopt's replica. A `sync` module in this crate pulls what differs into the device's plain folder with the same atomic writes.
4. **Writes last, one at a time.** A `PUT` is this crate's compare-and-swap; a refused one becomes a conflict copy on the device. Pull-only comes first, and writing to the real vault needs the owner's go after a run on a copy.

`lucina-vault-ingest` already writes to the Pi by compare-and-swap over SSH, so it stays compatible.
