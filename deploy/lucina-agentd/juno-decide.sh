#!/usr/bin/env bash
# lucina-agentd juno-decide hook: every juno_bash call also goes through lucina_decide
# (owner, 2026-11-07). Installed as ~/.config/lucina-agentd/juno-decide.
#
# stdin: {"command", "agent_id", "timeout_seconds"} from `lagent juno-mcp`.
# stdout: first line `approve` lets the call run; anything else (or a non-zero exit)
# refuses it, and the text is the refusal the agent sees. A pending, withdrawn or
# unreachable decision is a refusal: nothing is auto-allowed.
set -u
LUCINA_RS="${LUCINA_RS:-$HOME/.local/bin/lucina-rs}"
WAIT="${JUNO_DECIDE_WAIT:-300}"   # agentd kills the hook at 330 s

input=$(cat)
command=$(jq -r '.command // empty' <<<"$input" 2>/dev/null)
agent=$(jq -r '.agent_id // "unknown"' <<<"$input" 2>/dev/null)
timeout_s=$(jq -r '.timeout_seconds // 60' <<<"$input" 2>/dev/null)
if [ -z "$command" ]; then
  echo "refused: the hook got no command"
  exit 1
fi

run="Run it"
deny="Don't run it"
question="A CodeCor Claude agent wants to run a shell command on the Juno laptop as root: ${command:0:1500}"
context="The owner already pressed Allow on this call's card in CodeCor on Juno. This is the second check he asked for (2026-11-07): juno_bash runs only when both say yes. Agent: ${agent}. Time limit: ${timeout_s} s. Credentials never pass through this tool (credential paths are refused before this question, secrets are redacted from the output). Rule: no reload or reboot while the owner is using the laptop."

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
  echo "refused: lucina_decide failed (${rc}): $(head -c 300 "$errf")"
  exit 1
fi

id=$(jq -r '.decision_id // empty' <<<"$out" 2>/dev/null)
chosen=$(jq -r '.chosen_option // empty' <<<"$out" 2>/dev/null)
if [ "$chosen" = "$run" ]; then
  echo "approve"
  echo "lucina_decide ${id}: ${chosen}"
  exit 0
fi
status=$(jq -r '.status // empty' <<<"$out" 2>/dev/null)
if [ "$status" = pending ] && [ -n "$id" ]; then
  # Still waiting for the owner: take the question back so it does not linger.
  "$LUCINA_RS" decide withdraw --reason "juno_bash: no answer in time, the call was refused" "$id" >/dev/null 2>&1 || true
  echo "refused: lucina_decide ${id} got no answer within ${WAIT} s"
  exit 1
fi
echo "refused: lucina_decide ${id:-?}: ${chosen:-no decision}"
exit 1
