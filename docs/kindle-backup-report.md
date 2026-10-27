# Kindle backup report (Phase 1)

## What was backed up

| Item | Value |
|---|---|
| Taken | 2026-10-24 13:36 CEST, before any write to the Kindle |
| Location | Mac: `~/KindleBackups/pw3-20261024-133609/` (outside every git repository) |
| Source | `/Volumes/Kindle/` (USB user partition, FAT32) |
| Method | `rsync -a`, excluding the `.Spotlight-V100` and `.fseventsd` folders that macOS creates |
| Files | 2,070 on the Kindle, 2,070 in the backup (same `find -type f` count) |
| Size | 343,288 KiB on disk |
| Manifest | `manifest.sha256`: SHA-256 of every file, made from the backup |
| Verification | `shasum -a 256 -c manifest.sha256` run against the Kindle: all 2,070 files OK |
| Free space before copy | 17 GiB on the Mac data volume |

FAT32 has no Unix owners or permissions, and `rsync -a` keeps modification times. Hidden Kindle folders
(`.active_content_sandbox`, `.active-content-data`, `.Trash`) are included.

## What this backup is not

It is a **user-data backup** (books, clippings, sidecars, KOReader data, jailbreak files on the user partition).
It is **not** a firmware or eMMC backup: the root filesystem, kernel, bootloader, `/var/local` (where the stock
reader keeps its reading positions and collections database) and the device keys are not reachable over USB.
No raw eMMC dump was attempted.

## Restore

- One file: copy it from `data/` back to `/Volumes/Kindle/`.
- Everything: `rsync -a ~/KindleBackups/pw3-20261024-133609/data/ /Volumes/Kindle/` (no `--delete`, so files
  added later stay), then eject the Kindle.

## Recovery options for this exact device (PW3, FW 5.16.2.1.1)

These are from community documentation and untested here:
- Stock firmware can be reinstalled by copying Amazon's official `update_kindle_*.bin` for the PW3 to the user
  partition and running "Update Your Kindle". This removes homebrew, and `renameotabin` must be undone first.
- A factory reset from Settings wipes the user partition. Restore books and clippings from this backup afterwards.
- Serial-console or USB-downloader recovery needs opening the case and is out of scope.
