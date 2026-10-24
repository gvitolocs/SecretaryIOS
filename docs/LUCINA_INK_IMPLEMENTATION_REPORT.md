# Lucina Ink — implementation report

Status as of 2026-10-24. Branch `lucina-ink` in the SecretaryIOS repo, worktree
`/home/nez/Projects/.worktrees/SecretaryIOS/lucina-ink` on nezopt. **Nothing is deployed**: no production host,
service or the owner's live Secretary data has been touched. The Kindle was only read, apart from the KOReader
sleep screen the owner asked for.

Status words used below:
- **Verified**: implemented, then run and checked in this session.
- **Unverified**: implemented, but not run where it matters.
- **Mock-only**: works only against an emulator or test double.
- **Blocked**: cannot proceed without something outside this session.
- **Not implemented.**

## 1. Summary

| Area | Status | Evidence |
|---|---|---|
| Kindle detection and inventory (Phase 0) | Verified | USB descriptors, `system/version.txt`, KOReader device table and log; see `docs/kindle-hardware-inventory.md` |
| User-storage backup (Phase 1) | Verified | 2,070 files and 343,288 KiB; every SHA-256 re-checked against the Kindle (`docs/kindle-backup-report.md`) |
| Lucina, Secretary and Obsidian inventory (Phase 2) | Verified | `docs/lucina-secretary-inventory.md` |
| USB import, Mode A (`lucina-ink` CLI) | Verified on the real Kindle | Read-only scan (before/after snapshot identical), 48 files and 151 clippings, 14 s |
| Library model, merge and dedup (`ink-library`) | Verified | Property tests; real-data re-import is idempotent |
| Secretary Library API (`secretary-server`) | Verified on a local test server with the real data | Import, search, recent, stats, PATCH, brief, sync, notes index |
| Secretary web Library view | Verified in a browser on a local test server | List, detail, owner status change, extractive brief |
| Lucina MCP tools (`library_*`, 10 tools) | Verified on local test servers | Every tool called; results carry citations and state unknowns plainly |
| Obsidian adapter (`ink-obsidian`) | Verified read-only on the real vault | 1,211 notes indexed; 0 book references, confirmed by grep (the vault mentions none of the books) |
| Sync engine, Mode B (`ink-sync` + `POST /v1/library/sync`) | Verified in tests and models; no device client yet | 5 Stateright cases, 25 Verus proofs, TLC specs, fault-injection tests |
| Native reader core (`ink-epub`, `ink-render`, `ink-kindle-hal`) | Mock-only | Desktop E Ink emulator; static ARMv7 build links but has never run on the Kindle |
| Reader sleep, idle and splash screens with the Lucina picture | In progress | Branch `ink-screens` (subagent) |
| KOReader sleep screen with the Lucina picture | Implemented, unverified | Files written to the Kindle; the owner has not yet confirmed it on the device |
| Stock lock screen and boot splash | Not implemented by choice | Needs a third-party hack (lock screen) or a system-partition change (boot); both need explicit approval |
| Lucina memory (Honcho) references to books | Not implemented | The design is in §6; Honcho MCP was offline this session |
| Production deployment | Not implemented | Needs the owner's go-ahead |
| Native app on the device (Phase 12) | Blocked | Needs the owner's approval of the KUAL install route and on-device testing |

## 2. Repository and commits

All work is on branch `lucina-ink`, based on `main` at `a371fd6`: 15 commits, 130 files, about 22,400 added lines.

| Commit | Content |
|---|---|
| `eee5ba1` | `ink-library`, `ink-cli`, Library API, MCP tools (Codex) |
| `b82aeee` | Fixes from the real-Kindle scan: titles and ASINs, cp1252, clipping matching, canonical dedup keys, Topaz, dictionaries, homebrew markers (Codex, staged; reviewed and committed by Claude) |
| `e6c841e` | Merge `lucina-ink-web`, `e2a769d`: web Library view, typed API client, translations (Codex) |
| `d97b1af` | Phase 0–2 docs |
| `6738e47` | Merge `ink-obsidian`, `6ec5471`: Obsidian adapter and notes-index routes (subagent) |
| `d37c447` | Merge `ink-reader`, `a356c94`: reader core, emulator, ARMv7 cross build (subagent) |
| `c52eac5` | Justification limit raised to 8× the space width |
| `453d23b` | Merge `ink-sync`, `7f06349` and `6bb878b`: sync engine, Stateright/Verus/TLA+ (subagent) |
| `9e94204` | Library writes rewrite only changed books |
| `141d546` | Highlights in reading order, authors from clippings, old-format dictionaries |

