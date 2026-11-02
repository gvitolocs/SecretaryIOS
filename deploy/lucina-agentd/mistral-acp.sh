#!/bin/sh
# mistral-acp: the Mistral report agent as an ACP provider for lucina-agentd (~/.config/lucina-agentd/acp.toml, section
# agents.mistral). Mistral's Vibe in ACP mode, with its own read-only config (mistral-report.toml, installed as
# ~/.config/mistral-report/config.toml). Install as ~/.local/bin/mistral-acp. The key comes from infisical-brokerd for this
# process only; bytes a paste brought along are dropped from it here.
export VIBE_HOME="$HOME/.config/mistral-report"
exec /usr/local/bin/infisical-broker exec --token-file "$HOME/.config/infisical-mcp/svc-lane-mistral.token" \
  --env 'MISTRAL_API_KEY=secret://lucina/prod#Mistral_api_key_lucina' -- \
  /bin/sh -c 'MISTRAL_API_KEY=$(printf %s "$MISTRAL_API_KEY" | LC_ALL=C tr -cd "[:graph:]"); export MISTRAL_API_KEY; exec "$HOME/.local/bin/vibe-acp"'
