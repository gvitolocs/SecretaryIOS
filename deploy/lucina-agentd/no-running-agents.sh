#!/usr/bin/env bash
# The build step of `lagent deploy lucina-agentd`: restarting agentd interrupts the agents it runs, so a
# deploy waits until none is running. Claude and Secretary Code sessions only mirror themselves into the
# list and run elsewhere, so they don't count. `lagent deploy lucina-agentd --force` goes ahead anyway.
set -euo pipefail
# --json prints sorted keys, one per line: provider comes before status.
running=$(~/.local/bin/lagent --json ls | awk -F'"' '
  $2 == "provider" { provider = $4 }
  $2 == "status" && $4 == "running" && provider !~ /^(claude-session|secretary-code)/ { n++ }
  END { print n + 0 }')
if [ "$running" -gt 0 ] && [ "${LAGENT_FORCE:-}" != 1 ]; then
  echo "$running agent(s) running; a restart of lucina-agentd would interrupt them. Deploy when they are idle (lagent ls), or with --force." >&2
  exit 1
fi
