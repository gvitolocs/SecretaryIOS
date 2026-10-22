#!/usr/bin/env bash
# Phase-1 parity harness (port plan §4): a Node reference Lucina and a Rust
# Lucina, each on its own COPY of the live data and its own throwaway
# grocery database, both talking to fake DeepSeek and a fake secretary-server,
# get the same /api/secretary requests (tools/parity/http-cases.json).
#
# Nothing here writes live data: the live data dir and the grocery database
# are only read (rsync, pg_dump). The Node reference container gets every bot
# token set empty, the Telegram/Discord/DeepSeek hosts pinned to 127.0.0.1,
# Harness and Bot Hospital off, and a preflight aborts if its config still
# sees a token. The live container lucina-nez is never touched.
#
# Usage (on nezopt): tools/parity/run-harness.sh            # build release, run, clean up
#                    KEEP=1 tools/parity/run-harness.sh     # keep the work dir for inspection
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
LUCINA=${LUCINA:-/home/nez/Projects/Lucina}
BIN=${BIN:-$REPO/target/release/lucina}
WORK=${WORK:-$(mktemp -d /tmp/lucina-parity.XXXXXX)}
NODE_PORT=8791 RUST_PORT=8792 FWD_PORT=8793 DS_NODE=8799 DS_RUST=8797 SEC_PORT=8798 PG_PORT=55791
NODE_CT=lucina-parity-node PG_CT=lucina-parity-pg
mkdir -p "$WORK"; chmod 700 "$WORK"
PIDS=()

log() { printf '[parity] %s\n' "$*"; }
cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  docker rm -f "$NODE_CT" "$PG_CT" >/dev/null 2>&1 || true
  if [ "${KEEP:-0}" = 1 ]; then log "kept $WORK"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT

for p in $NODE_PORT $RUST_PORT $FWD_PORT $DS_NODE $DS_RUST $SEC_PORT $PG_PORT; do
  if ss -ltn "sport = :$p" | grep -q LISTEN; then log "port $p is busy"; exit 2; fi
done

[ -x "$BIN" ] || (cd "$REPO" && cargo build --release -p lucina-server)

# 1. Data copies (read only from the live tree).
for side in node rust fwd; do
  mkdir -p "$WORK/$side-data"
  for f in timeline.jsonl reminders.json recent-telegram-chat.json telegram-chats.json planner-provider-blocks.json shopping-catalog.v1.json; do
    [ -f "$LUCINA/data/$f" ] && cp "$LUCINA/data/$f" "$WORK/$side-data/$f"
  done
  [ -d "$LUCINA/data/presence" ] && cp -r "$LUCINA/data/presence" "$WORK/$side-data/presence"
done
mkdir -p "$WORK/locks"

