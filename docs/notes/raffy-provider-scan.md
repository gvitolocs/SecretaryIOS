# LM-R016 — where Lucina declares its OpenAI-compatible providers

Scan for change **LM-C008** (add a llama-server provider, proposed name `raffy-qwen3-4b`).
Read-only: no code changed, nothing built, no service touched. Host `nezopt`, worktree
`/home/nez/Projects/.worktrees/SecretaryIOS/lm-raffy-provider`, branch `lm-raffy-provider`.

All line numbers are from this worktree at `d90c0f64`. No secret value appears below — only
environment-variable names, Infisical reference strings and file paths.

**Headline:** there is no single provider table. Providers are compiled into Rust as small enums
plus one per-provider config struct, and their *settings* come from process environment read **once
at startup**. The generic provider path performs **no URL validation at all**, so a Tailscale
`http://…` endpoint is accepted there; the loopback-only rules are per-subsystem and keyed on their
own variables. The one genuine friction is that a *new named provider* needs about six code sites
plus a restart, and the live config it reads is **outside the repo**.

---

## 1. Where providers are declared, and how a request picks one

### The client and its settings (one struct, prefix-driven)

`crates/lucina-llm` is the whole OpenAI-compatible client. One type serves every provider; what
differs per provider is a base URL, a model name, a key source and a dialect.

- `crates/lucina-llm/src/lib.rs:28-32` — the only provider constants in the crate:
  `DEFAULT_BASE_URL` (`https://api.deepseek.com/v1`), `DEFAULT_MODEL`, `PRO_MODEL`,
  `MISTRAL_BASE_URL`, `MISTRAL_MODEL`. **This is the block a new provider extends.**
- `lib.rs:402-426` — `DeepSeekConfig { base_url, model, key, timeout, max_retries, backoff, dialect }`
  and its `Default`. The type keeps the old name; the module doc at `lib.rs:1-2` says it is the
  client for "DeepSeek, the local Qwen gateway and Mistral".
- `lib.rs:447-467` — `with_env(prefix, get)` is the generic plumbing: it reads
  `<PREFIX>_BASE_URL`, `<PREFIX>_MODEL`, `<PREFIX>_TIMEOUT_MS`, `<PREFIX>_MAX_RETRIES`,
  `<PREFIX>_API_KEY`, `<PREFIX>_KEY_FILE`. Adding a provider therefore costs **no new parsing code**.
- `lib.rs:432-434` and `lib.rs:437-445` — the two factories today: `from_lookup` (prefix `DEEPSEEK`)
  and `mistral_from_lookup` (prefix `MISTRAL`). A third, `raffy_from_lookup`, is the pattern to copy.
- `lib.rs:384-391` — `enum Dialect { DeepSeek, Mistral }`, where `DeepSeek` is documented as
  "DeepSeek **and the local Qwen gateway**" (`lib.rs:385`). A `--jinja` llama-server speaks the plain
  OpenAI shape, so **it belongs under the existing `DeepSeek` dialect; no new dialect is needed.**
  `Dialect::name()` at `lib.rs:394-399` is the only place a dialect becomes a string.

### The server-side selection

- `crates/lucina-server/src/config.rs:15-21` — `enum ModelProvider { DeepSeek, Mistral }`.
  **This is the provider table proper, and it is compiled in, not data.**
