#!/bin/sh
# vibe-codecor-acp: Mistral Vibe as a coding agent for CodeCor through lucina-agentd (acp.toml agents.vibe). Its own
# VIBE_HOME (~/.config/vibe-codecor: mistral-large, every tool asks through ACP), on the owner's Vibe sign-in in the OS
# keyring. MISTRAL_API_KEY is removed: Vibe would prefer it over the sign-in (the owner's rule: no API key).
export VIBE_HOME="$HOME/.config/vibe-codecor"
exec env -u MISTRAL_API_KEY "$HOME/.local/bin/vibe-acp"
