# Kindle hardware inventory (Phase 0)

Inspected on 2026-10-24, read-only, from the Mac the Kindle is plugged into. Serial numbers and other
identifiers are redacted. Every line says where its evidence came from.

## Host Mac

| Item | Value | Evidence |
|---|---|---|
| OS | macOS 27.0.1 (26A434) | `sw_vers` |
| CPU | arm64 (Apple silicon) | `uname -m` |
| Rust | rustc 1.99.1; targets `aarch64-apple-darwin`, `aarch64-apple-ios`, `aarch64-apple-ios-sim` | `rustc --version`, `rustup target list --installed` |
| Zig | not installed | `which zig` |
| Homebrew | `/opt/homebrew/bin/brew` | `which brew` |
| Free disk | 17 GiB on the data volume (92% used) | `df -h ~` |

Cross-compilation for the Kindle (ARMv7 hard-float, glibc) is not set up on the Mac. nezopt (Linux x86_64)
is the better place for it.

## Kindle on USB

| Item | Value | Evidence |
|---|---|---|
| USB vendor/product | `0x1949` (Amazon/Lab126) / `0x0004`, bcdDevice `0x0100`, "Amazon Kindle" | `ioreg -p IOUSB` |
| USB class | Mass storage (class 8, subclass 6 SCSI, protocol 0x50 bulk-only) | `ioreg` |
| SCSI identity | Vendor "Kindle", product "Internal Storage", rev 0100 | `IOBlockStorageServices` |
| Serial | `G090G1…` (rest redacted). Device code `0G1` | USB device signature |
| Model | **Kindle Paperwhite 3 (7th gen, 2015, "Muscat")** | `0G1` is in KOReader's `pw3_set` (`koreader/frontend/device/kindle/device.lua:1737`); KOReader's own `crash.log` says `initializing for device KindlePaperWhite3` |
| Display (per KOReader's device table) | 300 dpi, touch, frontlight | `device.lua` `KindlePaperWhite3` entry |
| User storage | FAT32, 3.3 GB (3,269,451,776 bytes), 342 MiB used | `diskutil info`, `df` |

The PW3 panel is 1072×1448, 8-bit grayscale (community documentation; not yet measured from the framebuffer,
which needs a shell on the device).

## USB behaviour seen

- When first checked, the Kindle was enumerated, but the SCSI unit had **no medium**: no `/dev/disk`, nothing
  under `/Volumes`. It exposed storage only later, after the owner unlocked or replugged it. This is the usual
  Kindle behaviour while it is locked or has left USB drive mode.
- On mount, macOS itself wrote `.Spotlight-V100/`, `.fseventsd/` and AppleDouble `._*` files to the Kindle (FSKit
  msdos driver). This is a write by the OS, not by our tools. `lucina-ink scan` ignores those names.

## Not accessible over USB

Root filesystem, kernel, bootloader, eMMC partitions, the framebuffer, input devices and the frontlight are not
visible over USB mass storage. Access to the user partition is **not** root access to the Kindle's Linux system.
