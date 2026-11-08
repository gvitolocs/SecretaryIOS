#!/usr/bin/env bash
# lucina-agentd juno-decide hook. Installed as ~/.config/lucina-agentd/juno-decide.
#
# Owner, 2026-11-08: CodeCor runs as root by default and everything is allowed on Juno
# ("we made the OS, we are the only ones on it, we are testing"). So juno_bash calls are
# approved without a phone question. The credential guard in agentd still refuses
# credential paths before this hook runs, and secrets are still redacted from output.
# Every call is logged to ~/.local/state/lucina-agentd/juno-bash-audit.log.
#
# To ask the owner on every call again (the 2026-11-07 behaviour), create
# ~/.config/lucina-agentd/juno-decide.ask (or set JUNO_DECIDE_ASK=1).
#
# stdin: {"command", "agent_id", "timeout_seconds"} from `lagent juno-mcp`.
# stdout: first line `approve` lets the call run; anything else (or a non-zero exit)
# refuses it, and the text is the refusal the agent sees.
set -u
LUCINA_RS="${LUCINA_RS:-$HOME/.local/bin/lucina-rs}"
WAIT="${JUNO_DECIDE_WAIT:-300}"   # agentd kills the hook at 330 s
AUDIT="${JUNO_DECIDE_AUDIT:-$HOME/.local/state/lucina-agentd/juno-bash-audit.log}"

input=$(cat)
command=$(jq -r '.command // empty' <<<"$input" 2>/dev/null)
agent=$(jq -r '.agent_id // "unknown"' <<<"$input" 2>/dev/null)
timeout_s=$(jq -r '.timeout_seconds // 60' <<<"$input" 2>/dev/null)
if [ -z "$command" ]; then
  echo "refused: the hook got no command"
  exit 1
fi

audit() {
  mkdir -p "$(dirname "$AUDIT")" 2>/dev/null
  jq -cn --arg ts "$(date -u +%FT%TZ)" --arg agent "$agent" --arg cmd "${command:0:2000}" --arg verdict "$1" \
    '{ts:$ts,agent:$agent,verdict:$verdict,command:$cmd}' >>"$AUDIT" 2>/dev/null || true
}

if [ "${JUNO_DECIDE_ASK:-0}" != 1 ] && [ ! -e "$HOME/.config/lucina-agentd/juno-decide.ask" ]; then
  audit "allowed (owner default 2026-11-08)"
  echo "approve"
  echo "allowed by the owner's default (2026-11-08): CodeCor runs as root on Juno"
  exit 0
fi

run="Run it"
deny="Don't run it"
question="A CodeCor Claude agent wants to run a shell command on the Juno laptop as root: ${command:0:1500}"
context="Ask mode is on (juno-decide.ask). Agent: ${agent}. Time limit: ${timeout_s} s. Credentials never pass through this tool (credential paths are refused before this question, secrets are redacted from the output). Rule: no reload or reboot while the owner is using the laptop."

errf=$(mktemp)
trap 'rm -f "$errf"' EXIT
out=$("$LUCINA_RS" decide \
  --question "$question" \
  --option "$run" --option "$deny" \
  --context "$context" \
  --impact medium --reversible false --urgency high \
  --topic codecor-juno-bash \
  --asked-by "lucina-agentd juno_bash (${agent})" \
  --wait "$WAIT" 2>"$errf")
rc=$?
if [ $rc -ne 0 ]; then
  audit "refused: decide failed"
  echo "refused: lucina_decide failed (${rc}): $(head -c 300 "$errf")"
  exit 1
fi

id=$(jq -r '.decision_id // empty' <<<"$out" 2>/dev/null)
chosen=$(jq -r '.chosen_option // empty' <<<"$out" 2>/dev/null)
if [ "$chosen" = "$run" ]; then
  audit "allowed by lucina_decide ${id}"
  echo "approve"
  echo "lucina_decide ${id}: ${chosen}"
  exit 0
fi
status=$(jq -r '.status // empty' <<<"$out" 2>/dev/null)
if [ "$status" = pending ] && [ -n "$id" ]; then
  "$LUCINA_RS" decide withdraw --reason "juno_bash: no answer in time, the call was refused" "$id" >/dev/null 2>&1 || true
  audit "refused: no answer ${id}"
  echo "refused: lucina_decide ${id} got no answer within ${WAIT} s"
  exit 1
fi
audit "refused: ${chosen:-no decision} ${id:-?}"
echo "refused: lucina_decide ${id:-?}: ${chosen:-no decision}"
exit 1
