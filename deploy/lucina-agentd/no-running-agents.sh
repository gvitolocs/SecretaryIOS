#!/usr/bin/env bash
# The build step of `lagent deploy lucina-agentd`: restarting agentd interrupts the agents it runs, so a
# deploy waits until none is running. `lagent deploy lucina-agentd --force` goes ahead anyway.
set -euo pipefail
running=$(~/.local/bin/lagent --json ls | grep -c '"status": "running"' || true)
if [ "$running" -gt 0 ] && [ "${LAGENT_FORCE:-}" != 1 ]; then
  echo "$running agent(s) running; a restart of lucina-agentd would interrupt them. Deploy when they are idle (lagent ls), or with --force." >&2
  exit 1
fi
