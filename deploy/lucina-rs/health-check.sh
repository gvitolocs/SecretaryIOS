#!/bin/sh
# Watchdog for lucina-rs (installed as ~/.local/bin/lucina-rs-health-check, run by
# lucina-rs-health.timer every 2 minutes).
# - The unit is failed (crash loop hit StartLimitBurst): reset it and start it again.
# - /health fails twice in a row (it answers 503 only when a background loop died:
#   Telegram poller, reminder scheduler, Discord, Bot Hospital, delivery monitor):
#   restart. One failure is only remembered, so a slow answer during a busy turn
#   never restarts a healthy process.
# - The unit is inactive (stopped on purpose): do nothing.
unit=lucina-rs.service
state="${XDG_RUNTIME_DIR:-/tmp}/lucina-rs-health.failures"
if systemctl --user is-failed --quiet "$unit"; then
    echo "lucina-rs: unit failed, resetting and starting" >&2
    systemctl --user reset-failed "$unit"
    systemctl --user start "$unit"
    rm -f "$state"
    exit 0
fi
systemctl --user is-active --quiet "$unit" || exit 0
if curl -fsS -m 10 -o /dev/null http://127.0.0.1:8790/health; then
    rm -f "$state"
    exit 0
fi
n=$(( $(cat "$state" 2>/dev/null || echo 0) + 1 ))
if [ "$n" -ge 2 ]; then
    echo "lucina-rs: /health failed twice in a row, restarting" >&2
    rm -f "$state"
    systemctl --user restart "$unit"
else
    echo "$n" > "$state"
    echo "lucina-rs: /health failed once, checking again next run" >&2
fi