# 2. Tokens (random; never the live ones).
rand() { head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n'; }
echo "par_$(rand)" > "$WORK/node-api.token"
echo "par_$(rand)" > "$WORK/rust-api.token"
echo "svc_$(rand)" > "$WORK/node-svc.token"
echo "svc_$(rand)" > "$WORK/rust-svc.token"
echo "fake-deepseek-key" > "$WORK/deepseek.key"
chmod 600 "$WORK"/*.token "$WORK/deepseek.key"

# 3. The Node reference env: the live container's environment, unchanged
#    except for the overrides below (values never printed).
"$BIN" env-from-node --container lucina-nez --format docker --out "$WORK/node.env" >/dev/null
grep '^GROCERY_DATABASE_URL=' "$WORK/node.env" > "$WORK/pg-src.env" || { log "no GROCERY_DATABASE_URL in the live env"; exit 2; }

# 4. Throwaway Postgres with two copies of hermes_grocery.
PGPASS=$(rand)
docker run -d --name "$PG_CT" -p 127.0.0.1:$PG_PORT:5432 -e POSTGRES_PASSWORD="$PGPASS" postgres:17-alpine >/dev/null
docker run --rm --network host --env-file "$WORK/pg-src.env" -v "$WORK:/w" postgres:17-alpine \
  sh -c 'pg_dump "$GROCERY_DATABASE_URL" --no-owner --no-acl -f /w/grocery.sql'
for i in $(seq 1 30); do docker exec "$PG_CT" pg_isready -U postgres >/dev/null 2>&1 && break; sleep 1; done
for db in grocery_node grocery_rust grocery_fwd; do
  docker exec "$PG_CT" createdb -U postgres "$db"
  docker exec -i "$PG_CT" psql -q -U postgres -d "$db" -v ON_ERROR_STOP=0 < "$WORK/grocery.sql" >/dev/null 2>&1
  if ! docker exec "$PG_CT" psql -tA -U postgres -d "$db" -c "select to_regclass('grocery.shopping_lists')" | grep -q shopping_lists; then
    docker exec -i "$PG_CT" psql -q -U postgres -d "$db" -v ON_ERROR_STOP=1 < "$LUCINA/migrations/002_shopping_v2.sql" >/dev/null
    log "$db: applied 002_shopping_v2 (not in the dump)"
  fi
done
PGURL() { echo "postgres://postgres:$PGPASS@127.0.0.1:$PG_PORT/$1"; }
log "grocery copies ready ($(docker exec "$PG_CT" psql -tA -U postgres -d grocery_node -c 'select count(*) from grocery.shopping_list_items') shopping rows)"

# 5. Fakes.
"$BIN" fake-deepseek --bind 127.0.0.1:$DS_NODE --script "$REPO/tools/parity/deepseek-script.json" --record "$WORK/ds-node.jsonl" >/dev/null 2>&1 & PIDS+=($!)
"$BIN" fake-deepseek --bind 127.0.0.1:$DS_RUST --script "$REPO/tools/parity/deepseek-script.json" --record "$WORK/ds-rust.jsonl" >/dev/null 2>&1 & PIDS+=($!)
"$BIN" fake-secretary --bind 127.0.0.1:$SEC_PORT --record "$WORK/secretary.jsonl" >/dev/null 2>&1 & PIDS+=($!)
touch "$WORK/ds-node.jsonl" "$WORK/ds-rust.jsonl" "$WORK/secretary.jsonl"

# 6. The Node reference container.
NODE_ARGS=(
  --network host --init
  --add-host api.telegram.org:127.0.0.1 --add-host discord.com:127.0.0.1 --add-host gateway.discord.gg:127.0.0.1
  --add-host api.deepseek.com:127.0.0.1 --add-host generativelanguage.googleapis.com:127.0.0.1
  --env-file "$WORK/node.env"
  -e HERMES_PORT=$NODE_PORT -e HERMES_PUBLIC_URL= -e HERMES_GATEWAY_LOCK_DIR=/w/locks -e HERMES_GATEWAY_INSTANCE=lucina-parity
  -e TELEGRAM_BOT_TOKEN= -e TELEGRAM_BOT_TOKEN_LUCINA= -e TELEGRAM_BOT_TOKEN_POKO= -e TELEGRAM_BOT_TOKEN_CITYHOP=
  -e DISCORD_BOT_TOKEN= -e DISCORD_BOT_TOKEN_LUCINA= -e DISCORD_BOT_TOKEN_POKO=
  -e DEEPSEEK_BASE_URL=http://127.0.0.1:$DS_NODE/v1 -e DEEPSEEK_API_KEY=fake-deepseek-key -e DEEPSEEK_KEY_FILE=/w/deepseek.key
  -e MEMORY_PROVIDER=none -e HONCHO_MIRROR_WRITES=false
  -e GROCERY_DATABASE_URL="$(PGURL grocery_node)"
  -e SECRETARY_API_ENABLED=true -e SECRETARY_API_TOKEN="$(cat "$WORK/node-api.token")"
  -e LUCINA_SECRETARY_SERVER_URL=http://127.0.0.1:$SEC_PORT -e LUCINA_SECRETARY_SERVICE_TOKEN_FILE=/w/node-svc.token
  -e LUCINA_AVATAR_OUTFIT=true -e LUCINA_NOTES=true -e LUCINA_CALENDAR=true -e LUCINA_PRESENCE_CONTEXT=true
  -e LUCINA_APP_NOTIFY=false -e LUCINA_PRESENCE_DIR=/app/data/presence
  -e DSH_REMOTE_ENABLED=false -e HERMES_HARNESS_ENABLED=false -e HERMES_CURSOR_ENABLED=false
  -e BOT_HOSPITAL_ENABLED=false -e BOT_HOSPITAL_DAILY_AUDIT_ENABLED=false -e POKO_NEWS_AUTOPOST_ENABLED=false
  -e LUCINA_PROACTIVE_CHECKIN_ENABLED=false -e ALEXA_LUCINA_ENABLED=false
  -v "$WORK/node-data:/app/data" -v "$LUCINA/src:/app/src:ro" -v "$WORK:/w"
)
PREFLIGHT='const {config} = await import("/app/src/config.js");
const t = [config.telegram?.token, config.telegram?.lucinaToken, config.telegram?.pokoToken, config.telegram?.cityhopToken, config.discord?.token, config.discord?.lucinaToken, config.discord?.pokoToken];
const bad = t.some(Boolean) || config.botHospital?.enabled || config.dshRemote?.enabled || config.harness?.enabled || config.memory?.provider === "honcho";
console.log("PREFLIGHT:" + (bad ? "unsafe" : "safe"));'
if ! docker run --rm "${NODE_ARGS[@]}" lucina:nez node --input-type=module -e "$PREFLIGHT" 2>/dev/null | grep -q 'PREFLIGHT:safe'; then
  log "preflight: the reference config still has a bot token, Harness, Bot Hospital or Honcho on; not starting it"
  exit 3
fi
log "preflight: no bot tokens, Harness, Bot Hospital or Honcho in the reference"
docker run -d --name "$NODE_CT" "${NODE_ARGS[@]}" lucina:nez node src/index.js >/dev/null

# 7. The Rust instances: the sandbox under test (local reminder writer on its
#    copy), and a forwarder that sends reminder writes to the Node reference.
rust_env() { # $1 = side, $2 = port, $3 = db
  echo "LUCINA_BIND=127.0.0.1:$2"
  echo "LUCINA_DATA_DIR=$WORK/$1-data"
  echo "SECRETARY_API_ENABLED=true"
  echo "SECRETARY_API_TOKEN_FILE=$WORK/rust-api.token"
  echo "DEEPSEEK_BASE_URL=http://127.0.0.1:$DS_RUST/v1"
  echo "DEEPSEEK_API_KEY=fake-deepseek-key"
  echo "LUCINA_MEMORY_MODE=off"
  echo "GROCERY_DATABASE_URL=$(PGURL "$3")"
  echo "LUCINA_SECRETARY_SERVER_URL=http://127.0.0.1:$SEC_PORT"
  echo "LUCINA_SECRETARY_SERVICE_TOKEN_FILE=$WORK/rust-svc.token"
  echo "LUCINA_AVATAR_OUTFIT=true"; echo "LUCINA_NOTES=true"; echo "LUCINA_CALENDAR=true"; echo "LUCINA_PRESENCE_CONTEXT=true"
  echo "LUCINA_PRESENCE_DIR=$WORK/$1-data/presence"
  echo "LUCINA_FAST_CHAT_ENABLED=true"
  echo "LUCINA_JOBS=false"
  echo "LUCINA_SHOPPING_CATALOG_PATH=$WORK/$1-data/shopping-catalog.v1.json"
  echo "LUCINA_RECOVERY_COMPANION_PATH=$LUCINA/docs/lucina-recovery-companion.md"
  echo "LUCINA_OPERATOR_BRIEF_PATH=$LUCINA/docs/lucina-operator-brief.md"
  grep -E '^(TELEGRAM_ALLOWED_CHAT_IDS|LUCINA_OPERATOR_CHAT_ID|LUCINA_OWNER_USER_PEER_ID|LUCINA_REMINDER_TIMEZONE|LUCINA_TIMEZONE|LUCINA_RUNTIME_HOST)=' "$WORK/node.env" || true
}
rust_env rust $RUST_PORT grocery_rust > "$WORK/rust.env"; echo "LUCINA_REMINDERS_WRITER=local" >> "$WORK/rust.env"
rust_env fwd $FWD_PORT grocery_fwd > "$WORK/fwd.env"
{ echo "LUCINA_REMINDERS_WRITER=node"; echo "LUCINA_NODE_URL=http://127.0.0.1:$NODE_PORT"; echo "LUCINA_NODE_TOKEN_FILE=$WORK/node-api.token"; } >> "$WORK/fwd.env"
chmod 600 "$WORK"/*.env
(set -a; . "$WORK/rust.env"; set +a; RUST_LOG=warn exec "$BIN" serve) > "$WORK/rust.log" 2>&1 & PIDS+=($!)
(set -a; . "$WORK/fwd.env"; set +a; RUST_LOG=warn exec "$BIN" serve) > "$WORK/fwd.log" 2>&1 & PIDS+=($!)

wait_up() { for i in $(seq 1 60); do curl -fsS -o /dev/null "http://127.0.0.1:$1/health" 2>/dev/null && return 0; sleep 1; done; log "port $1 did not come up"; return 1; }
wait_up $NODE_PORT; wait_up $RUST_PORT; wait_up $FWD_PORT
log "node reference :$NODE_PORT, rust :$RUST_PORT, rust forwarder :$FWD_PORT"

# 8. Variables from the copy (same in both): a delivered owner reminder and an older timeline id.
SENT=$(node -e '
const r = require(process.argv[1]); const owner = process.argv[2];
const s = (r.reminders || []).find((x) => x.status === "sent" && (!owner || String(x.chatId) === owner));
process.stdout.write(s ? s.id : "none");' "$WORK/node-data/reminders.json" "$(sed -n 's/^TELEGRAM_ALLOWED_CHAT_IDS=\([^,]*\).*/\1/p' "$WORK/node.env" | head -1)")
OLD=$(tail -n 12 "$WORK/node-data/timeline.jsonl" | head -n 1 | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.parse(s).id))')
BIG=$(head -c 70000 /dev/zero | tr '\0' a)

