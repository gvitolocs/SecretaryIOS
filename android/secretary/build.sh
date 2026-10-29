#!/usr/bin/env bash
# Secretary · Lucina 0.5.0: Kotlin shell + Rust core (crates/lucina-android-calls). No Gradle, no AndroidX.
# cargo ndk -> kotlinc (embeddable compiler from the Gradle cache) -> aapt2 -> R8 -> zip -> zipalign -> apksigner.
# Env: KEYSTORE_DIR (signing.p12 + signing.pass, alias secretary), OUT, CARGO_TARGET_DIR, SKIP_RUST=1 (reuse the last .so).
# The certificate must match $OUT/secretary-0.1.0.apk (the installed 0.1.0), so the install upgrades in place.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
app="$repo/android/secretary"
sdk=${ANDROID_HOME:-/home/nez/Android/Sdk}
tools="$sdk/build-tools/35.0.0"
android="$sdk/platforms/android-35/android.jar"
ndk="$sdk/ndk/28.2.13676358"
llvm="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
export JAVA_HOME=${JAVA_HOME:-/home/nez/tools/jdk-21.0.12.1+1}
java="$JAVA_HOME/bin/java"
keys=${KEYSTORE_DIR:-/home/nez/.local/state/secretary-android}
out=${OUT:-$keys/build}
reference_apk="$out/secretary-0.1.0.apk"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/cargo-target/lucina-android-calls}

kotlin=2.2.21
m2="$HOME/.gradle/caches/modules-2/files-2.1"
m2jar() { # group artifact version -> the binary jar in the Gradle cache
  local f
  f=$(find "$m2/$1/$2/$3" -name "$2-$3.jar" 2>/dev/null | head -n1)
  [[ -n "$f" ]] || { echo "missing $1:$2:$3 in $m2" >&2; exit 1; }
  echo "$f"
}
stdlib=$(m2jar org.jetbrains.kotlin kotlin-stdlib "$kotlin")
compiler_cp=$(m2jar org.jetbrains.kotlin kotlin-compiler-embeddable "$kotlin"):$stdlib
compiler_cp+=:$(m2jar org.jetbrains.kotlin kotlin-script-runtime "$kotlin")
compiler_cp+=:$(m2jar org.jetbrains.kotlin kotlin-reflect 1.6.10)
compiler_cp+=:$(m2jar org.jetbrains.kotlin kotlin-daemon-embeddable "$kotlin")
compiler_cp+=:$(m2jar org.jetbrains.intellij.deps trove4j 1.0.20200330)
compiler_cp+=:$(m2jar org.jetbrains.kotlinx kotlinx-coroutines-core-jvm 1.8.0)
compiler_cp+=:$(m2jar org.jetbrains annotations 13.0)

[[ -f "$keys/signing.p12" && -f "$keys/signing.pass" ]] || { echo "missing signing key in $keys" >&2; exit 1; }
umask 077
mkdir -p "$out"
chmod 700 "$keys" "$out"
# Keep secretary-0.1.0.apk: it is the certificate reference.
rm -rf "$out/classes" "$out/dex" "$out/native" "$out/res" "$out/raw.apk" "$out/aligned.apk" "$out/secretary.apk" "$out/secretary.apk.idsig"
mkdir -p "$out/classes" "$out/dex" "$out/native/lib/arm64-v8a" "$out/res/drawable" "$out/res/xml"

# 1. Rust core. Size-optimised with fat LTO (Graviola's hot paths are assembly either way); panics
#    still unwind, because every JNI entry point catches them.
so="$CARGO_TARGET_DIR/aarch64-linux-android/release/liblucina_calls.so"
if [[ "${SKIP_RUST:-0}" != 1 ]]; then
  (cd "$repo" && ANDROID_NDK_HOME="$ndk" CARGO_PROFILE_RELEASE_OPT_LEVEL=s CARGO_PROFILE_RELEASE_LTO=fat \
    CARGO_PROFILE_RELEASE_PANIC=unwind cargo ndk -t arm64-v8a --platform 29 build --release -p lucina-android-calls)
