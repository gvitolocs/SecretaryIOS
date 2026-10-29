#!/usr/bin/env bash
# Host tests for the pure Kotlin logic (DialPlan.kt, CallPlan.kt): compiles them with the same Kotlin compiler as
# build.sh and runs test/DialPlanTest.kt on the JDK. No Android, no Rust, no device. Exit 1 on any failure.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
export JAVA_HOME=${JAVA_HOME:-/home/nez/tools/jdk-21.0.12.1+1}
java="$JAVA_HOME/bin/java"
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

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
"$java" -Xmx1g -cp "$compiler_cp" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -no-stdlib -no-reflect -jvm-target 11 \
  -cp "$stdlib" -d "$out" "$here/../src/DialPlan.kt" "$here/../src/CallPlan.kt" "$here/DialPlanTest.kt" 2>&1 | grep -v '^warning: ' || true
"$java" -cp "$out:$stdlib" DialPlanTestKt