- `config.rs:249-253` — the fields on `Config`: `pub deepseek`, `pub mistral` (doc: "reached by name
  (`crate::model_try`) unless `ModelProvider::Mistral` is set"), `pub model_provider`.
- `config.rs:668-672` — built once: `deepseek: DeepSeekConfig::from_lookup(&get)`,
  `mistral: DeepSeekConfig::mistral_from_lookup(&get)`, and
  `model_provider: match get("LUCINA_MODEL_PROVIDER")… { Some("mistral") => Mistral, _ => DeepSeek }`.
  An unknown value silently falls through to DeepSeek (`config.rs:672`).
- `crates/lucina-server/src/lib.rs:224-250` — `AppState::build` makes one client per provider and
  *drops* a provider that has no key: `lib.rs:225` `.filter(|m| m.has_key())`, and `lib.rs:226-231`
  logs "LUCINA_MODEL_PROVIDER=mistral but Mistral has no key: DeepSeek answers". A worthwhile
  guardrail to copy: a misconfigured new provider must degrade, not fail to boot.
- `crates/lucina-server/src/lib.rs:255-258` — `named_models` is constructed in exactly this one
  place (field declared `lib.rs:129`).
- `crates/lucina-server/src/model_try.rs:21-24` — `NamedModels { deepseek, mistral: Option<…> }`.
- `crates/lucina-server/src/model_try.rs:56-62` — **how a request picks a provider by name**: a
  fixed two-arm match, `"deepseek" | "mistral"`, `other => bad_request("unknown provider {other}")`;
  a named provider without a key gives 503 `no_key` (`model_try.rs:59-61`). The default when the
  field is absent is `"mistral"` (`model_try.rs:56`).
- `crates/lucina-server/src/api.rs:381-382` — `/health` reports `modelProvider` and
  `mistral.{model, hasKey}`.

So a turn picks a provider **globally** (`LUCINA_MODEL_PROVIDER`), while
`POST /api/secretary/models/try` picks one **by name**. There is no per-role routing.

### Three *separate*, hard-coded "local Qwen only" endpoints (not the table above)

These are where the existing gateway `http://127.0.0.1:11436/v1` is declared. They are not
`ModelProvider` entries and gain nothing from adding one — but they are the precedent for how a
Qwen endpoint looks, and the closest neighbours to a llama-server target:

- `crates/lucina-server/src/secret_keep/local.rs:28-34` —
  `LUCINA_SECRET_KEEP_QWEN_{URL,MODEL,KEY_FILE}`, default `http://127.0.0.1:11436/v1`, model
  `qwen3.8:27b-128k` (`local.rs:33`).
- `crates/lucina-server/src/notebooks.rs:47-72` — `LUCINA_NOTEBOOKS_QWEN_{URL,MODEL,KEY_FILE}`,
  same default (`notebooks.rs:70`), key from `~/.config/qwen-openai.env` (`notebooks.rs:40,49-55`).
- `crates/lucina-vault-ingest/src/lib.rs:55` `DEFAULT_QWEN_URL`, `:245-248`
  `QWEN_URL`/`QWEN_KEY_FILE`, model `qwen3.8:27b-q4-72k` (`lib.rs:57`).
- `crates/lucina-notebooks/src/llm.rs:10-27` — a *second* provider enum
  (`Model::{Deepseek, Qwen}`, `parse` accepting `"qwen" | "local"`), validated at
  `crates/lucina-notebooks/src/api.rs:641` ("model is deepseek or qwen"), mirrored in MCP at
  `crates/secretary-mcp/src/notebooks.rs:52-66` and in the web client at
  `web/src/api/notebooks.ts:13` and `web/src/views/notebooks/ChatColumn.tsx:299-300`.
- Non-chat paths too: `deploy/lucina-agentd/lucinacode-acp.sh:7-12` (`OC_BASE_URL`, `OC_MODEL`,
  `OC_API_KEY_FILE`, `OC_CONTEXT_WINDOW`), `crates/lucina-server/src/about/extract.rs:37`,
  `crates/lucina-server/src/lib.rs:493` (a separate `:8080` default).
- The agent-side list is elsewhere again: `crates/lucina-delegate/src/types.rs:8-31`
  (`AgentId::{Claude, Grok, Chatgpt, Mistral}` with `const ALL: [AgentId; 4]`), budgets in
  `crates/lucina-delegate/src/config.rs:56-84`.

---

## 2. How a provider's API key is loaded

**Resolution order, identical for every provider** — `crates/lucina-llm/src/lib.rs:461-465`:

1. `<PREFIX>_API_KEY` wins when non-empty after cleaning (`lib.rs:462`) → `KeySource::Inline`.
2. else `<PREFIX>_KEY_FILE` → `KeySource::File` (`lib.rs:463`).
3. else `KeySource::None` (`lib.rs:464`), and every call then fails `LlmError::NoKey`
   (`lib.rs:677`; documented `lib.rs:436`).

Supporting machinery:

- `KeySource` — `lib.rs:366-369`, with a hand-written `Debug` (`lib.rs:372-378`) that prints only
  the path for `File` and `Inline(<redacted>)` for `Inline`. `DeepSeekConfig` derives `Debug`
  (`lib.rs:402`) and inherits it; `impl Debug for DeepSeek` prints only `cfg` (`lib.rs:543-546`).
- `clean_key` — `lib.rs:472-474` strips every non-ASCII-graphic byte, from both inline keys
  (`lib.rs:462`) and file contents (`lib.rs:631`); the comment records why: a pasted Mistral key
  carried four stray bytes and the API answered 401.
- **Rotation hot-reload** — `KeyCache` at `lib.rs:531-534`, field `lib.rs:539`, logic
  `lib.rs:620-638`: a `File` key is re-read when `checked` is older than 5 s, so a rotation lands
  without a restart (`lib.rs:626-634`; doc `lib.rs:11-13`). An `Inline` key is a fixed string
  (`lib.rs:622`).
- `has_key()` — `lib.rs:616-618` — the non-revealing probe the server uses to decide whether a
  provider exists at all (`lib.rs:225`).
- **Redactor registration** — `crates/lucina-server/src/secrets.rs:108` `register_bootstrap(cfg)`
  registers each provider key **by digest only** (`secrets.rs:120-127` for `DEEPSEEK_API_KEY`,
  `secrets.rs:128-135` for `MISTRAL_API_KEY`), reading a `File` source through `file_value`
  (`secrets.rs:99-104`). **A new provider must be added here or its key will not be recognised in
  outbound text and could leak into a reply.**

**Infisical.** Yes, integrated — and it injects *environment*, not key files:

- Crate `crates/lucina-secrets`; Universal Auth plus secret fetch in
  `crates/lucina-secrets/src/infisical.rs` (self-hosted server; `infisical.rs:4-13`), credentials
  from systemd credential files (`crates/lucina-secrets/src/config.rs:266-272`).
- Reference grammar `secret://<project>/<env>/<path>#<KEY>` —
  `crates/lucina-secrets/src/reference.rs:1-11`, parser `reference.rs:50-79`, printer
  `reference.rs:95-102`. Also `docs/lucina-rust/SECRETS.md:17,57,129-137`.
- The broker wraps `ExecStart`: `deploy/lucina-rs/with-mistral-key.sh:5-9` runs
  `infisical-broker exec --token-file ~/.config/infisical-mcp/svc-lucina-mistral.token`
  `--env 'MISTRAL_API_KEY=secret://lucina/prod#Mistral_api_key_lucina' -- <cmd>`, and **degrades to
  starting without the key** if the broker is unavailable (`with-mistral-key.sh:11-12`) — the
  precedent for "an optional provider must never stop Lucina".
- `deploy/lucina-memo/70-infisical.conf:6` sets
  `DEEPSEEK_API_KEY=secret://lucina/prod/lucina#DEEPSEEK_API_KEY_LUCINA` (its line 3 notes
  `lucina.env` no longer sets `DEEPSEEK_KEY_FILE`).
- Prose record: `docs/design/mistral-provider.md:16-19` — the reference, caller name
  `svc-lucina-mistral`, binding to `lucina-rs.service`, "added as one more `infisical-broker exec`
  in the `70-infisical.conf` drop-in", and the caveat: "An `--env` value is read when the unit
  starts, so after rotating the key restart `lucina-rs`".
- Note the two names differ by design: the Infisical **secret name** (`Mistral_api_key_lucina`) maps
  to the **process variable** (`MISTRAL_API_KEY`).

**Naming convention to follow.** Chat providers use bare `<PROVIDER>_*` (`DEEPSEEK_*`
`lib.rs:433`, `MISTRAL_*` `lib.rs:444`); subsystem-local endpoints use
`LUCINA_<AREA>_<X>_URL|MODEL|KEY_FILE` (`secret_keep/local.rs:30-34`, `notebooks.rs:48`). For a
provider named `raffy` the natural set is `RAFFY_BASE_URL`, `RAFFY_MODEL`, `RAFFY_API_KEY` /
`RAFFY_KEY_FILE`, `RAFFY_TIMEOUT_MS`, `RAFFY_MAX_RETRIES` — free from `with_env` (`lib.rs:447-467`).

**agentd scrubs these.** `crates/lucina-agentd/src/env.rs:60-71` denies a child environment any
variable prefixed `OPENAI_`, `ANTHROPIC_`, `MISTRAL_`, `DEEPSEEK_`, `INFISICAL_` unless the job
declares it (`scrubbed_env`, `env.rs:76`). A new `RAFFY_` prefix inherits no exception; if the key
must reach a child process it needs declaring there.

---

## 3. Restart or runtime? and the deploy step

**A new provider needs a code change and a restart. There is no runtime discovery.**

- `crates/lucina-server/src/main.rs:1639` `let cfg = Config::from_env()?;` inside `Cmd::Serve`, then
  `AppState::build(cfg, …)` at `main.rs:1643`. `Config::from_env` is `config.rs:423-425` and its
  sibling `from_lookup` is `config.rs:437`; the closure is live `std::env::var`, but it is invoked
  once, so the result is a **startup snapshot**. No `EnvironmentVariableChanged` handler and no
  config watcher exists.
- Clients are frozen into state (`lib.rs:224-258`); dispatch is a compiled match
  (`model_try.rs:57-62`).
- The exception is the **key**, not the provider: a `KeySource::File` is re-read every 5 s
  (`lib.rs:626-634`). `*_API_KEY` from the broker is fixed at start
  (`docs/design/mistral-provider.md:19`).
- `agentd` is the same shape: its provider list is built at process start
  (`crates/lucina-agentd/src/bin/lucina-agentd.rs:71-75` → `src/providers/mod.rs:43,71`), reading
  `~/.config/lucina-agentd/acp.toml` once (`src/providers/acp.rs:100-104`; "agentd reads `acp.toml`
  when it starts", `docs/design/mistral-provider.md:41`).

**Deploy.** Single trunk; every target builds from `main`; recipes in `.lucina/pipeline.toml`, gate
is `deploy-guard` (`docs/repo-workflow.md:3-8,68-70,200-212`).

- `[deploy.lucina-rs]` — `.lucina/pipeline.toml:40-43`, install step
  `deploy/lucina-rs/install.sh`: build (`install.sh:21` `cargo build --release -p lucina-server`),
  binary (`:23-26`), units (`:38`), `daemon-reload` (`:39`), and **`systemctl --user restart
  lucina-rs.service`** (`:41`), then `curl /health` (`:44`). Agentd counterpart:
  `[deploy.lucina-agentd]` — `.lucina/pipeline.toml:34-38`, install `deploy/lucina-agentd/install.sh`, restart at `:25`.
- **Crucially, no deploy step writes provider config.** `install.sh:7-9`: "Config is not generated
  here: `~/.config/lucina-rs/lucina.env` … and `mode.env` must exist; drop-ins in
  `lucina-rs.service.d` (Infisical, LAN, Gemini) are left as they are", and `install.sh:15-18`
  hard-fails if those files are missing. The live unit reads them at
  `deploy/lucina-rs.service:16-17` (`EnvironmentFile=%h/.config/lucina-rs/lucina.env`, then
  `mode.env`), starts at `:19` and gates on `/health` at `:21`;
  `Restart=on-failure`, `RestartSec=10` (`:22-23`).

**Net for LM-C008:** the code lands from `main` via `deploy/lucina-rs/install.sh` (which restarts),
but `RAFFY_BASE_URL`/`RAFFY_MODEL` must be hand-added to `~/.config/lucina-rs/lucina.env` (or
`mode.env`) and the key to the broker drop-in — **out-of-tree, on nezopt**. Those files are outside
this workspace, so the operator step cannot be authored here.

---

## 4. Tests, fixtures, and any gate over the provider list

- `crates/lucina-llm/tests/deepseek.rs` — offline `wiremock`, no network, no real key. Helper
  `client()` at `:13-23` points `base_url` at the mock; tool round trip with a tempdir key file
  `:40-73`, retry/`NoKey` `:74-116`, timeout `:119`, SSE including tool-call deltas `:137-204`,
  DSML recovery `:226`. Debug redaction assertion `:65`.
- `crates/lucina-llm/tests/mistral.rs` — the fixture a new provider copies. Notably
  `settings_come_from_mistral_variables_only` at `:59-75`: a **pure lookup-closure test**
  (`DeepSeekConfig::mistral_from_lookup(|k| match k {…})`) asserting base URL, model, dialect, and
  that `DEEPSEEK_*` is ignored. Wire-shape assertions read `server.received_requests()` (`:54-57`);
  a `deepseek_requests_are_unchanged` regression guard at `:122`; invisible-character key cleaning
  via a `MISTRAL_BASE_URL` override `:221-242`; Debug redaction `:244`.
- **`crates/lucina-server/tests/model_try.rs` — the real gate over the named-provider list.** It
  enumerates providers implicitly (`:44`, `:88`, `:119`, `:136`), mutates `Config` through the
  literal helper `with_mistral` (`:13-24`), and
  `bad_requests_are_refused_before_any_call` (`:177-189`) asserts **`{"provider":"qwen"}` → HTTP 400**
  with nothing sent (`:188`). Because LM-C008's provider string is `raffy`, not `qwen`, **that
  assertion stays valid** — do not "fix" it; it is load-bearing. But the harness builds state via
  `AppState::build`, so a new provider needs a `with_raffy` helper mirroring `:13-24` and a
  no-key-503 test mirroring `:43-50`.
- `crates/lucina-server/tests/common/mod.rs:1-80` — real server on an ephemeral port plus
  `lucina_server::fake` (a scripted OpenAI-compatible mock; `crates/lucina-server/src/fake.rs:1-25`,
  `:241-281`). Nothing here touches the network.
- Other lists with their own tests: `crates/lucina-notebooks/src/llm.rs:178-185`
  (`Model::parse(Some("gpt")) == None` at `:184`), `crates/secretary-mcp/tests/notebooks.rs:294,306`
  (wire value `"qwen"`), `crates/lucina-delegate/tests/fallback_cancel.rs:234` (iterates
  `AgentId::ALL`).
- **No gate validates a provider table.** `crates/lucina-upstream/src/registry.rs:251-361`
  `Registry::validate` (plus `errors()` `:364`, fixture `:491-495`) is for `docs/upstreams.toml` —
  upstream *watching*, not LLM providers. `decision.rs:18-152` is recommendation text. The `gate.rs`
  files are unrelated by name only: `crates/lucina-agentd/src/manager/gate.rs:1-16` (diffgate),
  `crates/lucina-vault-ingest/src/gate.rs:1-31` (GPU lease for the local Qwen),
  `crates/secretary-media-stream/src/gate.rs:1-8`. `crates/lucina-server/src/capabilities.rs:34-132`
  is prompt text with a hard-coded default model name at `:89-92`.

### The loopback rules — and whether a Tailscale URL would be refused

**Per subsystem, not global.** Each is keyed on its *own* variable:

- `crates/lucina-vault-ingest/src/lib.rs:171-181` `is_loopback_url` — `http://` only, host exactly
  `127.0.0.1 | localhost | ::1`; enforced in `IngestConfig::check` `:282-291` ("the Qwen URL must be
  the local gateway"), reached from `from_lookup` `:278`. Test `only_the_local_gateway` `:948-975`
  asserts `http://10.0.0.5:11436` **is refused** (`:962`).
- `crates/lucina-server/src/secret_keep/local.rs:39-42` reuses it; enforced before any request at
  `:91-93` (`LocalError::NotLocal` `:69`, surfaced as `local_model_not_local` at
  `crates/lucina-server/src/secret_keep.rs:1030`). Its tests `:258-288` explicitly reject
  `http://10.0.0.5:11436` (`:268`), and `crates/lucina-server/tests/secret_keep.rs:531` agrees.
- `crates/lucina-secrets/src/config.rs:209-218` — the Infisical URL itself must be `https://` or loopback.
- Opposite direction: `crates/lucina-tools/src/tools/web.rs:33-49` `public_ip` treats `100.64.0.0/10`
  as **private** (`:21`; test `:391-395` asserts `100.64.0.1` and `100.107.164.33` are not public),
  reused in `crates/lucina-server/src/share/extract.rs:16,62,1032` and
  `crates/secretary-server/src/links.rs:517`. Gaps recorded in `docs/verification-gaps.md:80-90`
  (L1) and `:92-99` (L2).

**Answer for LM-C008:** a Tailscale `http://<TAILNET_IP>:8080/v1` **is accepted** by the generic
provider path — `DeepSeekConfig.base_url` has **no scheme or host validation** (`lib.rs:449-451`
only trims a trailing `/`), and neither `config.rs:668-672` nor `lib.rs:224-250` checks it. It would
be **rejected** by secret-keep and vault-ingest, but those bind their own `*_QWEN_URL` variables
(`local.rs:30`, `LUCINA_VAULT_INGEST_QWEN_URL`) and are unaffected unless the new provider is
pointed at them. Notebooks likewise only *defaults* to loopback (`notebooks.rs:70`) without
enforcing it. Two honest cautions: (a) the remote endpoint is plaintext HTTP carrying the owner's
prompts over the tailnet, and `ufw` does not filter tailnet traffic on nezopt
(`deploy/lucina-media.service:8`, `deploy/lucina-com3d2.service:6-7`) — a decision note, not a code
block; (b) anything routed through `public_ip` / `links.rs` refuses `100.64/10` by design.

---

## 5. Proposed minimal diff (text only — **not applied**)

Provider string `raffy`, env prefix `RAFFY`, model `qwen3-4b`, `Dialect::DeepSeek` (plain OpenAI
shape, `lib.rs:385`). `<TAILNET_IP>` stays a placeholder. Six required sites plus tests; each line
mirrors an existing Mistral line.

```diff
--- a/crates/lucina-llm/src/lib.rs
+++ b/crates/lucina-llm/src/lib.rs
@@ -31,6 +31,9 @@ pub const MISTRAL_BASE_URL: &str = "https://api.mistral.ai/v1";
 pub const MISTRAL_MODEL: &str = "mistral-small-latest";
+/// A llama-server on another machine, reached over the tailnet. Placeholder until nezopt knows it.
+pub const RAFFY_BASE_URL: &str = "http://<TAILNET_IP>:8080/v1";
+pub const RAFFY_MODEL: &str = "qwen3-4b";
@@ -445,6 +448,16 @@ impl DeepSeekConfig {
         .with_env("MISTRAL", get)
     }
+
+    /// The same settings under `RAFFY_*`: a small llama-server started with `--jinja`, so the plain
+    /// OpenAI rules (`Dialect::DeepSeek`) apply and only the base URL and the model differ. Without
+    /// a key the client exists but every call is [`LlmError::NoKey`].
+    pub fn raffy_from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
+        Self {
+            base_url: RAFFY_BASE_URL.into(),
+            model: RAFFY_MODEL.into(),
+            ..Self::default()
+        }
+        .with_env("RAFFY", get)
+    }
```

```diff
--- a/crates/lucina-server/src/config.rs
+++ b/crates/lucina-server/src/config.rs
@@ -17,6 +17,7 @@ pub enum ModelProvider {
     #[default]
     DeepSeek,
     Mistral,
+    Raffy,
@@ -251,6 +252,7 @@ pub struct Config {
     /// Mistral (`MISTRAL_*`): reached by name (`crate::model_try`) unless [`ModelProvider::Mistral`] is set.
     pub mistral: DeepSeekConfig,
+    /// The tailnet llama-server (`RAFFY_*`): reached by name unless `LUCINA_MODEL_PROVIDER=raffy`.
+    pub raffy: DeepSeekConfig,
     pub model_provider: ModelProvider,
@@ -669,8 +671,10 @@ impl Config {
             deepseek: DeepSeekConfig::from_lookup(&get),
             mistral: DeepSeekConfig::mistral_from_lookup(&get),
+            raffy: DeepSeekConfig::raffy_from_lookup(&get),
             model_provider: match get("LUCINA_MODEL_PROVIDER").as_deref().map(str::trim) {
                 Some("mistral") => ModelProvider::Mistral,
+                Some("raffy") => ModelProvider::Raffy,
                 _ => ModelProvider::DeepSeek,
             },
```

```diff
--- a/crates/lucina-server/src/lib.rs
+++ b/crates/lucina-server/src/lib.rs
@@ -225,6 +225,11 @@ impl AppState {
         let mistral = Some(Arc::new(DeepSeek::new(cfg.mistral.clone())?)).filter(|m| m.has_key());
         if cfg.model_provider == ModelProvider::Mistral && mistral.is_none() {
             tracing::error!("LUCINA_MODEL_PROVIDER=mistral but Mistral has no key: DeepSeek answers");
         }
+        let raffy = Some(Arc::new(DeepSeek::new(cfg.raffy.clone())?)).filter(|m| m.has_key());
+        if cfg.model_provider == ModelProvider::Raffy && raffy.is_none() {
+            tracing::error!("LUCINA_MODEL_PROVIDER=raffy but the tailnet llama-server has no key: DeepSeek answers");
+        }
```

The turn-model `match` at `lib.rs:236-250` picks the provider from one tuple arm; adding a third
provider means either nesting a second match or restructuring it. **The smallest honest version
keeps `raffy` out of the turn path entirely** (never set `LUCINA_MODEL_PROVIDER=raffy` for a 4B
model) and only registers it as a named provider:

```diff
@@ -255,6 +260,7 @@ impl AppState {
         let named_models = model_try::NamedModels {
             deepseek: Arc::new(secrets::RedactingModel(deepseek)),
             mistral: mistral.map(|m| Arc::new(secrets::RedactingModel(m)) as Arc<dyn ChatModel>),
+            raffy: raffy.map(|m| Arc::new(secrets::RedactingModel(m)) as Arc<dyn ChatModel>),
         };
```

```diff
--- a/crates/lucina-server/src/model_try.rs
+++ b/crates/lucina-server/src/model_try.rs
@@ -22,6 +22,7 @@ pub struct NamedModels {
     pub deepseek: Arc<dyn ChatModel>,
     pub mistral: Option<Arc<dyn ChatModel>>,
+    pub raffy: Option<Arc<dyn ChatModel>>,
@@ -59,6 +60,9 @@ pub async fn handle(st: &crate::AppState, body: &Value) -> Result<Value, ApiError> {
         })?,
+        "raffy" => st.named_models.raffy.clone().ok_or_else(|| {
+            ApiError::new(503, "no_key", "the tailnet llama-server has no key (RAFFY_API_KEY / RAFFY_KEY_FILE)")
+        })?,
         other => return Err(ApiError::bad_request(format!("unknown provider {other}"))),
     };
```

Header doc `model_try.rs:6` lists the accepted `provider` values — extend it with `"raffy"`.

```diff
--- a/crates/lucina-server/src/secrets.rs
+++ b/crates/lucina-server/src/secrets.rs
@@ -135,6 +135,13 @@ pub fn register_bootstrap(cfg: &Config) -> usize {
+    add(
+        "RAFFY_API_KEY",
+        match &cfg.raffy.key {
+            lucina_llm::KeySource::Inline(k) => Some(k.clone()),
+            lucina_llm::KeySource::File(p) => file_value(p),
+            lucina_llm::KeySource::None => None,
+        },
+    );
```

```diff
--- a/crates/lucina-server/src/api.rs
+++ b/crates/lucina-server/src/api.rs
@@ -381,6 +381,7 @@ fn health_value(…)
-            "modelProvider": match c.model_provider { crate::ModelProvider::DeepSeek => "deepseek", crate::ModelProvider::Mistral => "mistral" },
+            "modelProvider": match c.model_provider { crate::ModelProvider::DeepSeek => "deepseek", crate::ModelProvider::Mistral => "mistral", crate::ModelProvider::Raffy => "raffy" },
             "mistral": {"model": c.mistral.model, "hasKey": st.named_models.mistral.is_some()},
+            "raffy": {"model": c.raffy.model, "hasKey": st.named_models.raffy.is_some()},
```

**Tests to add alongside:** `crates/lucina-llm/tests/raffy.rs` with one lookup-closure test modelled
on `mistral.rs:59-75` (asserting `RAFFY_BASE_URL`, `RAFFY_MODEL`, `Dialect::DeepSeek`, and that
`DEEPSEEK_*`/`MISTRAL_*` are ignored); and in `crates/lucina-server/tests/model_try.rs` a
`with_raffy` helper mirroring `:13-24` plus a no-key-503 test mirroring `:43-50`. Leave the
`{"provider":"qwen"}` → 400 assertion at `:182` alone.

### Context limit (16384) and tool-call support: **not declared in this repo**

There is no Rust model registry carrying those fields. `grep` for `context_length` /
`supports_tools` / `max_context` finds only diagnostics char caps
(`crates/lucina-diagnostics/src/ingest.rs:49`), intent caps (`crates/lucina-intent/src/notes.rs:12`),
and *event* fields read from external processes — `crates/lucina-agents/src/events.rs:25-47`
`context_window_max_tokens` / `context_window_used_tokens`, filled from Codex's own report at
`crates/lucina-agentd/src/providers/codex.rs:605-606`, fixture `src/mock.rs:754-755` (`128_000`).
Where the numbers actually live:

- **Encoded in the model name string:** `qwen3.8:27b-128k` (`secret_keep/local.rs:33`,
  `notebooks.rs:71`), `qwen3.8:27b-q4-72k` (`lucina-vault-ingest/src/lib.rs:57`,
  `lucinacode-acp.sh:8`, `lucina-agentd/src/lane.rs:471`).
- **In the llama-server command line itself** — the only place this repo sets a context window
  directly: `deploy/lucina-notebooks/lucina-notebooks-embed.service:6` launches
  `llama-server … -c 8192 -np 2` (the embeddinggemma-2 embedding server, port 8897). This is the
  closest in-repo precedent for LM-C008's own launch flags (`--jinja`, `-c 16384`, `--port 8080`) —
  note it lives in a unit file here, while the raffy unit would be an out-of-tree neighbor.
  The opencode wrapper sets its window with `OC_CONTEXT_WINDOW=81920`
  (`deploy/lucina-agentd/lucinacode-acp.sh:10`).
- **In agent-side TOML:** `max_context_length = 262144`
  (`deploy/lucina-agentd/mistral-report.toml:18`), and `:15,24,33,42`
  (`deploy/lucina-agentd/vibe-codecor.toml`).
- **Out of tree entirely:** the host build of opencode-rust keeps its own provider/model list in
  `~/.config/opencode/opencode.json`, and that is where a per-model numeric `limit`
  (`context`/`output`) would live. **The file is outside this workspace and was not read in this
  scan**, so treat its exact keys and numbers as unverified here — only the pointer is asserted.
  **Numeric per-model limits do not live in this repo** (nothing in `crates/` carries them).
- **Tool-call support is per-`Dialect`, not per-model** (`lib.rs:384-391`): `DeepSeek` sends
  `stream_options`/`thinking`, `Mistral` does not (`lib.rs:658-666`). A `--jinja` llama-server needs
  no new dialect.

So there is **no `16384` knob to set** in `lucina-llm`/`lucina-server`; the only per-request cap is
the fixed `MAX_TOKENS = 4096` (`crates/lucina-server/src/model_try.rs:18`, applied `:77-79`). The
16k window is enforced **by the llama-server itself**, and Lucina never asks for more than 4096
output tokens on the try path. If Lucina must *know* the 16k window, that is a new concept and out
of scope for a minimal diff; the honest options are the model-name suffix convention
(`qwen3-4b-16k`) or the external `opencode.json` `limit`.

One runtime caveat not visible from this repo: `DeepSeek` sends
`stream_options: {"include_usage": true}` for every non-Mistral dialect (`lib.rs:658-659`). Confirm
the target llama-server accepts it — e.g. `lucina-rs model-try --provider raffy --stream "ping"`,
per `docs/design/mistral-provider.md:23-25` — rather than assuming it.

### Deploy-side, out of tree (no repo file does this today)

Add `RAFFY_BASE_URL=http://<TAILNET_IP>:8080/v1` and `RAFFY_MODEL=qwen3-4b` to
`~/.config/lucina-rs/lucina.env`, and a broker line following
`deploy/lucina-rs/with-mistral-key.sh:5-9` with the reference
`secret://lucina/prod/raffy#RAFFY_LLAMA_API_KEY` (grammar
`crates/lucina-secrets/src/reference.rs:1-11`) mapped to the process variable `RAFFY_API_KEY`. The
precedent for materialising a tailnet address is `deploy/lucina-diagnostics/install.sh:25`, which
substitutes `@TAILNET@` with `tailscale ip -4` into `~/.config/…/diagnostics.env` (template
placeholder at `deploy/lucina-diagnostics/diagnostics.env.example:3`) — so a placeholder in a
`deploy/` template, resolved by the install script, is the established way to name a tailnet IP.
Then `deploy/lucina-rs/install.sh` restarts the unit (`:41`).

---

## How to run the relevant tests

`cargo test` (no `nextest.toml` anywhere). Fast entry point `scripts/check-fast` →
`tools/ci/lucina-ci` (fmt, clippy `--all-targets`, `cargo test --quiet --locked -p <crate> --lib`;
`tools/ci/lucina-ci/src/fast.rs:110-125`, standalone crates via `--manifest-path` `:141-145`).
Merge gate `[merge]` at `.lucina/pipeline.toml:5-30` (`diffgate = true` `:9`, `cargo = "check"` `:10`). There is no Rust CI job on GitHub
(`docs/ci/SHIFT-LEFT.md:1-3` — "There is no Rust job on GitHub for this repository"; `.github/workflows/verus.yml` is Verus only). For a provider change:

```
scripts/check-fast --base origin/main
cargo test -p lucina-llm --test deepseek --test mistral
cargo test -p lucina-server --test model_try
```

(Integration tests are separate binaries, so `--lib` alone will not run them.)

## What is, and is not, in this repo

The provider *code*, the *dispatch* and the *test gates* are all here, as above. The provider's
**live settings** are not: they are in `~/.config/lucina-rs/lucina.env` / `mode.env` and the
`lucina-rs.service.d/70-infisical.conf` drop-in named in `docs/design/mistral-provider.md:17-19` —
all on nezopt, all outside this workspace, and written by no deploy step
(`deploy/lucina-rs/install.sh:7-9`). Numeric per-model limits are not in this repo at all, and a
per-model context-window / tool-support registry does not exist in Rust — see §5.