fi
[[ -f "$so" ]] || { echo "no $so" >&2; exit 1; }
lib="$out/native/lib/arm64-v8a/liblucina_calls.so"
"$llvm/llvm-strip" --strip-unneeded -o "$lib" "$so"
exports=$("$llvm/llvm-nm" -D --defined-only "$lib" | awk '{print $3}' | sort)
expected=$(printf 'Java_tech_getsecretary_secretary_Native_%s\n' callerSettings callerState journal localVerdict phoneCall screenCall setCourier shareUpload transcribe ui voiceTurn | sort)
if [[ "$exports" != "$expected" ]]; then
  echo "ERROR: unexpected exports in liblucina_calls.so:" >&2
  echo "$exports" >&2
  exit 1
fi
echo "native: exports only the 11 JNI functions"

# 2. Kotlin -> classes (against android.jar only, no JDK APIs).
"$java" -Xmx1g -cp "$compiler_cp" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler \
  -no-stdlib -no-reflect -no-jdk -jvm-target 1.8 -cp "$android:$stdlib" \
  -Xno-call-assertions -Xno-param-assertions -Xno-receiver-assertions \
  -d "$out/classes" "$app"/src/*.kt "$repo"/android/common/*.kt
"$JAVA_HOME/bin/jar" cf "$out/classes.jar" -C "$out/classes" .

# 3. Manifest + icon -> base APK, plus keep rules for the manifest components.
cp "$repo/web/public/icon-192.png" "$out/res/drawable/lucina.png"
cp "$app"/res/xml/*.xml "$out/res/xml/"
"$tools/aapt2" compile --dir "$out/res" -o "$out/resources.zip"
"$tools/aapt2" link -I "$android" --manifest "$app/AndroidManifest.xml" \
  --min-sdk-version 29 --target-sdk-version 33 --proguard "$out/aapt-rules.pro" -o "$out/raw.apk" "$out/resources.zip"

# 4. R8 shrinks the Kotlin stdlib to what the shell uses.
"$java" -cp "$tools/lib/d8.jar" com.android.tools.r8.R8 --release --min-api 29 --lib "$android" \
  --pg-conf "$app/rules.pro" --pg-conf "$out/aapt-rules.pro" --pg-map-output "$out/mapping.txt" \
  --output "$out/dex" "$out/classes.jar" "$stdlib"

# 5. dex (deflated) + native library (stored, so Android maps it from the APK; page-aligned by zipalign -P 16).
zip -q -j "$out/raw.apk" "$out/dex/classes.dex"
(cd "$out/native" && zip -q -0 -r "$out/raw.apk" lib)
"$tools/zipalign" -f -P 16 4 "$out/raw.apk" "$out/aligned.apk"
"$tools/zipalign" -c -P 16 4 "$out/aligned.apk"

# 6. Sign with the 0.1.0 key.
"$tools/apksigner" sign --ks "$keys/signing.p12" --ks-key-alias secretary --ks-pass "file:$keys/signing.pass" \
  --out "$out/secretary.apk" "$out/aligned.apk"
"$tools/apksigner" verify --print-certs "$out/secretary.apk" | grep -E '^Signer #1 certificate (DN|SHA-256)'
cert() { "$tools/apksigner" verify --print-certs "$1" | sed -n 's/^Signer #1 certificate SHA-256 digest: //p'; }
if [[ -f "$reference_apk" ]]; then
  if [[ "$(cert "$out/secretary.apk")" == "$(cert "$reference_apk")" ]]; then
    echo "certificate: matches 0.1.0"
  else
    echo "ERROR: certificate differs from $reference_apk" >&2
    exit 1
  fi
else
  echo "WARNING: no $reference_apk; compare the certificate with the installed package by hand" >&2
fi
rm -f "$out/raw.apk" "$out/aligned.apk"
printf 'dex %s bytes, so %s bytes, apk %s bytes\n' "$(stat -c %s "$out/dex/classes.dex")" \
  "$(stat -c %s "$lib")" "$(stat -c %s "$out/secretary.apk")"
sha256sum "$out/secretary.apk"
