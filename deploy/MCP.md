# Lucina MCP (`secretary-mcp`)

A remote [MCP](https://modelcontextprotocol.io/specification/2026-07-28) server that lets
**Claude Code**, **ChatGPT** and **Grok** talk to Lucina and use her Secretary tools.
It runs on p1 next to secretary-server and calls only secretary-server, never Lucina directly.

```text
client ──HTTPS──▶ Caddy (getsecretary.tech) ──▶ secretary-mcp 127.0.0.1:8810 ──▶ secretary-server 127.0.0.1:8800 ──▶ Lucina
                    /mcp, /oauth/*, /.well-known/oauth-*        (own device token "lucina-mcp")
```

- **Transport.** Streamable HTTP at `https://getsecretary.tech/mcp`, built with the official Rust SDK
  `rmcp` 3.5.1 (pinned; published 2026-10-05). It speaks MCP **2026-07-28**, the current spec revision
  (stateless, with `server/discover`), and the older revisions down to 2024-11-05 for clients that still
  send `initialize`.
- **Stateless.** The server runs stateless with JSON responses: there is no `Mcp-Session-Id`, so a
  restart never strands a client. A response switches to SSE only to carry `ask_lucina` progress.
- **Two kinds of auth.**
  - `Authorization: Bearer mcp_…`: service tokens for Claude Code and Grok.
  - OAuth 2.1 for ChatGPT, with access tokens `mcpa_…`.

  Each token has a set of scopes, and every tool checks them.

## Tools, resources, prompts

| Tool | Scope | What it does |
|---|---|---|
| `ask_lucina {message, conversation?}` | `chat` | One Lucina turn (`POST /v1/chat`); returns her reply. See the note below the table. |
| `lucina_timeline {limit?, before?, channel?}` | `read` | Recent messages on all channels, newest first; paginate with `before`. |
| `shopping_list {list?}` | `read` | v2 list state: needed items with ids, recently bought, other lists. Falls back to legacy `/v1/shopping` when v2 answers 404/502. |
| `shopping_add {items: [name \| {name, spec?, quantity?, unit?}], list?}` | `write` | v2 `add` ops (new UUID `opId`s); legacy `POST /v1/shopping` as fallback. |
| `shopping_mark_bought {item, list?}` | `write` | The item (id, or name matched case- and accent-insensitively) gets a v2 `purchase` op; legacy `PATCH {status: purchased}` as fallback. |
| `reminders_list {scope?, include_sent?}` | `read` | `GET /v1/reminders`, times in the owner's zone. |
| `reminder_create {text, when}` | `write` | `when` as RFC 3339 sends `{text, dueAt}`. Anything else sends `{naturalText: "<when> — <text>"}` to Lucina's parser. |
| `reminder_cancel {id}` | `write` | `DELETE /v1/reminders/{id}`: status becomes `cancelled`. The record is kept. Annotated `destructiveHint`. |
| `calendar_list {from?, to?, days?}` | `read` | `GET /v1/calendar/events`: Google, iPhone and ICS events merged, times in the owner's zone, with ids. Default: 7 days from today. |
| `calendar_create {title, start, end?, location?, description?}` | `write` | **Never writes.** Files a proposal (`POST /v1/calendar/proposals`, `from: mcp:<caller>`) and answers that it needs owner confirmation in the app (Links → Waiting for your OK). |
| `gym_summary {workouts?}` | `read` | `GET /v1/insights/summary` (service token). Without it, `GET /v1/gym/state`. |
| `body_summary` | `read` | `GET /v1/body/state`: current weight, 7-day average, trends, goal, latest entries. |
| `note_save {title, content, slug?, tags?, pinned?, replace?}` | `write` | `POST /v1/notes` with `source: <caller>`. An existing slug is refused unless `replace: true` (then `PATCH`). Up to 256 KB of Markdown. |
| `note_append {slug \| id, content, continuation?}` | `write` | `POST /v1/notes/{key}/append`: a new paragraph (`separator: "\n\n"`), or glued on exactly with `continuation: true` (chunked uploads). |
| `note_get {slug \| id, offset?, max_chars?}` | `read` | `GET /v1/notes/{key}`: title, tags, source, update time and the content, paged (40 000 characters by default). |
| `notes_search {q?, tag?, limit?}` | `read` | `GET /v1/notes?q=&tag=`: slug, title, source and an excerpt per hit; without `q`, the latest notes. |
| `lucina_status` | `read` | Server and Lucina health, whether the backend token is accepted, owner time zone and local time, this caller's scopes. |

**`ask_lucina` details.** The message reaches Lucina prefixed `[via Lucina MCP, from <caller>] `, so
she knows who is asking. Each caller gets its own session `mcp-<caller>[-<conversation>]`. Progress
notifications go out every 10 s while she thinks. Messages are limited to 12 000 characters (Lucina
accepts 12 000 after the prefix; secretary-server 12 100 with it).

**Notes.** Long documents, briefings and anything worth keeping go to notes, not to `ask_lucina`
(the tool descriptions and the server instructions say so). A note shows up in the Secretary app
(Notes, web and iOS) with the caller as its source, and Lucina recalls it by slug ("cosa c'è nella nota
craftit-meeting"). For a document too long for one call, `note_save` the first part and
`note_append` the rest in order. Notes are the owner's own local data, so `note_save` and
`note_append` need only the `write` scope and no confirmation; each write is also logged on
`secretary_mcp::audit` (`event = "note_write"`, slug, id, size, caller).

**Resources** (`read`):

- `lucina://timeline/recent`: the last 20 timeline messages.
- `lucina://shopping/default`: the default list.

**Prompt** (`read`): `daily_briefing {date?}`. It builds the briefing from `GET /v1/agenda`. If the
agenda is unavailable, it falls back to today's reminders.

`tools/list`, `resources/list` and `prompts/list` show only what the caller's scopes allow.

**AI client status.** With `SECRETARY_MCP_STATUS_TOKEN_FILE` set, `GET /internal/status` (Bearer that token; 404 otherwise; not routed by Caddy) lists callers, how they connect, scopes, created/last-used times and whether they are active, never ids or hashes. secretary-server reads it for the Links tab.

**Safety.**

- No tool buys anything, messages third parties, or deletes data. There are no shopping `remove`, list
  delete, note delete, gym/body delete or post tools. `note_save` only overwrites a note with an explicit
  `replace: true`.
- Anything with consequences goes through `ask_lucina`, so it is handled by Lucina's own pipeline and
  her own confirmation flow.
- Every call is logged on target `secretary_mcp::audit` with tool, caller, auth kind, token or grant
  id, duration and ok. Arguments, bodies and tokens are never logged.

## Install on p1

### 1. Build (on nezopt)

```bash
cd ~/Projects/SecretaryIOS
CC_x86_64_unknown_linux_musl=gcc \
  cargo build -j2 --release --target x86_64-unknown-linux-musl -p secretary-mcp
ldd target/x86_64-unknown-linux-musl/release/secretary-mcp     # "statically linked"
scp target/x86_64-unknown-linux-musl/release/secretary-mcp deploy/secretary-mcp.service \
    deploy/secretary-mcp.env.example deploy/Caddyfile.mcp p1:/tmp/
```

### 2. User, files, backend token (on p1, as root)

```bash
useradd --system --home-dir /var/lib/secretary-mcp --shell /usr/sbin/nologin secretary-mcp
install -m 0755 /tmp/secretary-mcp /usr/local/bin/secretary-mcp
install -d -m 0750 -o root -g secretary-mcp /etc/secretary-mcp
install -m 0640 -o root -g secretary-mcp /tmp/secretary-mcp.env.example /etc/secretary-mcp/secretary-mcp.env

# The MCP's own secretary-server device token (shown once). Revoking it
# (`secretary-server token revoke`) cuts every MCP client off at once.
install -m 0600 -o secretary-mcp -g secretary-mcp /dev/null /etc/secretary-mcp/backend.token
sudo -u secretary env SECRETARY_DATA_DIR=/var/lib/secretary \
  /usr/local/bin/secretary-server token issue --name lucina-mcp | grep '^sec_' > /etc/secretary-mcp/backend.token

# Optional, for gym_summary via insights: the same svc_ token secretary-server uses
install -m 0600 -o secretary-mcp -g secretary-mcp /etc/secretary/service.token /etc/secretary-mcp/service.token
# …then uncomment SECRETARY_MCP_SERVICE_TOKEN_FILE in secretary-mcp.env

install -m 0644 /tmp/secretary-mcp.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now secretary-mcp
systemctl status secretary-mcp
curl -s http://127.0.0.1:8810/healthz                                   # ok
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:8810/mcp   # 401
```

The OAuth login calls secretary-server's `POST /v1/session` with the client IP in `X-Forwarded-For`.
secretary-server must keep `SECRETARY_TRUST_PROXY=1`, which is already the case behind Caddy, so its
per-IP login limit sees the real client.

### 3. Caddy

Paste `deploy/Caddyfile.mcp` into the `getsecretary.tech { … }` block, above the catch-all
`handle { reverse_proxy 127.0.0.1:8800 }`. Also add `resp_headers>Location delete` to the site's log
filter. Then:

```bash
caddy validate --config /etc/caddy/Caddyfile && systemctl reload caddy
curl -s https://getsecretary.tech/.well-known/oauth-protected-resource/mcp
curl -s https://getsecretary.tech/.well-known/oauth-authorization-server
curl -si -X POST https://getsecretary.tech/mcp | grep -i www-authenticate
```

(The routing was checked with `caddy adapt` on p1 against `deploy/Caddyfile.p1` plus the snippet on
2026-10-21. The `/mcp`, `/oauth/*` and `/.well-known/oauth-*` paths go to `:8810`, and everything else
stays on `:8800`.)

## Service tokens (Claude Code, Grok, scripts)

Run the CLI as the service user. The data dir defaults to `/var/lib/secretary-mcp`.

```bash
sudo -u secretary-mcp secretary-mcp token issue --name claude-code --scopes read,chat,write
sudo -u secretary-mcp secretary-mcp token issue --name grok --scopes read,chat
sudo -u secretary-mcp secretary-mcp token list
sudo -u secretary-mcp secretary-mcp token revoke grok        # by name (all its tokens) or by tok_… id
```

- **Format.** A token is `mcp_` followed by 43 base64url characters. It is printed once.
  `tokens.json` (0600) keeps only its SHA-256.
- **Names.** The name (`a-z 0-9 -`) is what Lucina and the logs see.
- **Default scopes.** `read,chat`. Add `write` only for clients that should edit lists and reminders.
- **Revoking.** Issue and revoke work on the running server at once, with no restart.

## Client setup

### Claude Code

Add Lucina once, in user scope (every project) ([docs](https://code.claude.com/docs/en/mcp)):

```bash
read -rs LUCINA_TOKEN      # paste the mcp_… token; keeps it out of the shell history
claude mcp add --transport http --scope user lucina https://getsecretary.tech/mcp \
  --header "Authorization: Bearer $LUCINA_TOKEN"
unset LUCINA_TOKEN
claude mcp list            # lucina … ✓ Connected
```

In a session, `/mcp` shows the server and its 11 tools. The token is stored in `~/.claude.json`.

Claude Code can also do OAuth (`/mcp` → Authenticate). Its callback is `http://localhost:<port>/callback`,
though, which this server's https-only `redirect_uri` policy refuses on purpose. Use the bearer header.

### ChatGPT (custom MCP connector)

1. In ChatGPT on the web, open **https://chatgpt.com/plugins**, press **+**, then **Add custom MCP server**.
2. Enter the name `Lucina` and a description.
3. Under **Connection**, enter the URL `https://getsecretary.tech/mcp`.
4. Under **Authentication**, choose **OAuth**. ChatGPT discovers everything from the 401 challenge and
   the metadata, so no client id or secret is needed.
5. Select **I understand and want to continue**, then **Create as a plugin**.
6. The Lucina sign-in page opens on `getsecretary.tech`. Sign in with the **Secretary owner email and
   password** (the same as the web app). Choose the permissions (`read`, `chat`, `write`) and press
   **Allow**.

Older accounts showed the same flow under Settings → Apps & Connectors → Advanced → **Developer mode** →
Create. Per OpenAI's help center, developer mode is no longer required first.

Sources:

- [Connect from ChatGPT](https://developers.openai.com/plugins/deploy/connect-chatgpt)
- [Connecting custom MCP servers in ChatGPT](https://help.openai.com/en/articles/12584461-connecting-custom-mcp-servers-in-chatgpt)

**Plans.** These come from OpenAI's help center, checked 2026-10-21. The help pages only showed up in
search snippets, so re-check before relying on them.

| Plan | Custom MCP connectors |
|---|---|
| Business, Enterprise, Edu | Full MCP including write actions ("rolling out in beta"). On Business, only admins and owners can create connectors, through the **Create plugins with MCPs** workspace permission. On Enterprise and Edu, admins can grant it with RBAC. |
| Pro | Read/fetch only, in developer mode. |
| Plus | No current official statement was found. |
| Free | Not supported. |

Sources:

- [Connecting custom MCP servers in ChatGPT](https://help.openai.com/en/articles/12584461-connecting-custom-mcp-servers-in-chatgpt)
- [Admin controls, security and compliance for plugins and apps](https://help.openai.com/en/articles/11509118-admin-controls-security-and-compliance-for-plugins-and-apps)

**What happens underneath.** Per the [Apps SDK auth docs](https://developers.openai.com/apps-sdk/build/auth):

1. ChatGPT reads `/.well-known/oauth-protected-resource/mcp` and `/.well-known/oauth-authorization-server`.
2. It uses CIMD, which ChatGPT prefers: its client id is `https://chatgpt.com/oauth/client.json`, which
   this server fetches and checks. Dynamic registration at `/oauth/register` is the fallback.
3. It runs the authorization code flow with PKCE S256 and `resource=https://getsecretary.tech/mcp`.
4. Because the server returns `iss` (RFC 9207), ChatGPT uses its stable callback
   `https://chatgpt.com/connector_platform_oauth_redirect`. The per-connector fallback
   `https://chatgpt.com/connector/oauth/{callback_id}` is allowed too.
5. ChatGPT gets a 1 h access token and a 30 d refresh token, and refreshes on its own. Refresh tokens
   rotate on every use.

**Managing grants.**

```bash
sudo -u secretary-mcp secretary-mcp oauth list
sudo -u secretary-mcp secretary-mcp oauth revoke <grant_…|client_id|all>
```

### Grok

**xAI API (remote MCP tools).** Grok's Responses API calls remote MCP servers itself
([docs](https://docs.x.ai/docs/guides/tools/remote-mcp-tools)). Issue a token named `grok`:

```bash
curl https://api.x.ai/v1/responses \
  -H "Authorization: Bearer $XAI_API_KEY" -H "Content-Type: application/json" \
  -d '{
    "model": "grok-4.7",
    "input": "Ask Lucina what is on my shopping list.",
    "tools": [{
      "type": "mcp",
      "server_url": "https://getsecretary.tech/mcp",
      "server_label": "lucina",
      "server_description": "The owner'\''s personal AI, Lucina",
      "authorization": "mcp_…grok token…",
      "allowed_tools": ["ask_lucina", "shopping_list", "reminders_list", "lucina_timeline"]
    }]
  }'
```

- **`authorization`.** xAI puts this value in the `Authorization` header. This server accepts it with
  or without the `Bearer ` prefix.
- **`allowed_tools`.** Optional; omit it to allow every tool.
- **`require_approval`, `connector_id`.** xAI does not support these yet.

With the Python `xai_sdk`:

```python
from xai_sdk.tools import mcp
chat = client.chat.create(model="grok-4.7", tools=[mcp(
    server_url="https://getsecretary.tech/mcp", server_label="lucina",
    authorization=GROK_LUCINA_TOKEN,
    allowed_tool_names=["ask_lucina", "shopping_list", "reminders_list"])])
```

**The owner's grok-bot (`/home/nez/Projects/grok-bot-cli`, `gbot`).** It runs on nezopt and is
registered in Claude Code through `~/.local/bin/nezopt-mcp` as
`node ~/Projects/grok-bot-cli/dist/mcp/mcp-grok-bot-*.mjs`.

- **What it is.** An MCP *server* in the other direction: Claude Code and Codex use it to talk to Grok
  Bot (`gbot_send`, `claude_send`, `codex_send`, …, in `src/mcp/grok-bot/tools`).
- **No place to register Lucina.** It has no MCP-client registry, so nothing gets added to that repo.
  The only server it bundles is `claude-channel` (`agent-bundle.config.ts`).
- **How to give Grok Bot agents Lucina.** Add Lucina as a **custom remote MCP server in the Grok Bot
  desktop app** with URL `https://getsecretary.tech/mcp` and header
  `Authorization: Bearer <mcp_… grok token>`. In Cursor-style JSON:
  `{"lucina": {"url": "https://getsecretary.tech/mcp", "headers": {"Authorization": "Bearer mcp_…"}}}`.
- **Unverified UI.** The app (0.68.1 on nezopt) has an MCP settings tab and hosted-MCP OAuth support,
  according to its feature flags in `~/.config/Grok Bot/`. I could not inspect the UI itself, so check
  the exact menu.
- **Use the bearer header, not OAuth.** Grok Bot's OAuth callback is not on this server's
  `redirect_uri` allowlist.
- **Alternative.** Grok Bot agents can already reach nezopt through Grok Bot Shell (`machineId`), and
  from there Claude Code once Lucina is registered there.

## Auth design

- **Bearer service tokens.**
  - 256-bit random, stored as SHA-256 in `tokens.json` (0600) under an exclusive lock, with atomic
    writes.
  - The server reloads the file when it changes (mtime, size or inode), so the CLI and the running
    service share it safely.
- **OAuth 2.1 authorization server.** Built into secretary-mcp, single owner.
  - **Discovery.** RFC 9728 protected-resource metadata, at both the path-inserted and root URLs, and
    RFC 8414 server metadata.
  - **Clients.** CIMD is limited to the `https://chatgpt.com/oauth/` prefix: no redirects, at most
    32 KiB, cached 1 h, public client only. RFC 7591 dynamic registration is the fallback: 10 per hour
    per IP, at most 50 clients, `none` / `client_secret_post` / `client_secret_basic`.
  - **`redirect_uri`.** Must be https, have no userinfo or fragment, and exactly match the client's
    registration *and* the allowlist. By default the allowlist holds ChatGPT's two callbacks.
  - **Authorization.** Code flow only. PKCE S256 is mandatory (`plain` and missing PKCE are refused),
    `iss` is in every response, and RFC 8707 `resource` must be `https://getsecretary.tech/mcp`. Tokens
    are bound to that resource.
- **Login.** The consent page is plain HTML with no script, a strict CSP and `frame-ancestors 'none'`.
  - **Checking the password.** The owner's email and password are checked by calling secretary-server's
    `POST /v1/session`. The session it creates is closed straight away with `DELETE /v1/session`.
  - **Why the backend, not `owner.json`.** Reading `owner.json` read-only would split the lockout
    counter and duplicate the Argon2 code. It would also force this service to run as the `secretary`
    user or loosen that 0700 state dir. Going through the backend keeps one Argon2 store, one lockout
    (10 failures → 15 min, doubling) and one per-IP limit, and a password change applies immediately.
  - **Rate limits.** On top of the backend's: 5 attempts per 5 min per IP, and 20 per 15 min globally.
  - **Consent phishing.** ChatGPT's callback is shared by every ChatGPT user, so a sign-in link someone
    else started would hand their connector your tokens. The page says to continue only for a
    connection you just started yourself. `oauth list` / `oauth revoke` undo a mistake.
- **Tokens.**
  - Access tokens (`mcpa_`, 1 h) and refresh tokens (`mcpr_`, 30 d) are opaque and stored as SHA-256 in
    `oauth.json` (0600).
  - Refresh tokens rotate on use. Replaying a rotated refresh token, or redeeming an authorization code
    twice, revokes the whole grant.
  - The consent can narrow the scopes, and so can a refresh (`scope=read`).
- **Scopes.** `read`, `chat` and `write`, checked in every tool, resource and prompt. A missing scope
  is a tool error, and for OAuth callers it carries ChatGPT's `_meta["mcp/www_authenticate"]`
  `insufficient_scope` challenge. A 401 carries
  `WWW-Authenticate: Bearer resource_metadata="…/.well-known/oauth-protected-resource/mcp", scope="read chat write"`.
- **Host and Origin checks.** They come from rmcp. Hosts: `getsecretary.tech` and localhost. Origins:
  the site, `https://chatgpt.com` and `https://claude.ai`. A request without an `Origin` passes.
- **Backend credentials.** secretary-mcp never forwards a client's token. It calls secretary-server
  with its own device token (`lucina-mcp`), plus the service token for insights only.
- **Shared rate limit.** All MCP clients share that device's 60 requests/min bucket on secretary-server.

## Open TODOs

- **secretary-server.** Whitelist `X-Secretary-Client: mcp` and the `channel: mcp` value. Today the MCP
  sends `web` plus an ignored `meta: {via: "mcp", caller}`, so MCP turns show as `web` in the timeline.
  Then switch `backend::chat` to `mcp`.
- **Lucina.** Treat `channel: mcp`, or the `[via Lucina MCP, from …]` prefix, as needing explicit owner
  confirmation for `social_post_task` and any message to third parties. Per Lucina's contract,
  `social_post_task` publishes directly today.
