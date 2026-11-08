#!/bin/sh
# with-mistral-key <command…>: runs the command with MISTRAL_API_KEY from the broker, or without it when the broker can't give it.
# Mistral is optional for Lucina, so a broker or Infisical outage must never keep her from starting (docs/design/mistral-provider.md).
# Install as ~/.local/bin/with-mistral-key; used in the lucina-rs.service drop-in 70-infisical.conf. The key stays out of argv and logs.
broker=/usr/local/bin/infisical-broker
token=$HOME/.config/infisical-mcp/svc-lucina-mistral.token
ref='MISTRAL_API_KEY=secret://lucina/prod#Mistral_api_key_lucina'
if [ -r "$token" ] && "$broker" exec --token-file "$token" --env "$ref" -- /bin/true 2>/dev/null; then
  exec "$broker" exec --token-file "$token" --env "$ref" -- "$@"
fi
echo "with-mistral-key: no key from the broker; starting without Mistral" >&2
exec "$@"
