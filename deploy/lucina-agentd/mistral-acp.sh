#!/bin/sh
# mistral-acp: the Mistral report agent as an ACP provider for lucina-agentd (~/.config/lucina-agentd/acp.toml, section
# agents.mistral). Mistral's Vibe in ACP mode, with its own read-only config (mistral-report.toml, installed as
# ~/.config/mistral-report/config.toml). Install as ~/.local/bin/mistral-acp. Since 2026-11-04 it runs on the owner's own
# Vibe sign-in (OS keyring), so it counts against the Vibe Code allowance and not the API allowance. MISTRAL_API_KEY is
# removed from the environment because Vibe would prefer it over the sign-in.
export VIBE_HOME="$HOME/.config/mistral-report"
exec env -u MISTRAL_API_KEY "$HOME/.local/bin/vibe-acp"
