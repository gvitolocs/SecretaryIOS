# Kindle original state (Phase 0)

The user partition as found on 2026-10-24, before any write by us. Counts only: book titles and highlight text are
private and stay in the local backup.

## Layout

| Path | Contents |
|---|---|
| `documents/` | 122 top-level entries: 35 `.kfx`, 9 `.azw`, 2 `.azw3`, 1 `.azw1`, 1 `.txt` (`My Clippings.txt`), `KUAL.kual`, `run_bridge.sh`, 70 `.sdr` sidecar folders, `dictionaries/`, empty `updates/` |
| `documents/*.sdr/` | Amazon sidecars: `.asc` (96), `.kfx` assets (77), `.mf`/`.meta` (44 each), `.phl` (40), `.yjf` (36), `.yjr` (28), `.mbs` (11), `.mbp1` (10). Undocumented formats; we record their presence only |
| `documents/My Clippings.txt` | 159 entries, Italian UI strings, last modified 2025-12-08. 138 page+location highlights, 4 location-only highlights, 17 bookmarks. A BOM appears before several entry titles, not only at the start of the file, and one metadata variant has a double space after `-` |
| File-name pattern | 46 books end in `_<10-char ASIN>.<ext>` (Amazon store or Send-to-Kindle); 4 end in a 32-char id (personal documents) |
| `system/` | Firmware data: `version.txt`, indexes, thumbnails, `bookcovers/`, `vocabulary/` |
| `koreader/` | KOReader v2024.11. `settings/statistics.sqlite3` has 2 books, about 2 minutes of reading (2025-04-14): KOReader was only tried briefly |
| Jailbreak files | See `kindle-firmware-compatibility.md` |

## DRM

Most books are Amazon store files (`.kfx`/`.azw` with ASINs) and are expected to be DRM-protected. Lucina Ink reads
only unencrypted header metadata and file hashes, and never decrypts them. A store book can be listed, matched to
highlights and tracked, but not opened by the Lucina reader.

## Changes made later (the owner asked on 2026-10-24)

- Added `screensaver/lucina.png` (1072×1448 grayscale).
- In `koreader/settings.reader.lua`, `screensaver_type` changed from `disable` to `random_image`; added
  `screensaver_dir = "/mnt/us/screensaver"`; `screensaver_show_message` changed from true to false;
  `screensaver_img_background` changed from black to white.
- macOS added `._lucina.png` and `._settings.reader.lua` (AppleDouble). Harmless; KOReader ignores `._` images.
- Rollback: copy `koreader/settings.reader.lua` back from the backup and delete `screensaver/`.
