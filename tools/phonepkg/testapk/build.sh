#!/usr/bin/env bash
# Builds a throwaway APK with no code (tech.getsecretary.pkgtest) for testing phonepkg.
# Usage: build.sh <versionCode> <versionName> <out.apk>
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 <versionCode> <versionName> <out.apk>" >&2
  exit 2
fi
vc=$1 vn=$2 out=$3
[[ $vc =~ ^[0-9]+$ ]] || { echo "versionCode must be a number" >&2; exit 2; }
[[ $vn =~ ^[A-Za-z0-9._+-]+$ ]] || { echo "versionName may only use A-Z a-z 0-9 . _ + -" >&2; exit 2; }

sdk=$HOME/Android/Sdk
bt=$sdk/build-tools/36.0.0
jar=$sdk/platforms/android-33/android.jar

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cat >"$work/AndroidManifest.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    package="tech.getsecretary.pkgtest"
    android:versionCode="$vc"
    android:versionName="$vn">
    <uses-sdk android:minSdkVersion="29" android:targetSdkVersion="33" />
    <application android:label="pkgtest" android:hasCode="false" android:debuggable="true" />
</manifest>
XML

"$bt/aapt2" link --manifest "$work/AndroidManifest.xml" -I "$jar" -o "$work/unaligned.apk"
"$bt/zipalign" -p -f 4 "$work/unaligned.apk" "$work/aligned.apk"
"$bt/apksigner" sign --ks "$HOME/.android/debug.keystore" --ks-key-alias androiddebugkey \
  --ks-pass pass:android --key-pass pass:android --out "$work/signed.apk" "$work/aligned.apk"
mv "$work/signed.apk" "$out"
echo "$out"