New crates:
- `crates/ink-library`
- `crates/ink-cli` (binary `lucina-ink`)
- `crates/ink-obsidian`
- `crates/ink-sync`
- `crates/ink-epub`
- `crates/ink-render`
- `crates/ink-kindle-hal`
- `apps/desktop-preview` (binary `ink-preview`)

Changed crates:
- `secretary-server`: new `src/library.rs` module
- `secretary-mcp`: `library_*` tools
- `web/`: Library view
- `verification/`, `formal/` and `scripts/verify.sh`

The structure is adapted from the brief's suggested `lucina-ink/` workspace. The crates live in the existing
SecretaryIOS workspace, because the server, MCP and web app are already here.

## 3. Tests actually run

Results on nezopt (AMD Ryzen 5 7600, Linux), last run at commit `141d546`:

```
cargo fmt --all --check                                   → clean
cargo clippy -p ink-library -p ink-cli -p ink-obsidian -p ink-sync -p ink-kindle-hal -p ink-render \
  -p ink-epub -p ink-preview -p secretary-server -p secretary-mcp --all-targets -- -D warnings → clean
cargo test  (same packages)                               → 354 passed, 0 failed
```

From the `ink-sync` branch run (subagent), re-checked through the merged test suite:

```
scripts/verify.sh --only models  → GREEN (41 cases; the 7 ink-sync cases include 2,949,071 states for I1, plus
                                   5 mutation cases that must fail and do)
scripts/verify.sh --only verus   → GREEN (library_merge.rs: 25 verified, 0 errors)
scripts/verify.sh --only tlc     → GREEN (InkSync_status 35,800 states, InkSync_highlights 17,556)
```

Web (`web/`, branch `lucina-ink-web`):

```
npm run typecheck → clean · vitest 45 files / 301 tests passed · vite build → ok
```

Covered by tests:
- USB-import fixtures: English and Italian clippings, BOMs, malformed entries.
- Hand-built MOBI, EPUB and KFX containers, including truncated, garbage and DRM-wrapped ones.
- Malicious file names and symlinks escaping the root.
- Idempotent and duplicate imports; owner-status protection.
- Offline and reconnect via a flaky transport; conflict resolution.
- MCP invocation; Obsidian parsing and link resolution.
- Emulated E Ink navigation, and anchors that survive font changes.

### End to end with real data

A local test server on nezopt with a throwaway data directory and test credentials; the owner's live server was not
involved.

| Check | Result |
|---|---|
| Real scan from the Mac | 48 files (38 books, 8 dictionaries, 2 documents), 151 clippings; the Kindle is byte-identical afterwards (names, sizes, mtimes) |
| First import | 50 books (48 files plus 2 clipping-only), 151 highlights, about 1.1 s including the CLI |
| Second import | 50 unchanged, 151 skipped |
| Owner status after a re-import | Kept (`completed`, source `owner`) |
| Save time for one edit | 1,100 ms → 13–20 ms after `9e94204` |
| Highlights in reading order, authors from clippings | Confirmed (`Kishimi, Ichiro`; locations 212, 212, 218, 297…) |
| Web view (in-app browser, through an SSH tunnel) | List, detail, status change to completed, extractive brief labelled as generated |
| MCP tools | Search with citations; "progress unknown"; "reading sessions unavailable on stock Kindle"; brief labelled "not the owner's words" |

## 4. Hardware verified

- **Model:** Kindle Paperwhite 3 (2015, "muscat"), device code `0G1`.
- **Firmware:** 5.16.2.1.1.
- **Jailbreak:** already present (WinterBreak 1.7.0, KUAL, MRPI, OTA blocker), with KOReader v2024.11 installed on 2025-04-08.
- **Unverified:** the jailbreak hotfix.

What was not measured, because it needs a shell on the device:
- the framebuffer layout;
- touch axes;
- refresh timings and power;
- whether the device runs the ARMv7 binary at all.

No hardware performance numbers are claimed.

## 5. Emulator results

These come from `docs/emulator-validation.md`, measured on nezopt with a release build.

