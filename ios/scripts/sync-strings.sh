#!/usr/bin/env bash
# Refreshes the String Catalog from the code and merges the Italian/Danish translations.
#   ios/scripts/sync-strings.sh        (after adding or changing UI strings)
# New English strings are reported as missing; add them to ios/l10n/translations.json.
set -euo pipefail
ios="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ios"
xcodebuild -project Secretary.xcodeproj -scheme Secretary -configuration Debug \
    -destination 'generic/platform=iOS Simulator' -derivedDataPath build CODE_SIGNING_ALLOWED=NO build -quiet
files=()
while IFS= read -r f; do files+=("$f"); done < <(find build -name "*.stringsdata" -path "*Secretary.build*" | grep -v Tests)
xcrun xcstringstool sync Secretary/Resources/Localizable.xcstrings --stringsdata "${files[@]}"
cargo run -q --manifest-path tools/l10n-merge/Cargo.toml -- Secretary/Resources/Localizable.xcstrings l10n/translations.json "$@"
