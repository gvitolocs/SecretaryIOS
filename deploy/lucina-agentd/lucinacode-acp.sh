#!/bin/sh
# lucinacode-acp: LucinaCode (opencode-rust `oc-cli acp`) on the local Qwen as an ACP provider for lucina-agentd
# (~/.config/lucina-agentd/acp.toml, section agents.lucinacode). Install as ~/.local/bin/lucinacode-acp, with the host build
# of oc-cli at ~/.local/bin/oc-cli. CodeCor on Juno offers it beside Claude, Codex and Mistral.
# The same model settings as the Juno app (LucinaOS crates/lucina-app-coder): Qwen behind the gateway, thinking medium,
# compacting at 60% of 81,920 tokens. Edits and commands are asked of the client (agentd's permission book), never allowed here.
export OC_BASE_URL="http://127.0.0.1:11436/v1"
export OC_MODEL="openai/qwen3.8:27b-q4-72k"
export OC_API_KEY_FILE="$HOME/.config/llama-server-api.key"
export OC_CONTEXT_WINDOW=81920
export OC_COMPACT_BUFFER=32768
export OC_EXTRA_BODY='{"chat_template_kwargs":{"enable_thinking":true,"reasoning_effort":"medium"},"temperature":1.0,"top_p":0.95,"top_k":20,"min_p":0.0,"presence_penalty":0.0}'
export OC_DATA="${LUCINACODE_ACP_DATA:-$HOME/.local/share/lucinacode-acp}"
exec env -u OC_BASE -u OC_PERMISSIONS -u OPENAI_API_KEY "$HOME/.local/bin/oc-cli" acp
