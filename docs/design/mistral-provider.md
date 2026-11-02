# Mistral as a model provider (2026-11-02)

Mistral answers through the same client as DeepSeek (`crates/lucina-llm`), selected by `Dialect::Mistral`. DeepSeek stays the
default: Mistral answers her turns only with `LUCINA_MODEL_PROVIDER=mistral` and a key. Without a key that setting logs an error and
DeepSeek answers.

## What differs from DeepSeek

- No `stream_options` and no `thinking` in a request (not in Mistral's API reference).
- `reasoning_content` is not sent back.
- Tool call ids must be 9 letters and digits. Other ids are hashed the same way on the call and on its result.
- `content` may be a list of chunks; only `text` chunks are the answer, and `thinking` chunks go to the reasoning.

## Settings

`MISTRAL_API_KEY` or `MISTRAL_KEY_FILE`, `MISTRAL_MODEL` (default `mistral-small-latest`), `MISTRAL_BASE_URL`, `MISTRAL_TIMEOUT_MS`,
`MISTRAL_MAX_RETRIES`. The key is `secret://lucina/prod#Mistral_api_key_lucina`, from broker caller `svc-lucina-mistral`, bound to
`lucina-rs.service`, added as one more `infisical-broker exec` in the `70-infisical.conf` drop-in. An `--env` value is read when the
unit starts, so after rotating the key restart `lucina-rs`.

## Trying a provider

`POST /api/secretary/models/try` (bearer token, not forwarded by secretary-server) makes one call through a named provider, behind the
redactor, with her real tool specs if asked (`"tools": "registry"`). No turn runs: no timeline, no Honcho, no recent chat, and no tool
is executed. The CLI is `lucina-rs model-try [--provider mistral] [--model id] [--stream] [--tools] "prompt"`.

`/health` shows `modelProvider` and `mistral.{model, hasKey}`.

## Rollback

Remove `LUCINA_MODEL_PROVIDER` from `mode.env` and restart. To remove Mistral completely, also take the exec out of the drop-in and the
two callers (`svc-lucina-mistral`, `svc-lane-mistral`) out of `broker.toml`.

## The report agent (Lucina ↔ Mistral)

`message_mistral` (`crates/lucina-tools/src/tools/reach.rs`, `docs/lucina-rust/REACH.md`) gives a report or a question to a Mistral agent in
Lucina Agents, and she can write again on the same thread. The answer is read from the agent's timeline. She needs no agent or room from
him: "post the report to the agent" goes to `LUCINA_REACH_REPORT_AGENT` (`claude` unless set to `mistral`).

- **Provider:** `[agents.mistral]` in `~/.config/lucina-agentd/acp.toml`, running `deploy/lucina-agentd/mistral-acp.sh` with
  `deploy/lucina-agentd/mistral-report.toml` as `~/.config/mistral-report/config.toml`. agentd reads `acp.toml` when it starts.
- **What it can do:** answer from the text it is given. Vibe's tools are off and a tool call is denied, so it can't read files, run commands or
  change anything. Merging, deploying and spending stay with the existing go-ahead routes.
- **Where he sees it:** the `agent:mistral` conversation in All chats (the Mistral chip on the web), and the answer in the chat it came from.
  Every item has `meta.via` and the `reach` kind, so none of it reaches his phone.
- **What leaves:** the thread text after `clean_outbound` (link tokens and secret references masked) and the turn's redaction. Private text goes to Mistral
  under its data terms: check the Privacy setting at admin.mistral.ai before sending any.
- **Off:** `LUCINA_REACH_MISTRAL=off`. Settings: `LUCINA_REACH_MISTRAL_PROVIDER`, `LUCINA_REACH_MISTRAL_MODEL`, `LUCINA_REACH_MISTRAL_TIMEOUT_SECS`.

## The connector

Mistral's custom connector uses `https://getsecretary.tech/mcp` with OAuth. `caller_for_host` names its caller `mistral`, so its turns land in the same
`agent:mistral` conversation. Its callback must be in `SECRETARY_MCP_OAUTH_REDIRECTS`.
