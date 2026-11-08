#!/usr/bin/env bash
# Deploy-time only. Never trace or echo a Google key.
# Writes the server key (GOOGLE_MAPS_SERVER_KEY, required: exit 3 when missing) and the apps'
# client keys (GOOGLE_MAPS_WEB_KEY, GOOGLE_MAPS_IOS_KEY: a warning when missing) from one export.
set -euo pipefail
umask 077
key_dir="$HOME/.config/secretary-maps"
server_file="${MAPS_GOOGLE_KEY_FILE:-$key_dir/google.key}"
web_file="${MAPS_GOOGLE_WEB_KEY_FILE:-$key_dir/google-web.key}"
ios_file="${MAPS_GOOGLE_IOS_KEY_FILE:-$key_dir/google-ios.key}"
# Held in memory only; printf is a builtin, so the export never reaches argv or disk.
export_json="$("$HOME/bin/infisical" export --domain http://127.0.0.1:8088/api --projectId f06a1c6c-7e23-41fa-99be-6cc1dd3c4283 --env dev --format json)"

key_tmp=""
trap 'if [ -n "$key_tmp" ]; then rm -f -- "$key_tmp"; fi' EXIT

# sync <secret> <file>: 0 written, 3 missing; a parse error exits the script with 2.
sync() {
    local name="$1" file="$2" status=0
    mkdir -p -- "$(dirname -- "$file")"
    key_tmp="$(mktemp "${file}.XXXXXX")"
    printf '%s' "$export_json" | python3 -c '
import json, sys
name = sys.argv[1]
try:
    data = json.load(sys.stdin)
except (ValueError, TypeError):
    sys.stderr.write("Cannot parse Infisical export JSON\n")
    sys.exit(2)
# Infisical JSON exports use name/value entries; also accept object exports.
if isinstance(data, list):
    value = next((entry.get("value") for entry in data if isinstance(entry, dict) and entry.get("key") == name), None)
elif isinstance(data, dict):
    value = data.get(name)
else:
    value = None
if not isinstance(value, str) or not value.strip():
    sys.stderr.write(name + " missing from Infisical dev export\n")
    sys.exit(3)
sys.stdout.write(value.strip() + "\n")
' "$name" > "$key_tmp" || status=$?
    if [ "$status" -eq 0 ]; then
        mv -f -- "$key_tmp" "$file"
    else
        rm -f -- "$key_tmp"
    fi
    key_tmp=""
    if [ "$status" -ne 0 ] && [ "$status" -ne 3 ]; then
        exit "$status"
    fi
    return "$status"
}

server_status=0
sync GOOGLE_MAPS_SERVER_KEY "$server_file" || server_status=$?
for client in "GOOGLE_MAPS_WEB_KEY:$web_file" "GOOGLE_MAPS_IOS_KEY:$ios_file"; do
    if ! sync "${client%%:*}" "${client#*:}"; then
        echo "warning: ${client%%:*} not written; the apps get null for it from /v1/maps/config" >&2
    fi
done
exit "$server_status"
