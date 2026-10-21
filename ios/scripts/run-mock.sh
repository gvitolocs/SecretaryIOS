#!/usr/bin/env bash
# Runs the web app's in-memory mock API for the iOS app, with an iOS overlay.
#
#   ios/scripts/run-mock.sh start|stop|restart|status
#
# The mock (web/mock/server.mjs) is copied into ios/.mock/ (gitignored): from the
# local clone when it has one, else over SSH from nezopt (it may be uncommitted).
# scripts/mock-overlay.js is inserted into the copy so it speaks Bearer device tokens
# and POST /v1/auth/device like the real server. Port: MOCK_PORT (default 8791).
# Mock sign-in: owner@example.test / dev-password-123 (legacy access key: "dev").
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
ios="$(cd "$here/.." && pwd)"
repo="$(cd "$ios/.." && pwd)"
dir="$ios/.mock"
port="${MOCK_PORT:-8791}"
pidfile="$dir/mock.pid"
mkdir -p "$dir"

fetch() {
    if [[ -n "${MOCK_SOURCE:-}" ]]; then
        cp "$MOCK_SOURCE" "$dir/server.mjs"
    elif ssh -o ConnectTimeout=5 nezopt true 2>/dev/null; then
        scp -q nezopt:/home/nez/Projects/SecretaryIOS/web/mock/server.mjs "$dir/server.mjs"
    elif [[ -f "$repo/web/mock/server.mjs" ]]; then
        cp "$repo/web/mock/server.mjs" "$dir/server.mjs"
    else
        echo "error: no mock source (set MOCK_SOURCE=/path/to/server.mjs)" >&2
        exit 1
    fi
}

patch() {
    local src="$dir/server.mjs" out="$dir/server.ios.mjs"
    cp "$src" "$out"
    # Upstream may grow these features; patch only what is missing.
    if ! grep -q '/v1/auth/device' "$out"; then
        OVERLAY="$here/mock-overlay.js" perl -0pi -e '
            open(my $fh, "<", $ENV{OVERLAY}) or die "overlay: $!";
            local $/; my $overlay = <$fh>;
            s/(\n  if \(path === "\/v1\/health" && method === "GET"\) \{)/\n$overlay$1/ or die "anchor /v1/health not found\n";
        ' "$out"
        perl -0pi -e '
            s/(const sessions = new Map\(\);)/$1\nconst IOS_EMAIL = "owner\@example.test";\nlet iosPassword = process.env.MOCK_PASSWORD ?? "dev-password-123";\n\/** \@type {Map<string, {deviceName: string, deviceId: string}>} *\/\nconst iosTokens = new Map();\nconst iosLock = new Map();/ or die "anchor sessions not found\n";
        ' "$out"
    fi
    if ! grep -q 'req.headers.authorization' "$out"; then
        perl -0pi -e '
            s/(function currentSession\(req\) \{\n)/$1  const auth = String(req.headers.authorization ?? "");\n  if (auth) {\n    if (!auth.startsWith("Bearer ")) return null;\n    const token = auth.slice(7);\n    if (token === ACCESS_KEY) return { deviceName: "Secretary iPhone (mock)" };\n    return (typeof iosTokens !== "undefined" \&\& iosTokens.get(token)) || null;\n  }\n/ or die "anchor currentSession not found\n";
            s/req\.headers\["x-secretary-csrf"\] !== "1"\)/req.headers["x-secretary-csrf"] !== "1" \&\& !req.headers.authorization)/ or die "anchor csrf not found\n";
            s/!\(method === "POST" \&\& path === "\/v1\/session"\)/!(method === "POST" \&\& (path === "\/v1\/session" || path === "\/v1\/auth\/device"))/ or die "anchor csrf-exempt not found\n";
        ' "$out"
    fi
    node --check "$out"
}

stop() {
    if [[ -f "$pidfile" ]] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
        kill "$(cat "$pidfile")" && sleep 0.3
    fi
    rm -f "$pidfile"
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
