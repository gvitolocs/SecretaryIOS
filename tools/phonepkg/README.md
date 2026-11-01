# phonepkg

Installs, updates and removes apps on the owner's Samsung S20 FE (SM-G780G, serial
`RF8T31C33FW`, Android 13) over the air. It drives the SDK `adb` against the phone over
Tailscale (`100.89.7.91:5555`). It runs its own adb server on port 5094, never the shared
5037 one, and kills that server when it finishes, also on errors and on Ctrl-C.

## Install

```sh
cargo install --path tools/phonepkg --root ~/.local     # -> ~/.local/bin/phonepkg on nezopt
```

On the Mac, copy `mac/phonepkg` onto the PATH. It runs the command on nezopt over ssh
(`nezopt` on the home LAN, `nezopt-remote` elsewhere). With `install <apk>`, an APK under
`~/mnt/nezopt/` is used in place. Any other APK is copied to nezopt's
`~/scratch/phonepkg/` first and deleted again after the run.

## Commands

```sh
phonepkg list [--all] [--json]                # third-party apps (--all: every package)
phonepkg install app.apk [--wait 20] [--json] # install or update, keeps data
phonepkg uninstall com.foo [-k] [--force] [--wait 20] [--json]
phonepkg status [--json]                      # reachability, device facts, Wolt gate
phonepkg rearm [--full]                       # re-enable adb on TCP 5555 after a reboot
phonepkg pair 192.168.1.20:41011 123456       # one-time Wireless debugging pairing
```

- `install` reads the package and versions from the APK with `aapt2`. It always refuses a
  downgrade (an installed versionCode above the APK's), and no flag overrides this. After
  installing, it checks that the phone reports the new versionCode.
- `uninstall` always refuses system packages. Protected apps need `--force`: Wolt,
  Secretary, Lucina OTP, Lucina Maps, Play Store and Play services. `-k` keeps the app's
  data.
- Every install and uninstall attempt adds one JSON line to `$PHONEPKG_STATE/log.jsonl`.

## The Wolt gate

The owner is a Wolt courier, so `install` and `uninstall` refuse while Wolt might be in
use. Wolt counts as busy if any of these is true:

- Wolt is the resumed (on-screen) activity.
- Wolt has a foreground service.
- Wolt is requesting location right now, according to the providers section of
  `dumpsys location`. The timestamped request history is ignored.

If any of these reads fails, Wolt counts as busy. `--wait <minutes>` checks again every
30 s until Wolt is idle or the time runs out. Without it, phonepkg checks once and prints
why it refused.

## After a reboot

A reboot turns off adb's TCP mode. On any Wi-Fi, tap the **Wireless debugging** quick
settings tile, then run any command. phonepkg runs `rearm` by itself when it cannot connect.
`rearm` looks for the wireless-debugging port, first over mDNS and then by scanning ports
30000-50000 on the phone's Tailscale IP (`--full`: 1024-65535). It connects through that
port and switches adb back to TCP 5555.

## One-time pairing

You only need this if nezopt's adb key is not trusted yet. On the phone, open Developer
options -> Wireless debugging -> Pair device with pairing code, then run
`phonepkg pair <ip:port> <code>` with the address and the 6-digit code shown there.

## Configuration

| Variable | Default |
| --- | --- |
| `PHONEPKG_ADB` | `$HOME/Android/Sdk/platform-tools/adb` |
| `PHONEPKG_AAPT2` | `$HOME/Android/Sdk/build-tools/36.0.0/aapt2` |
| `PHONEPKG_TARGET` | `100.89.7.91:5555` |
| `PHONEPKG_SERIAL` | `RF8T31C33FW` (phonepkg refuses any other device) |
| `PHONEPKG_ADB_PORT` | `5094` (phonepkg's own adb server; 5037 is rejected) |
| `PHONEPKG_STATE` | `$HOME/.local/state/phonepkg` (log directory) |

## Test APK

`testapk/build.sh <versionCode> <versionName> <out.apk>` builds `tech.getsecretary.pkgtest`,
an APK with no code that you can install and uninstall safely.
