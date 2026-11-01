#!/bin/zsh
# Launch and tab-switch timing on the iPhone (App/PerfLog.swift), without Xcode attached:
#   ios/scripts/perf-selftest.sh <Secretary.app> <out dir> [runs]
# Build Release (what the owner runs; Debug builds do not start on iOS 27 from Xcode 26.6):
#   xcodebuild -project Secretary.xcodeproj -scheme Secretary -configuration Release \
#     -destination 'platform=iOS,name=<device name>' -derivedDataPath build/device \
#     -allowProvisioningUpdates DEVELOPMENT_TEAM=<team id> CODE_SIGN_STYLE=Automatic build
# Each run is a cold launch with -SecretaryPerfSelfTest: the app waits for its first frame, opens
# every tab, writes Documents/perf-selftest.txt, and this script copies the file into <out dir>.
# The phone must be unlocked with Secretary signed in. Several sessions share the phone: the lock
# directory /tmp/iphone16e.lock is held throughout (as in device-shots.sh).
set -u
app=${1:?Secretary.app}
out=${2:?out dir}
runs=${3:-3}
dev=${SECRETARY_DEVICE:-FA374B43-B705-5A50-8628-A175A5CA4201}
bundle=tech.getsecretary.app
mkdir -p "$out"
until mkdir /tmp/iphone16e.lock 2>/dev/null; do sleep 10; done
echo "perf-selftest ($out)" > /tmp/iphone16e.lock/owner
trap 'rm -f /tmp/iphone16e.lock/owner; rmdir /tmp/iphone16e.lock 2>/dev/null' EXIT
xcrun devicectl device install app --device "$dev" "$app" > "$out/install.log" 2>&1 || { echo "install failed: $out/install.log"; exit 1; }
for i in $(seq 1 "$runs"); do
  # A cold launch: the old process is terminated first. Arguments after `--` go to the app.
  xcrun devicectl device process launch --device "$dev" --terminate-existing "$bundle" -- -SecretaryPerfSelfTest > "$out/launch-$i.log" 2>&1
  # 4 s settle + 10 tab switches + the first stage load.
  sleep 45
done
xcrun devicectl device copy from --device "$dev" --domain-type appDataContainer --domain-identifier "$bundle" \
  --source Documents/perf-selftest.txt --destination "$out/perf-selftest.txt" > "$out/copy.log" 2>&1 \
  || { echo "copy failed: $out/copy.log"; exit 1; }
grep -E "launch:|tab:|lucina:|check|setup: AppModel" "$out/perf-selftest.txt" | tail -n $((runs * 20))
