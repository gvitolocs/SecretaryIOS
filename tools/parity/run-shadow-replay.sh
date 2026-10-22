#!/usr/bin/env bash
# Shadow replay: the last N real app/web chat turns from the LIVE timeline
# (read only) replayed against a Rust Lucina sandbox, compared with what the
# Node Lucina answered at the time.
#
# Shadow semantics: real DeepSeek, Honcho read only, the owner's real notes and
# calendar READ through secretary-server; every write lands in a copy (data
# dir, grocery database, reminders) or in a local stand-in (note saves,
# calendar creates, outfit). Harness jobs are off. Nothing live is written.
#
# Usage (on nezopt): tools/parity/run-shadow-replay.sh [N]      (default 40)
#                    SHOW_TEXT=1 ... prints messages and both replies
set -euo pipefail

N=${1:-40}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
LUCINA=${LUCINA:-/home/nez/Projects/Lucina}
BIN=${BIN:-$REPO/target/release/lucina}
WORK=${WORK:-$(mktemp -d /tmp/lucina-replay.XXXXXX)}
PORT=8794 SEC_PORT=8796 PG_PORT=55792 PG_CT=lucina-replay-pg
mkdir -p "$WORK"; chmod 700 "$WORK"
PIDS=()
log() { printf '[replay] %s\n' "$*"; }
cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  docker rm -f "$PG_CT" >/dev/null 2>&1 || true
  if [ "${KEEP:-0}" = 1 ]; then log "kept $WORK"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT
for p in $PORT $SEC_PORT $PG_PORT; do
  if ss -ltn "sport = :$p" | grep -q LISTEN; then log "port $p is busy"; exit 2; fi
done

# Data copy (the live files are only read).
D="$WORK/data"; mkdir -p "$D"
for f in timeline.jsonl reminders.json telegram-chats.json shopping-catalog.v1.json; do
  [ -f "$LUCINA/data/$f" ] && cp "$LUCINA/data/$f" "$D/$f"
done
[ -d "$LUCINA/data/presence" ] && cp -r "$LUCINA/data/presence" "$D/presence"

# The live settings (values never printed), then sandbox overrides.
"$BIN" env-from-node --container lucina-nez --format shell --out "$WORK/node.env" >/dev/null
"$BIN" env-from-node --container lucina-nez --format docker --out "$WORK/node.docker.env" >/dev/null
grep '^GROCERY_DATABASE_URL=' "$WORK/node.docker.env" > "$WORK/pg-src.env"; rm -f "$WORK/node.docker.env"

PGPASS=$(head -c 18 /dev/urandom | od -An -tx1 | tr -d ' \n')
docker run -d --name "$PG_CT" -p 127.0.0.1:$PG_PORT:5432 -e POSTGRES_PASSWORD="$PGPASS" postgres:17-alpine >/dev/null
docker run --rm --network host --env-file "$WORK/pg-src.env" -v "$WORK:/w" postgres:17-alpine \
  sh -c 'pg_dump "$GROCERY_DATABASE_URL" --no-owner --no-acl -f /w/grocery.sql'
for i in $(seq 1 30); do docker exec "$PG_CT" pg_isready -U postgres >/dev/null 2>&1 && break; sleep 1; done
docker exec "$PG_CT" createdb -U postgres grocery_replay
docker exec -i "$PG_CT" psql -q -U postgres -d grocery_replay < "$WORK/grocery.sql" >/dev/null 2>&1
rm -f "$WORK/grocery.sql" "$WORK/pg-src.env"

head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n' | sed 's/^/rep_/' > "$WORK/api.token"
head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n' | sed 's/^/svc_/' > "$WORK/svc.token"
chmod 600 "$WORK"/*.token

# secretary-server stand-in: GETs go to the real server (the owner's notes and
# calendar, read only) with Lucina's real service token; writes stay local.
UPSTREAM=$(set -a; . "$WORK/node.env"; printf '%s' "${LUCINA_SECRETARY_SERVER_URL:-}")
UPTOKEN=$(set -a; . "$WORK/node.env"; printf '%s' "${LUCINA_SECRETARY_SERVICE_TOKEN_FILE:-}")
if [ -n "$UPSTREAM" ] && [ -r "$UPTOKEN" ]; then
  "$BIN" fake-secretary --bind 127.0.0.1:$SEC_PORT --record "$WORK/secretary.jsonl" --upstream "$UPSTREAM" --upstream-token-file "$UPTOKEN" >/dev/null 2>&1 & PIDS+=($!)
  log "secretary-server reads go to the real server (writes stay local)"
else
  "$BIN" fake-secretary --bind 127.0.0.1:$SEC_PORT --record "$WORK/secretary.jsonl" >/dev/null 2>&1 & PIDS+=($!)
  log "secretary-server: local stand-in only (no upstream configured)"
fi

cat > "$WORK/sandbox.env" <<EOF
LUCINA_BIND=127.0.0.1:$PORT
LUCINA_DATA_DIR=$D
LUCINA_TIMELINE_PATH=$D/timeline.jsonl
SECRETARY_API_ENABLED=true
SECRETARY_API_TOKEN_FILE=$WORK/api.token
LUCINA_MEMORY_MODE=readonly
LUCINA_REMINDERS_WRITER=local
LUCINA_SCHEDULER=false
LUCINA_JOBS=false
SECRETARY_JOBS_PATH=$D/secretary-jobs.json
LUCINA_RECENT_SEED_PATH=$WORK/no-seed.json
GROCERY_DATABASE_URL=postgres://postgres:$PGPASS@127.0.0.1:$PG_PORT/grocery_replay
LUCINA_SECRETARY_SERVER_URL=http://127.0.0.1:$SEC_PORT
LUCINA_SECRETARY_SERVICE_TOKEN_FILE=$WORK/svc.token
LUCINA_PRESENCE_DIR=$D/presence
LUCINA_SHOPPING_CATALOG_PATH=$D/shopping-catalog.v1.json
EOF
chmod 600 "$WORK/sandbox.env"
(set -a; . "$WORK/node.env"; . "$WORK/sandbox.env"; unset SECRETARY_API_TOKEN; set +a; RUST_LOG=warn exec "$BIN" serve) > "$WORK/rust.log" 2>&1 & PIDS+=($!)
for i in $(seq 1 60); do curl -fsS -o /dev/null "http://127.0.0.1:$PORT/health" 2>/dev/null && break; sleep 1; done
curl -fsS "http://127.0.0.1:$PORT/health" | grep -q '"memory":{"mode":"readonly"' || { log "sandbox not read-only on Honcho; stopping"; exit 3; }
log "sandbox :$PORT (real DeepSeek, Honcho read only, data copies)"

"$BIN" shadow-replay --timeline "$LUCINA/data/timeline.jsonl" --last "$N" --channels web,ios \
  --rust-url "http://127.0.0.1:$PORT" --token-file "$WORK/api.token" --out "$WORK/replay.json" \
  ${SHOW_TEXT:+--show-text}
