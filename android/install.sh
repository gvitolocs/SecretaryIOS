#!/usr/bin/env bash
# Installs the built Lucina phone apps on the S20 FE over Wi-Fi adb, from nezopt (where the APKs are built).
#   android/install.sh            # both apps
#   android/install.sh otp        # Lucina OTP only
#   android/install.sh secretary  # Secretary only
#   android/install.sh status     # installed versions and the OTP app's status, installs nothing
# The phone is LUCINA_PHONE_ADB (default 192.168.178.27:5555). Wi-Fi adb must be on (`adb tcpip 5555` once over USB
# after a reboot). From the Mac over USB instead: copy the APK and run `adb -s RF8T31C33FW install -r <apk>`.
# In-place upgrades only (same signing keys), so permissions, the Doze allowlist entry and the pairing are kept.
# Prints versions and the app's status JSON; never a code or token.
set -euo pipefail
adb=${ADB:-$HOME/Android/Sdk/platform-tools/adb}
phone=${LUCINA_PHONE_ADB:-192.168.178.27:5555}
otp_apk=$HOME/.local/state/lucina-otp/build/lucina-otp.apk
secretary_apk=$HOME/.local/state/secretary-android/build/secretary.apk

"$adb" connect "$phone" >/dev/null 2>&1 || true
if ! "$adb" -s "$phone" get-state >/dev/null 2>&1; then
  echo "phone $phone not reachable: check it is on Wi-Fi and Wi-Fi adb is on (adb tcpip 5555 over USB)" >&2
  exit 1
fi

install() { # $1 = apk, $2 = package
  [[ -f "$1" ]] || { echo "missing $1; build it first" >&2; exit 1; }
  "$adb" -s "$phone" install -r "$1"
  "$adb" -s "$phone" shell dumpsys package "$2" | grep -m2 -E "versionName|versionCode" | sed 's/^ */  /'
}

case "${1:-all}" in
  status)
    for p in tech.getsecretary.lucinaotp tech.getsecretary.secretary; do
      echo "$p:"
      "$adb" -s "$phone" shell dumpsys package "$p" | grep -m2 -E "versionName|versionCode" | sed 's/^ */  /'
    done
    "$adb" -s "$phone" shell content call --uri content://tech.getsecretary.lucinaotp.pair --method status
    ;;
  otp | all)
    install "$otp_apk" tech.getsecretary.lucinaotp
    "$adb" -s "$phone" shell content call --uri content://tech.getsecretary.lucinaotp.pair --method status
    ;;&
  secretary | all)
    install "$secretary_apk" tech.getsecretary.secretary
    ;;
  otp) ;;
  *) echo "usage: $0 [otp|secretary|all|status]" >&2; exit 2 ;;
esac
