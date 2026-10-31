#!/bin/zsh
# Screenshots of the Calendar, Instagram, Pixiv and NClient screens on the iPhone, against the live server:
#   ios/scripts/plugin-shots.sh <Secretary.app> <out dir>
# Build the app as Release with the SCREENSHOTS condition (it reads -SecretaryOpenRoute and the plugin hooks):
#   xcodebuild ... -configuration Release 'SWIFT_ACTIVE_COMPILATION_CONDITIONS=$(inherited) SCREENSHOTS' build
# The calendar shots run CalendarSelfTest: one event in Lucina's calendar for tomorrow, renamed, then deleted; its
# log is copied to <out dir>/calendar-selftest.log. Needs pymobiledevice3 (see device-shots.sh).
# SHOTS="calendar pixiv" takes only the named groups. The lock directory /tmp/iphone16e.lock is held throughout.
set -u
app=${1:?Secretary.app}
out=${2:?out dir}
dev=${SECRETARY_DEVICE:-FA374B43-B705-5A50-8628-A175A5CA4201}
mkdir -p "$out"
until mkdir /tmp/iphone16e.lock 2>/dev/null; do sleep 10; done
echo "plugin-shots ($out)" > /tmp/iphone16e.lock/owner
trap 'rm -f /tmp/iphone16e.lock/owner; rmdir /tmp/iphone16e.lock 2>/dev/null' EXIT
xcrun devicectl device install app --device "$dev" "$app" > "$out/install.log" 2>&1 || { echo "install failed: $out/install.log"; exit 1; }

wanted() { [[ -z ${SHOTS:-} || " $SHOTS " == *" $1 "* ]] }
launch() { xcrun devicectl device process launch --device "$dev" --terminate-existing tech.getsecretary.app -- "$@" > /dev/null 2>&1 }
shot() { pymobiledevice3 developer dvt screenshot "$out/$1.png" > /dev/null 2>&1 || echo "$1: screenshot failed" }

if wanted calendar; then
  # The self-test creates the event ~5 s after launch and holds each state 10 s.
  launch -SecretaryOpenRoute /calendar -secretary.calendar.view month
  sleep 7; shot calendar-1-month
  launch -SecretaryOpenRoute /calendar -secretary.calendar.view month -SecretaryCalendarSelfTest YES
  sleep 9; shot calendar-2-created
  sleep 10; shot calendar-3-edited
  sleep 10; shot calendar-4-deleted
  xcrun devicectl device copy from --device "$dev" --domain-type appDataContainer --domain-identifier tech.getsecretary.app \
    --source Documents/calendar-selftest.log --destination "$out/calendar-selftest.log" > /dev/null 2>&1 \
    || echo "calendar: self-test log not copied"
  launch -SecretaryOpenRoute /calendar -secretary.calendar.view agenda
  sleep 7; shot calendar-5-agenda
  launch -SecretaryOpenRoute /calendar -secretary.calendar.view month
fi
if wanted instagram; then
  launch -SecretaryOpenRoute /instagram
  sleep 8; shot instagram-1-home
  # A cold profile takes 10-35 s on the backend.
  launch -SecretaryOpenRoute /instagram/natgeo
  sleep 30; shot instagram-2-profile
  launch -SecretaryOpenRoute /instagram/natgeo -SecretaryInstagramTab highlights -SecretaryInstagramHighlight first
  sleep 40; shot instagram-3-highlight
fi
if wanted pixiv; then
  launch -SecretaryOpenRoute /pixiv
  sleep 10; shot pixiv-1-ranking
  launch -SecretaryOpenRoute /pixiv -SecretaryPixivOpen first
  sleep 18; shot pixiv-2-work
fi
if wanted nclient; then
  launch -SecretaryOpenRoute /nclient
  sleep 10; shot nclient-1-browse
fi
if wanted plugins; then
  launch -SecretaryInitialTab plugins
  sleep 6; shot plugins
fi
echo "$(ls "$out"/*.png 2>/dev/null | wc -l | tr -d ' ') screenshots in $out"
