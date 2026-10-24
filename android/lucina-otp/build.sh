#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
sdk=/home/nez/Android/Sdk
tools="$sdk/build-tools/35.0.0"
android="$sdk/platforms/android-35/android.jar"
src="$repo/android/otp-bridge"
out="$repo/data/android-build/otp-bridge"
rm -rf "$out/classes" "$out/dex"
mkdir -p "$out/classes" "$out/dex"
chmod 700 "$repo/data/android-build" "$out"
javac -source 8 -target 8 -cp "$android" -d "$out/classes" "$src"/src/*.java
jar cf "$out/classes.jar" -C "$out/classes" .
"$tools/d8" --release --min-api 26 --lib "$android" --output "$out/dex" "$out/classes.jar"
"$tools/aapt2" link -I "$android" --manifest "$src/AndroidManifest.xml" --min-sdk-version 26 --target-sdk-version 33 -o "$out/raw.apk"
zip -q -j "$out/raw.apk" "$out/dex/classes.dex"
mkdir -p "$out/native/lib/arm64-v8a"
cp "$repo/rust/lucina-device/target/aarch64-linux-android/release/liblucina_device.so" "$out/native/lib/arm64-v8a/"
(cd "$out/native" && zip -q -r "$out/raw.apk" lib)
"$tools/zipalign" -p -f 4 "$out/raw.apk" "$out/aligned.apk"
if [[ ! -f "$out/signing.p12" ]]; then
 umask 077
 openssl rand -hex 32 >"$out/signing.pass"
 keytool -genkeypair -keystore "$out/signing.p12" -storetype PKCS12 -storepass:file "$out/signing.pass" -keypass:file "$out/signing.pass" -alias lucina-otp -keyalg RSA -keysize 3072 -validity 3650 -dname "CN=Lucina Local OTP"
fi
"$tools/apksigner" sign --ks "$out/signing.p12" --ks-key-alias lucina-otp --ks-pass "file:$out/signing.pass" --out "$out/lucina-otp.apk" "$out/aligned.apk"
"$tools/apksigner" verify "$out/lucina-otp.apk"
sha256sum "$out/lucina-otp.apk"
