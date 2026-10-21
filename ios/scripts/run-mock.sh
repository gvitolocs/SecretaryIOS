#!/usr/bin/env bash
# Runs the web app's in-memory mock API for the iOS app, with an iOS overlay.
#
#   ios/scripts/run-mock.sh start|stop|restart|status
#
# The mock (web/mock/server.mjs and its modules) is copied into ios/.mock/
# (gitignored): over SSH from nezopt (it may be uncommitted there), else from the
# local clone. Only what upstream lacks is patched into the copy:
#   - Bearer device tokens on every route, CSRF-exempt as on the real server;
#   - POST /v1/auth/device (email + password -> device token);
#   - POST /v1/session/password, and the unified timeline (if missing).
# Port: MOCK_PORT (default 8791). Sign-in: owner@example.test / dev-password-123
# (or the device key "dev").
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
ios="$(cd "$here/.." && pwd)"
repo="$(cd "$ios/.." && pwd)"
dir="$ios/.mock"
port="${MOCK_PORT:-8791}"
pidfile="$dir/mock.pid"
modules=(server.mjs shopping-v2.mjs)
mkdir -p "$dir"

fetch() {
    if [[ -n "${MOCK_SOURCE:-}" ]]; then
        cp "$MOCK_SOURCE"/*.mjs "$dir/"
    elif ssh -o ConnectTimeout=5 nezopt true 2>/dev/null; then
        for m in "${modules[@]}"; do
            scp -q "nezopt:/home/nez/Projects/SecretaryIOS/web/mock/$m" "$dir/$m" 2>/dev/null || true
        done
    elif [[ -f "$repo/web/mock/server.mjs" ]]; then
        cp "$repo"/web/mock/*.mjs "$dir/"
    else
        echo "error: no mock source (set MOCK_SOURCE=/path/to/web/mock)" >&2
        exit 1
    fi
    [[ -f "$dir/server.mjs" ]] || { echo "error: server.mjs missing" >&2; exit 1; }
}

# insert <file> <anchor-regex> <snippet-file> [before|after]
insert() {
    SNIPPET="$3" ANCHOR="$2" WHERE="${4:-before}" perl -0pi -e '
        open(my $fh, "<", $ENV{SNIPPET}) or die "snippet: $!";
        local $/; my $snippet = <$fh>;
        my $anchor = $ENV{ANCHOR};
        if ($ENV{WHERE} eq "after") { s/($anchor)/$1\n$snippet/ or die "anchor not found: $anchor\n"; }
        else { s/(\n\s*$anchor)/\n$snippet$1/ or die "anchor not found: $anchor\n"; }
    ' "$1"
}

patch() {
    local src="$dir/server.mjs" out="$dir/server.ios.mjs"
    cp "$dir/server.mjs" "$out"
    local routes='if \(path === "\/v1\/health" && method === "GET"\) \{'
    local state='const sessions = new Map\(\);'

    if ! grep -q 'req.headers.authorization' "$src"; then
        insert "$out" "$state" "$here/mock-overlay-state-auth.js" after
        perl -0pi -e '
            s/(function currentSession\(req\) \{\n)/$1  const auth = String(req.headers.authorization ?? "");\n  if (auth) {\n    if (!auth.startsWith("Bearer ")) return null;\n    const token = auth.slice(7);\n    if (token === ACCESS_KEY) return { deviceName: "Secretary iPhone (mock)" };\n    return iosTokens.get(token) || null;\n  }\n/ or die "anchor currentSession not found\n";
            s/req\.headers\["x-secretary-csrf"\] !== "1"\)/req.headers["x-secretary-csrf"] !== "1" \&\& !req.headers.authorization)/ or die "anchor csrf not found\n";
            s/!\(method === "POST" \&\& path === "\/v1\/session"\)/!(method === "POST" \&\& (path === "\/v1\/session" || path === "\/v1\/auth\/device"))/ or die "anchor csrf-exempt not found\n";
        ' "$out"
    fi
    if ! grep -q '"/v1/auth/device"' "$src"; then
        insert "$out" "$routes" "$here/mock-overlay-auth.js"
    fi
    if ! grep -q '"/v1/session/password"' "$src"; then
        insert "$out" "$routes" "$here/mock-overlay-password.js"
    fi
    if ! grep -q '"/v1/device/events"' "$src"; then
        insert "$out" "$routes" "$here/mock-overlay-device.js"
    fi
    if ! grep -q '"/v1/timeline"' "$src"; then
        insert "$out" "$state" "$here/mock-overlay-state-timeline.js" after
        insert "$out" "$routes" "$here/mock-overlay-timeline.js"
    fi
    node --check "$out"
}

stop() {
    if [[ -f "$pidfile" ]] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
        kill "$(cat "$pidfile")" && sleep 0.3
    fi
    rm -f "$pidfile"
    # Whatever still listens on the port (an earlier run) goes too.
    local stale
    stale="$(lsof -nP -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null || true)"
    if [[ -n "$stale" ]]; then kill $stale 2>/dev/null || true; sleep 0.3; fi
}

start() {
    stop
    fetch
    patch
    MOCK_PORT="$port" nohup node "$dir/server.ios.mjs" > "$dir/mock.log" 2>&1 &
    echo $! > "$pidfile"
    for _ in $(seq 1 50); do
        if curl -sf "http://127.0.0.1:$port/v1/health" >/dev/null; then
            echo "mock running on http://localhost:$port (pid $(cat "$pidfile"))"
            return 0
        fi
        sleep 0.1
    done
    echo "error: mock did not start; see $dir/mock.log" >&2
    exit 1
}

case "${1:-start}" in
    start|restart) start ;;
    stop) stop ;;
    status) curl -sf "http://127.0.0.1:$port/v1/health" && echo ;;
    *) echo "usage: $0 start|stop|restart|status" >&2; exit 2 ;;
esac
