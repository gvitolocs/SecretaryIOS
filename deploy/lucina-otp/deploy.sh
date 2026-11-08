#!/usr/bin/env bash
# Replace `lucina-device otp-server` with the Rust lucina-otp-relay on p1.
#   deploy/lucina-otp/deploy.sh --dry-run   # read-only checks, changes nothing
#   deploy/lucina-otp/deploy.sh             # deploy; rolls back by itself on failure
# Runs from the Mac or nezopt. Build first, on nezopt:
#   CARGO_TARGET_DIR=$HOME/.cache/cargo-target/lucina-android-relay \
#     cargo build --release -p lucina-otp-relay --target x86_64-unknown-linux-musl
#
# What it does on p1 (sudo -n is allowed for install and systemctl only):
#   1. checks: the unit's ExecStart is the old relay (or already the new one), the live unit
#      matches deploy/lucina-otp/lucina-otp.service apart from ExecStart (FORCE_UNIT=1 skips),
#      /usr/local/bin/lucina-device exists (the rollback target, never touched);
#   2. copies the binary and the unit to /tmp, checks the binary's sha256;
#   3. backs up the unit (and an earlier lucina-otp-relay) as *.bak-<timestamp>;
#   4. installs /usr/local/bin/lucina-otp-relay and the unit (ExecStart=/usr/local/bin/lucina-otp-relay),
#      daemon-reload, restart; drop-ins (continuous.conf) stay as they are;
#   5. expects the unit active with the new ExecStart, and 401 from an unauthenticated
#      GET /status both on 127.0.0.1:18866 and through Caddy (https://getsecretary.tech/v1/phone/otp/status);
#   6. on any failure: restores the unit backup, daemon-reload, restart, and checks again.
# Never prints tokens; never reads the state directory.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
P1=${P1_HOST:-p1}
NEZ=${NEZOPT_HOST:-}
if [ -z "$NEZ" ]; then NEZ=nezopt; nc -z -G 2 nezopt.fritz.box 22 >/dev/null 2>&1 || NEZ=nezopt-remote; fi
# The musl binary on nezopt (default) or a local path in BIN.
NEZ_BIN=${NEZ_BIN:-/home/nez/.cache/cargo-target/lucina-android-relay/x86_64-unknown-linux-musl/release/lucina-otp-relay}
BIN=${BIN:-}
UNIT_SRC=$HERE/lucina-otp.service
DRY=0
case "${1:-}" in
  --dry-run) DRY=1 ;;
  "") ;;
  *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
esac
say() { printf '[lucina-otp] %s\n' "$*"; }

if [ -z "$BIN" ] && [ -x "$NEZ_BIN" ]; then BIN=$NEZ_BIN; fi # running on nezopt
if [ -n "$BIN" ]; then
  [ -x "$BIN" ] || { say "binary $BIN missing"; exit 1; }
  SUM=$(sha256sum "$BIN" | cut -d' ' -f1)
  KIND=$(file -b "$BIN")
else
  # shellcheck disable=SC2029 # expands here on purpose
  SUM=$(ssh "$NEZ" "sha256sum '$NEZ_BIN'" | cut -d' ' -f1) || { say "binary missing on $NEZ: $NEZ_BIN"; exit 1; }
  # shellcheck disable=SC2029
  KIND=$(ssh "$NEZ" "file -b '$NEZ_BIN'")
fi
case "$KIND" in
  *x86-64*static*) ;;
  *) say "not a static x86-64 binary: $KIND"; exit 1 ;;
esac
say "binary sha256 $SUM"

# Runs on p1. $1 = mode (check|deploy), $2 = expected sha256, $3 = FORCE_UNIT, $4 = the unit, base64.
REMOTE=$(cat <<'REMOTE_EOF'
set -euo pipefail
mode=$1 sum=$2 force=$3
new_unit=$(printf "%s" "$4" | base64 -d)
svc=lucina-otp.service
new_bin=/usr/local/bin/lucina-otp-relay
old_bin=/usr/local/bin/lucina-device
say() { printf '[p1] %s\n' "$*"; }
sudo -n true || { say "sudo -n is not available"; exit 1; }
unit=$(systemctl show -p FragmentPath --value "$svc")
if [ -z "$unit" ] || [ ! -f "$unit" ]; then say "$svc has no unit file"; exit 1; fi
exec_now=$(sed -n 's/^ExecStart=//p' "$unit")
say "unit $unit, ExecStart=$exec_now, active=$(systemctl is-active "$svc" || true)"
say "drop-ins: $(systemctl show -p DropInPaths --value "$svc")"
case "$exec_now" in
  "$old_bin otp-server"|"$new_bin") ;;
  *) say "unexpected ExecStart, not touching it"; exit 1 ;;
