# Lucina secrets skill

Lucina reads secrets from the owner's self-hosted Infisical
(`https://secrets.pokoin.com`, on lubmipc) inside her pipeline and never shows
one in clear text: not in replies, prompts, logs, the timeline, Honcho,
Telegram/Discord/app messages or MCP responses.

Code: `crates/lucina-secrets` (Rust), wired into `lucina-tools`,
`lucina-store`, `lucina-memory` and `lucina-server`. The Node Lucina has the
same redactor behind `LUCINA_REDACT=true` (`src/secret-redaction*.js`).
Owner script: `deploy/lucina-secrets/store-identity.sh`.

## Rules

1. **The model never sees a value.** Lucina reasons with DeepSeek, an
   external service. Tools and config name secrets by reference,
   `secret://<project>/<env>/<path>#<KEY>`. The value is resolved inside the
   tool, right before its outbound call (an HTTP header or query parameter,
   a child process's environment or stdin), and never returned.
2. **No reveal path.** No tool or intent returns a value. "Dimmi la password
   di X", "show me the token", "vis mig adgangskoden" get a fixed refusal
   (Italian, English, Danish), decided by code, not by the model. Lucina may
   say which secrets exist, whether they are set, when they last changed and
   who or what used them.
3. **One redactor in front of every sink** (table below).
4. **Encrypted at rest, minimal in memory.** The Infisical machine identity
   is a systemd credential encrypted with the TPM2. Values live in memory
   only (`secrecy`, `zeroize`, a 10-minute TTL cache), never on disk.
5. **Least privilege.** One read-only machine identity on one Infisical
   folder (`lucina/prod/lucina`); Lucina enforces the same scope before any
   call.
6. **sudo is out of scope** (see [sudo guidelines](#sudo-guidelines)).
7. **Audit.** Every resolve is logged with the secret's name, the tool, the
   time and the outcome, never the value.

## Architecture

```
tool (e.g. weather)
  └─ ctx.secret("weather.api_key", "weather")          lucina-tools Ctx::secret
       └─ SecretStore::resolve(ref, tool)              lucina-secrets
            1. policy: scope, hard deny (login/sudo/root), deny terms
            2. TTL cache (10 min, zeroized on every exit)
            3. Infisical: Universal Auth token (renewed at 80 % of its TTL,
               fresh login on 401) -> GET /api/v4/secrets/{KEY}
            4. minimum length (8): shorter values cannot be redacted safely
            5. redactor learns the value (digests only)
            6. audit line (name, tool, time, ok/cached/error code)
       └─ SecretValue -> the outbound request only
  └─ tool result -> redactor -> model / timeline / Honcho
```

### Crate `lucina-secrets`

| module | what |
|---|---|
| `reference` | `SecretRef` parser/printer. `secret://lucina/prod/lucina/github#GITHUB_TOKEN` = key `GITHUB_TOKEN` in folder `/lucina/github`, env `prod`, project alias `lucina`. |
| `infisical` | `POST /api/v1/auth/universal-auth/login`, `POST /api/v1/auth/token/renew`, `GET /api/v4/secrets/{name}` (v3: `/api/v3/secrets/raw/{name}`), `GET /api/v4/secrets?viewSecretValue=false&recursive=true` for names. Bodies that hold values are parsed from a buffer that is zeroized afterwards. |
| `cache` | `TtlCache`: every value leaving it (expired, replaced, removed, cleared, dropped) is zeroized explicitly. |
| `value` | `SecretValue`: no `Display`, no `Serialize`, `Debug` prints `SecretValue([redacted])`, zeroized on drop. |
| `redact` | The redactor (below). Process-wide: `lucina_secrets::redactor()`. |
| `reveal` | Reveal / status / audit classification and the fixed texts (it/en/da). |
| `policy` | Scope check, `HARD_DENY` (not configurable), `DEFAULT_DENY` (configurable), `allow_names`. |
| `audit` | JSON lines, 0600, plus the recent entries in memory. |
| `store` | `SecretStore`: resolve, status, audit, preload. |
| `outbound` | Sensitive `HeaderValue`s, child env, child stdin. |
| `log` | `RedactingMakeWriter` for `tracing_subscriber`. |
| `status` | Fixed owner-facing texts for status and audit. |
| `config` | `secrets.toml` (`LUCINA_SECRETS_CONFIG`), see `deploy/lucina-secrets/secrets.example.toml`. |

Without the `client` feature the crate is only the redactor and the reveal
rules: that is what `lucina-store` and `lucina-memory` link.

### The redactor

1. **Known values.** Every resolved secret, every value in Lucina's folder
   (preloaded at start, `redaction.preload`), the machine identity, the
   Infisical access tokens, and Lucina's bootstrap credentials
   (`SECRETARY_API_TOKEN`, the Node token, the DeepSeek key, the Honcho key,
   the secretary-server service token, the DSH token, the grocery database
   password). Each is registered raw, JSON-escaped (once and twice),
   URL-encoded (upper/lower hex, form) and in base64 at all three byte
   alignments in both alphabets, so `base64("user:" + value)` is caught.
   Matches become `[secret:<NAME>]`.

   The redactor keeps **no values**: per encoding a length, a Rabin-Karp
   rolling hash with a random base, and a SHA-256 digest with a random
   per-process salt. A scan slides one window per distinct length and
   confirms hits with the digest. Once seen, a value stays masked for the
   life of the process, also after a rotation.
2. **Detector** for secrets nobody registered: `sk-…`, `sk_live_…`, `ghp_…`,
   `github_pat_…`, `glpat-…`, `mcp_…`, `sec_…`, `svc_…`, Slack, AWS, Google,
   Telegram bot tokens, Infisical service tokens, JWTs, PEM blocks (also
   unterminated private keys), `Bearer …`/`Basic …`, `password=…`,
   `"token": "…"`, `?apikey=…`-style URL parameters, and 32+ character
   strings with lower and upper case, 3+ digits and entropy ≥ 4.2 bits/char.
   ULIDs, UUIDs, hex digests, paths and `secret://` refs pass.

### Sinks

| sink | where it is redacted |
|---|---|
| model prompts and replies (DeepSeek) | `lucina-server::secrets::RedactingModel` wraps every `ChatModel` (text, reasoning, tool-call arguments) |
| tool results | `lucina_tools::agent::execute`, before the model, the timeline and Honcho see them |
| API replies (app, web, Lucina MCP `ask_lucina`) | `/api/secretary/chat` output (`api.rs`) |
| timeline file and live stream | `TimelineStore::append` |
| recent turns (prompt context, file) | `RecentChat::append` |
| job results and errors | `JobStore::insert` / `update` |
| Honcho writes and queries | `HonchoClient::post`, the context search query |
| logs (journald) | `RedactingMakeWriter` in `lucina serve` |
| app notifications | the `notify` tool payload |
| Telegram, Discord, Node logs | Node Lucina, `LUCINA_REDACT=true` (below) |

The Rust Lucina has no Telegram or Discord sender yet; when it gets one, the
outgoing text goes through `lucina_secrets::redactor().redact()` like the
notify payload.

### Turns

- **Reveal request** (any channel, MCP included): fixed refusal, action
  `secrets_refused`, no model call, no Honcho context read.
- **Status / audit** ("quali segreti ci sono?", "who used the GitHub
  token?"): a fixed-template answer from Infisical metadata and the audit
  log, no model call. Lucina MCP callers get a refusal instead.
- **Tools mode** (`LUCINA_PLANNER=tools`): `secrets_status` and
  `secrets_audit` are tools for the owner. They are not registered on Lucina
  MCP turns (`lucina_tools::OWNER_ONLY_TOOLS`, `mcp_registry`).

## How a tool uses a secret

1. The value goes in Infisical: project `lucina`, env `prod`, folder
   `/lucina`, key `OPEN_METEO_API_KEY`.
2. `~/.config/lucina-rs/secrets.toml` maps a tool setting to it:

   ```toml
   [refs]
   "weather.api_key" = "secret://lucina/prod/lucina#OPEN_METEO_API_KEY"
   ```
3. The tool asks for it by setting name, uses it for its own request, and
   returns only what the API answered (`crates/lucina-tools/src/tools/weather.rs`):

   ```rust
   // Resolved right before the call (cached at most 10 minutes), never returned.
   let api_key = ctx.secret("weather.api_key", "weather").await?;
   let mut request = ctx.env.http.get(url).query(&[/* ... */]);
   if let Some(key) = &api_key {
       request = request.query(&[("apikey", key.expose_secret())]);
   }
   let resp = request.send().await?;
   ```

   For a bearer token: `.header(AUTHORIZATION, lucina_secrets::outbound::bearer(&token)?)`
   (a sensitive header value). For a child process:
   `outbound::child_env(&mut cmd, "GITHUB_TOKEN", &token)` or
   `outbound::stdin_secret(&mut child, &token)`.
4. `Ok(None)` means no ref is configured for that setting (the tool works
   without a key); an error (`denied`, `not_found`, `unauthorized`, ...)
   reaches the model as a tool error that names the setting, never a value.

Candidates: the weather and places tools if keys are ever needed, email and
GitHub sources for the proactive engine, Todoist if its token moves from
secretary-server to Infisical.

## Threat model

**Assets**: the values in Lucina's Infisical folder; the machine identity
(client id and secret) that can read them; the bootstrap credentials.

| threat | mitigation | residual |
|---|---|---|
| The model provider (DeepSeek) logs or trains on prompts | values never enter a prompt: tools resolve them after the model decided, results and prompts are redacted | a secret in an unknown format pasted by the owner and missed by the detector |
| Prompt injection (a web page, an email, a tool result tells Lucina to print a key) | the model cannot print what it never saw; there is no tool that returns a value; tool results are redacted before the model | an injected instruction can still make a tool *use* a secret against an endpoint the tool already calls (tools have fixed endpoints) |
| Owner or anyone with the owner's chat asks for a value | fixed refusal by code, also on MCP | none: the value is in Infisical only |
| An MCP client (ChatGPT, Claude Code) probes Lucina | MCP turns get refusals for secrets questions and no secrets tools; replies are redacted | — |
| Logs, timeline, Honcho, notifications, Telegram/Discord copy a value | every sink is redacted (table above); tests inject a secret into a tool output and scan every sink | short values (< 8 characters) are refused, not redacted |
| Disk theft, backups, a copied home directory | the identity is TPM2-sealed (`systemd-creds`), values never touch disk, the audit log holds names only | the redactor's digests live in memory only |
| Another process running as `nez` | — | it can read Lucina's memory or unseal the TPM credential like the service does; secrets are only as safe as the `nez` account |
| A compromised Lucina host | read-only identity on one folder: the owner's other secrets are not reachable; revoke the client secret in Infisical | values in that folder |
| Login, sudo, root passwords | `HARD_DENY` refuses them by name or path, before any network call | a password stored under a misleading name; do not put such passwords in Lucina's folder |
| Memory forensics after use | `secrecy`/`zeroize` on our buffers, 10-minute cache | the HTTP stack's transient buffers (reqwest/hyper/rustls) are not zeroized |

The Node redactor learns values from its own environment (it has no
Infisical access); it keeps the patterns as strings (JavaScript cannot
zeroize).

## sudo guidelines

- Lucina never gets, stores or uses the owner's sudo or login passwords, not
  even from Infisical. The secrets skill refuses to resolve any secret whose
  name or path looks like one (`sudo`, `root`, `login`, `passwd`,
  `shadow`, `unix`, `ssh_pass`, `*_password` of an account), and that list
  cannot be configured away.
- Root operations on nezopt go only through the scoped NOPASSWD sudoers
  allowlist the owner manages, `/etc/sudoers.d/claude-nez` (today:
  `/usr/sbin/ufw` and `/usr/bin/journalctl`).
- When Lucina needs a root action, she proposes the allowlist line for the
  owner to add, scoped to the exact command and arguments, e.g.

  ```
  nez ALL=(root) NOPASSWD: /usr/bin/systemctl restart caddy.service
  ```

  never a shell, an editor, a wildcard command, `ALL`, or a tool that can
  run other commands (`env`, `find -exec`, `tee` on arbitrary paths, ...).
  The owner reviews and adds it with `sudo visudo -f /etc/sudoers.d/claude-nez`.
- `lucina-rs` runs with `NoNewPrivileges=yes`: it cannot use sudo at all.
  Root actions are proposals for the owner, not something the service does.

## Owner steps

You do these: they involve credentials and admin consoles. Nothing here is
done by the code or by an agent.

### 1. Infisical: a folder and a read-only machine identity

On `https://secrets.pokoin.com`:

1. **Project.** Create a project `Lucina` (or pick the one Lucina should
   use). Copy its **Project ID** (Project Settings → General).
2. **Folder.** In environment **Production** (slug `prod`) create the folder
   `/lucina`. Lucina's secrets go there and nowhere else.
3. **Identity.** Organization → Access Control → Identities → Create
   identity `lucina-nezopt`, organization role **No Access**.
4. **Universal Auth.** On the identity: Authentication → Universal Auth:
   Access Token TTL `3600`, Max TTL `86400`, Trusted IPs: the address
   lubmipc sees for nezopt (LAN or Tailscale) if you want to pin it.
   Create a **Client Secret** (optionally with an expiry for rotation).
   Keep the page open: the client secret is shown once.
5. **Read-only on the folder.** Project `Lucina` → Access Control → Roles →
   create role `lucina-read`: Secrets → **Read Value** and **Describe
   Secret** only (no create, edit, delete), conditions *Environment* =
   `prod` and *Secret Path* glob `/lucina/**` (add `/lucina` itself too);
   Secret Folders → read, same conditions. Then Access Control → Machine
   Identities → Add `lucina-nezopt` with role `lucina-read`. Do not add the
   identity to any other project.

### 2. nezopt: store the identity encrypted (one command)

One-time, if `nez` cannot use the TPM yet (the script tells you):

```sh
sudo usermod -aG tss nez
sudo systemctl restart user@1000.service   # or log out/in or reboot; restarts lucina-rs and lucina-presence
systemd-creds has-tpm2                       # should now say "yes"
```

Then, in your own terminal:

```sh
/home/nez/Projects/SecretaryIOS-secrets/deploy/lucina-secrets/store-identity.sh
```

It asks for the client ID and the client secret with `read -s` (nothing is
echoed, nothing goes to argv, history or disk in clear), encrypts each with
`systemd-creds encrypt --with-key=tpm2` into
`~/.config/lucina-rs/credstore.encrypted/`, checks the round trip without
printing it, and writes the drop-in
`~/.config/systemd/user/lucina-rs.service.d/secrets.conf`:

```ini
[Service]
LoadCredentialEncrypted=lucina-infisical-client-id:/home/nez/.config/lucina-rs/credstore.encrypted/lucina-infisical-client-id.cred
LoadCredentialEncrypted=lucina-infisical-client-secret:/home/nez/.config/lucina-rs/credstore.encrypted/lucina-infisical-client-secret.cred
Environment=LUCINA_SECRETS_CONFIG=%h/.config/lucina-rs/secrets.toml
```

At start systemd decrypts them into the unit's private
`$CREDENTIALS_DIRECTORY` (ramfs, 0400); Lucina reads them from there.

Why TPM2 and not the host key: `lucina-rs` is a **user** unit and nezopt
runs **systemd 255** (checked 2026-10-22). The user manager decrypts
credentials as `nez`; the host key (`/var/lib/systemd/credential.secret`) is
root-only, and per-user credentials (`systemd-creds --user`) arrive only in
systemd 256. So `LoadCredentialEncrypted=` in the user unit works with a
TPM2-sealed credential once `nez` may open `/dev/tpmrm0` (group `tss`).
Plain `LoadCredential=` works in the user unit today (checked), encrypted
host-key credentials do not. The alternative is to move `lucina-rs` to a
system unit with `User=nez` and run the script as root with
`--with-key host+tpm2` (credentials in `/etc/credstore.encrypted`); an
age-encrypted file would need its key somewhere the user unit can read,
which is no better than the TPM.

Then the config (no values in it):

```sh
cp /home/nez/Projects/SecretaryIOS-secrets/deploy/lucina-secrets/secrets.example.toml ~/.config/lucina-rs/secrets.toml
chmod 600 ~/.config/lucina-rs/secrets.toml
# set [projects] lucina = "<the Project ID from step 1.1>"
```

Restart only once a build with the secrets skill is deployed (an older
binary ignores the variable, but a credential that cannot be decrypted
stops the unit from starting):

```sh
systemctl --user daemon-reload && systemctl --user restart lucina-rs
journalctl --user -u lucina-rs -n 50 | grep -i secrets   # "Infisical store ready", "preloaded"
```

### 3. The first secrets

1. Infisical → project `Lucina` → `prod` → folder `/lucina` → **Add
   secret**: name in `UPPER_SNAKE_CASE` (e.g. `OPEN_METEO_API_KEY`), the
   value pasted into Infisical only.
2. If a tool should use it, add its ref to `~/.config/lucina-rs/secrets.toml`:

   ```toml
   [refs]
   "weather.api_key" = "secret://lucina/prod/lucina#OPEN_METEO_API_KEY"
   ```
3. `systemctl --user restart lucina-rs` (the config is read at start). The
   preload teaches the redactor every value in the folder.
4. Ask Lucina "quali segreti ci sono?": the new name shows as set.

## How to add a secret (later)

- Same as step 3. Names must not look like a login, sudo or root password
  (refused) and should not be personal passwords or PINs (refused unless
  listed in `policy.allow_names`). Values shorter than 8 characters are
  refused (they cannot be redacted without masking ordinary text).
- A new tool that needs it: a setting name (`<tool>.<what>`), a `[refs]`
  line, and `ctx.secret("<tool>.<what>", "<tool>")` right before the call.

## Rotation

- **A secret's value**: change it in Infisical (Lucina's identity cannot).
  Lucina uses the new value within the cache TTL (10 minutes) or at once
  after a restart. The old value stays masked for the life of the process.
  `last changed` in the status shows the Infisical `updatedAt`.
- **The machine identity's client secret**: Infisical → identity → Universal
  Auth → create a new client secret; run `store-identity.sh --force`;
  `systemctl --user restart lucina-rs`; check the journal; then revoke the
  old client secret in Infisical.
- **After a suspected leak**: revoke at the provider (GitHub, Todoist, ...)
  first, then put the new value in Infisical; check `secrets_audit` for who
  used the secret.

## Node Lucina (`LUCINA_REDACT=true`)

Branches `secretary-live` and `secretary-shopping-v2` of
`/home/nez/Projects/Lucina`. Off by default; set `LUCINA_REDACT=true` in the
container's env file and restart `lucina-nez` when you decide to.

- `src/secret-redaction.js`: the same redactor (known values from the
  process environment: names with TOKEN, SECRET, PASSWORD, API_KEY, ...;
  passwords inside connection URLs) and the same reveal rules and texts.
- `src/secret-redaction-install.js`, imported first by `src/index.js`:
  Telegraf `Telegram.callApi` payloads (every Bot API call), discord.js
  `MessagePayload` bodies (send, reply, edit, interactions) and
  `console.*`.
- `planLucinaTurn` (Telegram, Secretary app) and `planLucinaResponse`
  (Discord) answer reveal requests with the fixed refusal, without the model.

## Tests

- `cargo test -p lucina-secrets`: refs, policy (scope, login/sudo/root
  denylist, configurable terms), redactor (every encoding, detector shapes,
  ordinary text untouched, no values kept), reveal refusals in it/en/da,
  cache TTL and zeroize on every exit, audit file (0600, no values),
  config, outbound helpers, the log writer; `tests/infisical.rs` (login,
  renewal, re-login on 401, v4 and v3 reads, names without values,
  cache/TTL, policy refusals without network calls, error texts without
  values, preload); `tests/store_identity.rs` (the owner script with dummy
  values and a fake `systemd-creds`: no echo, no argv, no clear text on disk,
  the drop-in).
- `cargo test -p lucina-server --test secrets`: a secret injected into a
  tool's output (raw, base64, URL-encoded) and a token the model "leaks"
  reach no sink (API reply, model requests, timeline, recent turns, Honcho,
  logs, audit); reveal refusals and status without the model in both
  planner modes; MCP turns get refusals and no secrets tools. With the
  tool-result and model redaction disabled the test fails (checked).
- `cargo test -p lucina-store --test redaction`, `-p lucina-memory --test
  redaction`: timeline (file and stream), recent turns, jobs, Honcho.
- Node: `tests/secret-redaction.test.mjs`.
