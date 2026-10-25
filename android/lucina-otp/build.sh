#!/usr/bin/env bash
# Lucina OTP 1.0.1: Kotlin shell + Rust core (crates/lucina-android). No Gradle, no network.
# kotlinc (embeddable compiler from the Gradle cache) -> R8 -> aapt2 -> zip -> zipalign -> apksigner.
# Env: KEYSTORE_DIR (signing.p12 + signing.pass, alias lucina-otp), OUT, SKIP_RUST=1 (reuse the last .so).
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
app="$repo/android/lucina-otp"
sdk=${ANDROID_HOME:-/home/nez/Android/Sdk}
tools="$sdk/build-tools/35.0.0"
android="$sdk/platforms/android-35/android.jar"
ndk="$sdk/ndk/28.2.13676358"
export JAVA_HOME=${JAVA_HOME:-/home/nez/tools/jdk-21.0.12.1+1}
java="$JAVA_HOME/bin/java"
keys=${KEYSTORE_DIR:-/home/nez/Projects/Lucina/data/android-build/otp-bridge}
reference_apk="$keys/lucina-otp.apk" # installed 0.4.0, for the certificate check
out=${OUT:-$HOME/.local/state/lucina-otp/build}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/cargo-target/lucina-android-kt}

kotlin=2.2.21
m2="$HOME/.gradle/caches/modules-2/files-2.1"
m2jar() { # group artifact version -> path of the binary jar in the Gradle cache
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

umask 077
mkdir -p "$out"
chmod 700 "$out"
rm -rf "$out/classes" "$out/dex" "$out/native" "$out"/*.apk "$out"/*.idsig
mkdir -p "$out/classes" "$out/dex" "$out/native/lib/arm64-v8a"

# 1. Rust core.
so="$CARGO_TARGET_DIR/aarch64-linux-android/release/liblucina_android.so"
if [[ "${SKIP_RUST:-0}" != 1 ]]; then
  (cd "$repo" && ANDROID_NDK_HOME="$ndk" cargo ndk -t arm64-v8a --platform 26 build --release --offline -p lucina-android)
fi
[[ -f "$so" ]] || { echo "no $so" >&2; exit 1; }
"$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip" --strip-unneeded -o "$out/native/lib/arm64-v8a/liblucina_android.so" "$so"
if "$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm" -D --defined-only "$out/native/lib/arm64-v8a/liblucina_android.so" \
  | grep -q ' Java_tech_getsecretary_lucinaotp_Native_init$'; then
  echo "native: JNI exports present"
else
  echo "WARNING: liblucina_android.so has no Java_tech_getsecretary_lucinaotp_Native_* exports (skeleton crate?)" >&2
fi

# 2. Kotlin -> classes.
"$java" -Xmx1g -cp "$compiler_cp" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler \
  -no-stdlib -no-reflect -jvm-target 1.8 -cp "$android:$stdlib" \
  -Xno-call-assertions -Xno-param-assertions -Xno-receiver-assertions \
  -d "$out/classes" "$app"/src/*.kt "$repo"/android/common/*.kt
"$JAVA_HOME/bin/jar" cf "$out/classes.jar" -C "$out/classes" .

# 3. Manifest -> base APK, plus keep rules for the manifest components.
"$tools/aapt2" link -I "$android" --manifest "$app/AndroidManifest.xml" \
  --min-sdk-version 26 --target-sdk-version 35 --proguard "$out/aapt-rules.pro" -o "$out/raw.apk"

# 4. R8 shrinks the Kotlin stdlib to what the shell uses.
"$java" -cp "$tools/lib/d8.jar" com.android.tools.r8.R8 --release --min-api 26 --lib "$android" \
  --pg-conf "$app/rules.pro" --pg-conf "$out/aapt-rules.pro" --pg-map-output "$out/mapping.txt" \
  --output "$out/dex" "$out/classes.jar" "$stdlib"

# 5. dex (deflated) + native library (stored, page-aligned by zipalign -P 16).
zip -q -j "$out/raw.apk" "$out/dex/classes.dex"
(cd "$out/native" && zip -q -0 -r "$out/raw.apk" lib)
"$tools/zipalign" -f -P 16 4 "$out/raw.apk" "$out/aligned.apk"
"$tools/zipalign" -c -P 16 4 "$out/aligned.apk"

# 6. Sign with the 0.4.0 key, so the install upgrades in place.
"$tools/apksigner" sign --ks "$keys/signing.p12" --ks-key-alias lucina-otp --ks-pass "file:$keys/signing.pass" \
  --out "$out/lucina-otp.apk" "$out/aligned.apk"
"$tools/apksigner" verify --print-certs "$out/lucina-otp.apk" | grep -E '^Signer #1 certificate (DN|SHA-256)'
cert() { "$tools/apksigner" verify --print-certs "$1" | sed -n 's/^Signer #1 certificate SHA-256 digest: //p'; }
if [[ -f "$reference_apk" ]]; then
  if [[ "$(cert "$out/lucina-otp.apk")" == "$(cert "$reference_apk")" ]]; then
    echo "certificate: matches 0.4.0"
  else
    echo "ERROR: certificate differs from $reference_apk" >&2
    exit 1
  fi
fi
rm -f "$out/raw.apk" "$out/aligned.apk"
printf 'dex %s bytes, so %s bytes, apk %s bytes\n' "$(stat -c %s "$out/dex/classes.dex")" \
  "$(stat -c %s "$out/native/lib/arm64-v8a/liblucina_android.so")" "$(stat -c %s "$out/lucina-otp.apk")"
sha256sum "$out/lucina-otp.apk"
