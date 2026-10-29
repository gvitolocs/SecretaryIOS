#!/usr/bin/env bash
# Read-only checks of the phone role on the S20 FE over adb. Places, answers and rejects NOTHING, changes no
# setting, never dials (not even 112: the "emergency" method only reports the route). Run only while Wolt is idle.
# Usage: device-check.sh [adb serial or host:port]
set -uo pipefail
adb=${ADB:-$HOME/Android/Sdk/platform-tools/adb}
[[ $# -gt 0 ]] && adb="$adb -s $1"
pkg=tech.getsecretary.secretary
echo "== installed version"
$adb shell dumpsys package $pkg | grep -E "versionName|versionCode" | head -2
echo "== phone role: holders (Secretary is NOT expected until the owner taps)"
$adb shell cmd role get-role-holders android.app.role.DIALER
echo "== call screening role holders"
$adb shell cmd role get-role-holders android.app.role.CALL_SCREENING
echo "== is the role offered to Secretary? (qualifying activities and the in-call service)"
$adb shell cmd package query-activities --brief -a android.intent.action.DIAL | grep -c "$pkg" | sed 's/^/DIAL activities of Secretary: /'
$adb shell cmd package query-activities --brief -a android.intent.action.DIAL -d tel:123456 | grep "$pkg" | head -2
$adb shell cmd package query-services --brief -a android.telecom.InCallService | grep "$pkg"
$adb shell cmd package query-services --brief -a android.telecom.CallScreeningService | grep "$pkg"
echo "== missed-call and voicemail receivers"
$adb shell cmd package query-receivers --brief -a android.telecom.action.SHOW_MISSED_CALLS_NOTIFICATION | grep "$pkg"
$adb shell cmd package query-receivers --brief -a android.telephony.action.SHOW_VOICEMAIL_NOTIFICATION | grep "$pkg"
echo "== current default dialer according to Telecom"
$adb shell dumpsys telecom | grep -iE "default dialer|system dialer|DefaultDialer" | head -5
echo "== the real Samsung phone package"
$adb shell pm list packages | grep -iE "dialer|incallui" 
echo "== emergency routing (dry run, nothing is dialed)"
for n in 112 911 999 112123 +4566778702 "*21#"; do
  printf '%s -> ' "$n"
  $adb shell content call --uri content://$pkg.calls --method emergency --arg "$n" | head -1
done
$adb kill-server
