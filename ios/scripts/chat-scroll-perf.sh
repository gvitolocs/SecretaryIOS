#!/bin/zsh
# How soon the chat scrolls after a cold launch and after a return from the background, on the iPhone
# (ScrollTrace in App/PerfLog.swift, driven by SecretaryUITests/ChatScrollPerfTests):
#   ios/scripts/chat-scroll-perf.sh <derived data dir> <out dir> [runs]
# Build first (Release, what the owner runs):
#   xcodebuild build-for-testing -project Secretary.xcodeproj -scheme SecretaryReleaseUI \
#     -destination 'id=<udid>' -derivedDataPath build/dd -allowProvisioningUpdates \
#     DEVELOPMENT_TEAM=<team id> CODE_SIGN_STYLE=Automatic
# Each run is one cold launch and one return from the background while the test keeps dragging the
# list; the app appends both timelines to Documents/perf-scroll.txt, which this script copies into
# <out dir>. The phone must be unlocked with Secretary signed in. Several sessions share the phone:
# the lock directory /tmp/iphone16e.lock is held throughout (as in perf-selftest.sh).
set -u
dd=${1:?derived data dir}
out=${2:?out dir}
runs=${3:-2}
udid=${SECRETARY_UDID:-00008140-001225CE1A03801C}
dev=${SECRETARY_DEVICE:-FA374B43-B705-5A50-8628-A175A5CA4201}
bundle=tech.getsecretary.app
here=${0:A:h}
mkdir -p "$out"
until mkdir /tmp/iphone16e.lock 2>/dev/null; do sleep 10; done
echo "chat-scroll-perf ($out)" > /tmp/iphone16e.lock/owner
trap 'rm -f /tmp/iphone16e.lock/owner; rmdir /tmp/iphone16e.lock 2>/dev/null' EXIT
for i in $(seq 1 "$runs"); do
  xcodebuild test-without-building -project "$here/../Secretary.xcodeproj" -scheme SecretaryReleaseUI \
    -destination "id=$udid" -derivedDataPath "$dd" \
    -only-testing:SecretaryUITests/ChatScrollPerfTests > "$out/test-$i.log" 2>&1 \
    || echo "run $i: the UI test failed, see $out/test-$i.log"
done
xcrun devicectl device copy from --device "$dev" --domain-type appDataContainer --domain-identifier "$bundle" \
  --source Documents/perf-scroll.txt --destination "$out/perf-scroll.txt" > "$out/copy.log" 2>&1 \
  || { echo "copy failed: $out/copy.log"; exit 1; }
grep -E "^== scroll trace|^first reader scroll" "$out/perf-scroll.txt" | tail -n $((runs * 4))
