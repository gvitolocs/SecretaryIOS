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