| Measure | Result |
|---|---|
| Open the synthetic 2 MB book | 17–29 ms |
| Paginate the whole book | 83–89 ms |
| Page turn | median about 0.7 ms |
| Font change | about 90 ms |
| Peak memory | 33–47 MB |
| Partial updates | each page turn updates only the changed regions; a full GC16 refresh every 6 updates |

The PW3's single 1 GHz Cortex-A9 is expected to be at least ten times slower. E Ink waveform timing and power are not
emulated.

## 6. Lucina integration

**Built**
- `library_*` tools in `secretary-mcp`, so Claude Code, ChatGPT and Grok get them through the existing OAuth and
  token auth, with read and write scopes.
- Every answer cites `[bk_… p. loc, source]`.
- Unknowns (progress, reading time, related notes) are stated, never guessed.
- Briefs are extractive and labelled as generated.

**Kept apart:**
- Reading activity and status changes: `library/activity.json`.
- Highlights with provenance: `library/highlights/`.
- Generated briefs: on the book record, `kind: extractive`.
- Note links: `library/notes/`.
- Model summaries and semantic retrieval are **not implemented**.

**Next:**
- Long-term memory references in Honcho: write only `{bookId, highlightId?, location, at}` pointers, never book
  text.
- Model briefs through the existing local models on nezopt, opt-in.
- Register the same tools in the Rust Lucina (`lucina-tools`, branch `lucina-rs`).

## 7. Obsidian

There was no working integration before; Hermes had only a prompt skill, pointing at a vault path that doesn't exist.

The new `ink-obsidian` adapter:
- reads an approved vault root only, with privacy excludes on by default;
- never follows symlinks, and re-indexes incrementally;
- resolves wikilinks the way Obsidian does;
- detects book references conservatively;
- can generate book notes, with marker-based writes that need approval (nothing is written to the vault yet).

The real vault on `pi-home` was indexed from a temporary copy, which was then deleted.

## 8. Physical Kindle

**Changed (the owner asked):**
- Added `/mnt/us/screensaver/lucina.png`.
- KOReader screensaver settings, 4 lines in `koreader/settings.reader.lua`.
- Rollback: restore that file from the backup and delete `screensaver/`.

**Not changed:** firmware, partitions, the boot splash, the Amazon UI's lock screen. No homebrew was installed.

**Phase 12 (native app) needs the owner's approval of:**
1. Turning on KOReader's SSH (dropbear) for read-only inspection of fb0, input devices and the frontlight.
2. Installing a KUAL extension that launches the static `ink-preview` / reader binary from `/mnt/us/extensions/`.
   Rollback is deleting that folder.

## 9. Known limitations and open items

**Reader**
- No hyphenation, so justified lines can show wide gaps.
- Italic needs a second font file.
- Images are placeholders.
- Pagination runs over the whole book on open and on each font change.

**KFX books**
- No title or author comes from the container metadata. They come from the file name and, now, the clippings.
- DRM state stays "unknown"; this is by design, since no DRM work is done.

**Stock Kindle**
- No reading progress or sessions over USB. The API says so explicitly.
- KOReader's `statistics.sqlite3` is a future import source (it is barely used today).

**Server**
- One invalid notes-index reference rejects the whole POST.
- The conflict log is excluded from the sync convergence proof.
- Verus proves a simplified model, not the real Rust code.

**Infrastructure**
- `ClearAllForwardings yes` in the Mac's SSH config for `nezopt` silently drops `-L` tunnels; pass
  `-o ClearAllForwardings=no`.
- ZCode headless runs are **blocked**: the CLI's registry hides the Start Plan account and falls back to a plan
  whose login fails. The desktop app works.

**Not started**
- Phase 5 benchmarks with real academic or scanned PDFs (`ink-pdf`).
- Weekly reading summaries.
- iOS Library view.

## 10. How to run it

```bash
# On the Mac with the Kindle mounted (read-only):
lucina-ink scan /Volumes/Kindle
lucina-ink import /Volumes/Kindle --server https://<secretary> --token-file <device-token-file> --dry-run

# Emulator:
ink-preview book.epub --out /tmp/pages --font crates/ink-render/fixtures/LiberationSerif-Regular.ttf --pages 5

# Kindle cross build (static, links only; not run on the device):
cargo build --release --target armv7-unknown-linux-musleabihf -p ink-preview
```