esac
[ -x "$old_bin" ] || { say "rollback binary $old_bin missing"; exit 1; }
if [ "$force" != 1 ] && ! diff <(grep -v '^ExecStart=' "$unit") <(printf '%s\n' "$new_unit" | grep -v '^ExecStart=') >/dev/null; then
  say "live unit differs from deploy/lucina-otp/lucina-otp.service beyond ExecStart (< live, > new):"
  diff <(grep -v '^ExecStart=' "$unit") <(printf '%s\n' "$new_unit" | grep -v '^ExecStart=') || true
  say "set FORCE_UNIT=1 to install it anyway"
  exit 1
fi
codes() {
  local l p
  l=$(curl -s -m 5 -o /dev/null -w '%{http_code}' http://127.0.0.1:18866/status || true)
  p=$(curl -s -m 5 -o /dev/null -w '%{http_code}' --resolve getsecretary.tech:443:127.0.0.1 \
        https://getsecretary.tech/v1/phone/otp/status || true)
  echo "$l $p"
}
healthy() {
  local _
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    systemctl is-active --quiet "$svc" && [ "$(codes)" = "401 401" ] && return 0
  done
  say "unhealthy: active=$(systemctl is-active "$svc" || true), status codes local/caddy=$(codes)"
  return 1
}
if [ "$mode" = check ]; then
  say "unauthenticated /status local/caddy: $(codes) (expected 401 401)"
  say "checks passed"
  exit 0
fi
[ "$(sha256sum /tmp/lucina-otp-relay.new | cut -d' ' -f1)" = "$sum" ] || { say "binary checksum mismatch"; exit 1; }
ts=$(date +%Y%m%d%H%M%S)
unit_bak=$unit.bak-$ts
sudo -n install -m 0644 "$unit" "$unit_bak"
bin_bak=
if [ -e "$new_bin" ]; then bin_bak=$new_bin.bak-$ts; sudo -n install -m 0755 "$new_bin" "$bin_bak"; fi
say "backups: $unit_bak ${bin_bak:-(no earlier $new_bin)}"
rollback() {
  say "ROLLING BACK"
  sudo -n install -m 0644 "$unit_bak" "$unit"
  if [ -n "$bin_bak" ]; then sudo -n install -m 0755 "$bin_bak" "$new_bin"; fi
  sudo -n systemctl daemon-reload
  sudo -n systemctl restart "$svc" || true
  if healthy; then say "rolled back to: $(sed -n 's/^ExecStart=//p' "$unit")"; else say "ROLLBACK UNHEALTHY, check $svc by hand"; fi
  exit 1
}
trap rollback ERR
sudo -n install -m 0755 /tmp/lucina-otp-relay.new "$new_bin"
tmp_unit=$(mktemp)
printf "%s\n" "$new_unit" > "$tmp_unit"
sudo -n install -m 0644 "$tmp_unit" "$unit"
rm -f "$tmp_unit"
sudo -n systemctl daemon-reload
systemctl show -p ExecStart --value "$svc" | grep -q "path=$new_bin "
sudo -n systemctl restart "$svc"
healthy
trap - ERR
rm -f /tmp/lucina-otp-relay.new
say "deployed: $(systemctl show -p ExecStart --value "$svc" | sed 's/ ;.*//'), status codes local/caddy=$(codes)"
say "rollback by hand: sudo install -m 0644 $unit_bak $unit && sudo systemctl daemon-reload && sudo systemctl restart $svc"
REMOTE_EOF
)

UNIT_B64=$(base64 < "$UNIT_SRC" | tr -d "\n")
if [ "$DRY" = 1 ]; then
  ssh "$P1" bash -s -- check "$SUM" "${FORCE_UNIT:-0}" "$UNIT_B64" <<<"$REMOTE"
  say "dry run done; nothing changed"
  exit 0
fi
ssh "$P1" bash -s -- check "$SUM" "${FORCE_UNIT:-0}" "$UNIT_B64" <<<"$REMOTE"
say "copying the binary to $P1:/tmp"
if [ -n "$BIN" ]; then
  scp -q "$BIN" "$P1:/tmp/lucina-otp-relay.new"
else
  scp -q -3 "$NEZ:$NEZ_BIN" "$P1:/tmp/lucina-otp-relay.new"
fi
ssh "$P1" bash -s -- deploy "$SUM" "${FORCE_UNIT:-0}" "$UNIT_B64" <<<"$REMOTE"
say "done"
