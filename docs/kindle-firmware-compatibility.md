# Kindle firmware and homebrew compatibility (Phase 0)

## Firmware

`system/version.txt` on the user partition reads **`Kindle 5.16.2.1.1 (409747 002)`** (file dated 2023-05-01).
The owner can confirm it on the Kindle under Settings → Device Options → Device Info.

## Homebrew already present (installed 2025-04-08)

Seen on the user partition. None of it was installed by us.

| Component | Evidence | Meaning |
|---|---|---|
| WinterBreak 1.7.0 jailbreak (uses Mesquito) | `winterbreak.log`, `jb.sh`, `mesquito/`, `README.md` | The log says the developer keys were installed (`pubdevkey01.pem`, `uks.sqsh`), with the developer and "mntus exec" flags enabled |
| Patched update keystore | `patchedUks.sqsh`, `mkk/updater_keys.sqsh`, `mkk/developer.keystore` | Lets the Kindle accept homebrew update packages |
| KUAL | `extensions/KUAL.cfg`, `documents/KUAL.kual` | Launcher for extensions |
| MR Package Installer (MRPI) | `extensions/MRInstaller/`, empty `mrpackages/` | Installs `Update_*.bin` homebrew packages |
| OTA blocker | `extensions/renameotabin/` | Stops Amazon firmware updates that would remove the jailbreak |
| KOReader v2024.11 | `koreader/git-rev`, `extensions/koreader/` | Alternative reader; it ships `dropbear` (SSH) and `fbink` |
| libkh / FBInk | `libkh/bin/fbink` | Framebuffer drawing helper |

**Unverified:** the log ends with "Please Install HOTFIX now". No evidence on the user partition shows whether the
hotfix (which keeps the jailbreak across reboots and updates) was installed. Check it on the device: KUAL lists a
"Rerun Hotfix" or "Run Hotfix" entry once it is installed.

## Consequences for Lucina Ink

- Mode A (stock Kindle, USB import) works with no device change.
- Mode B (native reader) has an existing route that needs **no new jailbreak and no firmware change**: a KUAL
  extension that starts an ARMv7 binary from `/mnt/us/extensions/…`, the same way KOReader starts. This still
  needs the owner's explicit approval before anything is installed (Phase 12).
- A root shell for read-only inspection (framebuffer layout, input devices, frontlight sysfs, real rootfs) is
  possible through KOReader's SSH plugin (dropbear) over Wi-Fi. That is also a change to the device's state and
  needs approval.
- Boot splash and stock lock screen live in the root filesystem or boot partition. Changing them is a system
  modification and is not attempted.

## Sources

Local evidence on the device, KOReader's device table, and the WinterBreak README shipped on the device.
Current upstream documentation (kindlemodding.org, MobileRead) has not been re-checked in this session; check it
before any Phase 12 action.
