#!/usr/bin/env bash
# SDK UI glue + Rust application/network/journal logic. No Gradle, Node or Python runtime.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
sdk=/home/nez/Android/Sdk
tools="$sdk/build-tools/35.0.0"
android="$sdk/platforms/android-35/android.jar"
rust=/home/nez/Projects/Lucina/rust/lucina-device
src="$repo/android/app"
out=/home/nez/.local/state/secretary-android/build
keys=/home/nez/.local/state/secretary-android
mkdir -p "$out";chmod 700 "$keys" "$out"
rm -rf "$out/classes" "$out/dex" "$out/native" "$out/res"
mkdir -p "$out/classes" "$out/dex" "$out/native/lib/arm64-v8a" "$out/res/drawable"
(cd "$rust" && CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$sdk/ndk/28.2.13676358/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang" cargo build --release --lib --no-default-features --features android-network --target aarch64-linux-android)
cp "$rust/target/aarch64-linux-android/release/liblucina_device.so" "$out/native/lib/arm64-v8a/"
variant=arm64
if [[ "$#" -gt 0 ]]; then variant="$1"; fi
if [[ "$variant" == "--universal" ]]; then
 (cd "$rust" && CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$sdk/ndk/28.2.13676358/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android29-clang" cargo build --release --lib --no-default-features --features android-network --target x86_64-linux-android)
 mkdir -p "$out/native/lib/x86_64"
 cp "$rust/target/x86_64-linux-android/release/liblucina_device.so" "$out/native/lib/x86_64/"
fi
cp "$repo/web/public/icon-192.png" "$out/res/drawable/lucina.png"
javac -encoding UTF-8 -source 8 -target 8 -cp "$android" -d "$out/classes" "$src"/src/*.java
jar cf "$out/classes.jar" -C "$out/classes" .
"$tools/d8" --release --min-api 29 --lib "$android" --output "$out/dex" "$out/classes.jar"
"$tools/aapt2" compile --dir "$out/res" -o "$out/resources.zip"
"$tools/aapt2" link -I "$android" --manifest "$src/AndroidManifest.xml" --min-sdk-version 29 --target-sdk-version 33 -o "$out/raw.apk" "$out/resources.zip"
zip -q -j "$out/raw.apk" "$out/dex/classes.dex"
(cd "$out/native" && zip -q -r "$out/raw.apk" lib)
"$tools/zipalign" -p -f 4 "$out/raw.apk" "$out/aligned.apk"
if [[ ! -f "$keys/signing.p12" ]]; then
 umask 077
 openssl rand -hex 32 >"$keys/signing.pass"
 keytool -genkeypair -keystore "$keys/signing.p12" -storetype PKCS12 -storepass:file "$keys/signing.pass" -alias secretary -keyalg RSA -keysize 3072 -validity 3650 -dname "CN=Secretary Lucina Local"
fi
"$tools/apksigner" sign --ks "$keys/signing.p12" --ks-key-alias secretary --ks-pass "file:$keys/signing.pass" --out "$out/secretary.apk" "$out/aligned.apk"
"$tools/apksigner" verify "$out/secretary.apk"
sha256sum "$out/secretary.apk"
