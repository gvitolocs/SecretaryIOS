#!/usr/bin/env bash
# Links / Google Calendar secrets for p1. Run on nezopt (it reads Lucina's env
# file there). Prints no secret values.
#
# 1. Copies Lucina's existing Google OAuth client (GMAIL_* by default, or
#    KEEP_* with GOOGLE_ENV_PREFIX=KEEP) to p1 as
#    /etc/secretary/google-client-id and /etc/secretary/google-client-secret
#    (secretary:secretary 0600).
# 2. Creates the shared secretary-mcp status token:
#    /etc/secretary/mcp-status.token (secretary 0600) and
#    /etc/secretary-mcp/status.token (secretary-mcp 0600), same content.
#
# Then add the env lines it prints and restart both services.
set -euo pipefail

ENV_FILE=${LUCINA_ENV:-/home/nez/Projects/Lucina/private/lucina/.env.local}
P1=${P1:-p1}
PREFIX=${GOOGLE_ENV_PREFIX:-GMAIL}

val() { grep -E "^$1=" "$ENV_FILE" | head -1 | cut -d= -f2- | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'\$//"; }

id=$(val "${PREFIX}_CLIENT_ID")
secret=$(val "${PREFIX}_CLIENT_SECRET")
if [ -z "$id" ] || [ -z "$secret" ]; then
  echo "missing ${PREFIX}_CLIENT_ID or ${PREFIX}_CLIENT_SECRET in $ENV_FILE" >&2
  exit 1
fi

put() { # put <owner> <path>   (content on stdin)
  ssh "$P1" "sudo sh -c 'umask 077; cat > \"$2.tmp\" && chown $1:$1 \"$2.tmp\" && chmod 600 \"$2.tmp\" && mv \"$2.tmp\" \"$2\"'"
}

printf '%s\n' "$id" | put secretary /etc/secretary/google-client-id
printf '%s\n' "$secret" | put secretary /etc/secretary/google-client-secret
unset id secret

token=$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')
printf '%s\n' "$token" | put secretary /etc/secretary/mcp-status.token
printf '%s\n' "$token" | put secretary-mcp /etc/secretary-mcp/status.token
unset token

cat <<'MSG'
Installed (values not shown). Add to /etc/secretary/secretary.env:
  SECRETARY_TIMEZONE=Europe/Copenhagen
  SECRETARY_GOOGLE_CLIENT_ID_FILE=/etc/secretary/google-client-id
  SECRETARY_GOOGLE_CLIENT_SECRET_FILE=/etc/secretary/google-client-secret
  SECRETARY_MCP_STATUS_TOKEN_FILE=/etc/secretary/mcp-status.token
and to /etc/secretary-mcp/secretary-mcp.env:
  SECRETARY_MCP_STATUS_TOKEN_FILE=/etc/secretary-mcp/status.token
then: systemctl restart secretary-server secretary-mcp
MSG
