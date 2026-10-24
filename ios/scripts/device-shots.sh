#!/bin/zsh
# Screenshots of every tab on the iPhone, without XCUITest (Xcode 26.6 cannot test- or
# debug-launch on iOS 27, and Debug builds do not start there):
#   ios/scripts/device-shots.sh <Secretary.app> <out dir>
# Build the app as Release with the SCREENSHOTS condition so it reads -SecretaryInitialTab:
#   xcodebuild ... -configuration Release 'SWIFT_ACTIVE_COMPILATION_CONDITIONS=$(inherited) SCREENSHOTS' build
# Needs pymobiledevice3 (it opens the iOS 17+ developer tunnel itself; run outside the sandbox).
# Several sessions share the phone: the lock directory /tmp/iphone16e.lock is held throughout.
set -u
app=${1:?Secretary.app}
out=${2:?out dir}
dev=${SECRETARY_DEVICE:-FA374B43-B705-5A50-8628-A175A5CA4201}
mkdir -p "$out"
until mkdir /tmp/iphone16e.lock 2>/dev/null; do sleep 10; done
echo "device-shots ($out)" > /tmp/iphone16e.lock/owner
trap 'rm -f /tmp/iphone16e.lock/owner; rmdir /tmp/iphone16e.lock 2>/dev/null' EXIT
xcrun devicectl device install app --device "$dev" "$app" > "$out/install.log" 2>&1 || { echo "install failed: $out/install.log"; exit 1; }
i=0
for tab in chat notes tasks shopping gym weight settings; do
  i=$((i+1))
  # `--` so devicectl passes -SecretaryInitialTab to the app instead of parsing it.
  xcrun devicectl device process launch --device "$dev" --terminate-existing tech.getsecretary.app -- -SecretaryInitialTab $tab > /dev/null 2>&1
  sleep 7
  pymobiledevice3 developer dvt screenshot "$out/0$i-$tab.png" > /dev/null 2>&1 || echo "$tab: screenshot failed"
done
echo "$(ls "$out"/*.png 2>/dev/null | wc -l | tr -d ' ') screenshots in $out"
