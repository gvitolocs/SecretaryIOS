#!/usr/bin/env bash
# Deploy-time only. Never trace or echo the Google key.
set -euo pipefail
umask 077
key_file="${MAPS_GOOGLE_KEY_FILE:-$HOME/.config/secretary-maps/google.key}"
mkdir -p -- "$(dirname -- "$key_file")"
key_tmp="$(mktemp "${key_file}.XXXXXX")"
trap 'rm -f -- "$key_tmp"' EXIT
"$HOME/bin/infisical" export --domain http://127.0.0.1:8088/api --projectId f06a1c6c-7e23-41fa-99be-6cc1dd3c4283 --env dev --format json |
    python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except (ValueError, TypeError):
    sys.stderr.write("Cannot parse Infisical export JSON\n")
    sys.exit(2)
# Infisical JSON exports use name/value entries; also accept object exports.
if isinstance(data, list):
    value = next((entry.get("value") for entry in data if isinstance(entry, dict) and entry.get("key") == "GOOGLE_MAPS_SERVER_KEY"), None)
elif isinstance(data, dict):
    value = data.get("GOOGLE_MAPS_SERVER_KEY")
else:
    value = None
if not isinstance(value, str) or not value.strip():
    sys.stderr.write("GOOGLE_MAPS_SERVER_KEY missing from Infisical dev export\n")
    sys.exit(3)
sys.stdout.write(value.strip() + "\n")
' > "$key_tmp"
mv -f -- "$key_tmp" "$key_file"