# 9. Run.
set +e
"$BIN" parity --cases "$REPO/tools/parity/http-cases.json" \
  --node-url http://127.0.0.1:$NODE_PORT --rust-url http://127.0.0.1:$RUST_PORT \
  --node-token-file "$WORK/node-api.token" --rust-token-file "$WORK/rust-api.token" \
  --node-deepseek-record "$WORK/ds-node.jsonl" --rust-deepseek-record "$WORK/ds-rust.jsonl" \
  --secretary-record "$WORK/secretary.jsonl" \
  --node-service-token-file "$WORK/node-svc.token" --rust-service-token-file "$WORK/rust-svc.token" \
  --node-timeline "$WORK/node-data/timeline.jsonl" --rust-timeline "$WORK/rust-data/timeline.jsonl" \
  --var "sentReminderId=$SENT" --var "oldTimelineId=$OLD" --var "bigText=$BIG" \
  --out "$WORK/report.json"
STATUS=$?

# 10. Reminder forwarding (phase 1): writes through the Rust forwarder land in the Node reference's store.
FWD_TOKEN=$(cat "$WORK/rust-api.token"); NODE_TOKEN=$(cat "$WORK/node-api.token")
FWD=$(curl -sS -X POST "http://127.0.0.1:$FWD_PORT/api/secretary/reminders" -H "authorization: Bearer $FWD_TOKEN" -H 'content-type: application/json' \
  -d '{"text":"parity forward check","dueAt":"2027-06-01T10:00:00+02:00"}')
FID=$(printf '%s' "$FWD" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(JSON.parse(s).data.reminder.id)}catch{process.stdout.write("")}})')
SEEN=$(curl -sS "http://127.0.0.1:$NODE_PORT/api/secretary/reminders?scope=all" -H "authorization: Bearer $NODE_TOKEN" | grep -c "$FID" || true)
CANCEL=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "http://127.0.0.1:$FWD_PORT/api/secretary/reminders/$FID" -H "authorization: Bearer $FWD_TOKEN")
STILL=$(curl -sS "http://127.0.0.1:$NODE_PORT/api/secretary/reminders?scope=all" -H "authorization: Bearer $NODE_TOKEN" | grep -c "$FID" || true)
if [ -n "$FID" ] && [ "$SEEN" = 1 ] && [ "$CANCEL" = 200 ] && [ "$STILL" = 0 ]; then
  log "forwarding: create and cancel through Rust reached the Node store (ok)"
else
  log "forwarding: FAILED (id=${FID:-none} seen=$SEEN cancel=$CANCEL still=$STILL)"; STATUS=1
fi

# 11. Shared timeline writers: Node (reference) and Rust append to ONE copy at once.
SHARED="$WORK/shared-data"; mkdir -p "$SHARED"; cp "$WORK/node-data/timeline.jsonl" "$SHARED/timeline.jsonl"
log "shared-writer check: 300 Node + 300 Rust appends to one file at the same time"
# No network at all for this one-off Node process.
docker run --rm --network none --env-file "$WORK/node.env" -e TELEGRAM_BOT_TOKEN= -e TELEGRAM_BOT_TOKEN_LUCINA= \
  -v "$LUCINA/src:/app/src:ro" -v "$SHARED:/s" lucina:nez node --input-type=module -e '
const t = await import("/app/src/timeline.js");
const store = t.createTimelineStore({ filePath: "/s/timeline.jsonl" });
for (let i = 0; i < 300; i++) await store.append({ channel: "telegram", role: "user", text: `node line ${i}` });
' >/dev/null 2>&1 & NODEW=$!
"$BIN" timeline-append-test --path "$SHARED/timeline.jsonl" --count 300 >/dev/null 2>&1 & RUSTW=$!
wait $NODEW; wait $RUSTW
"$BIN" check-timeline --path "$SHARED/timeline.jsonl" --expect-node 300 --expect-rust 300 || STATUS=1

log "report: $WORK/report.json"
[ "$STATUS" = 0 ] && log "PARITY GREEN" || log "PARITY: differences (see above)"
exit $STATUS
