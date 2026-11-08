# The Juno shell tool (`juno_bash`) for CodeCor's Claude agents

The owner's decision (2026-11-07): Claude's CodeCor agent may have Juno's `juno_bash` as a tool. **Every call
shows an Allow/Deny card to the owner** (agentd's permission book; CodeCor on Juno draws it) and runs only after
Allow. Nothing is auto-allowed. When Lucina is back, each call also goes through `lucina_decide`; until then the
card alone gates it. Never pass credentials through it. Never use `StrictHostKeyChecking=no`; pin the host key.

**Changed 2026-11-08 (owner):** "CodeCor is root by default, everything is allowed, since we made the OS and we are
the only ones on it; we are testing." The `lucina_decide` hook (`deploy/lucina-agentd/juno-decide.sh`) now approves
every call without a phone question and logs it to `~/.local/state/lucina-agentd/juno-bash-audit.log`. To ask on
every call again, create `~/.config/lucina-agentd/juno-decide.ask` (or set `JUNO_DECIDE_ASK=1`). The credential
guard and output redaction are unchanged. The Juno card described below is still enforced by agentd until its own
change lands; the text below describes the 2026-11-07 design.

## Opting in

The agent label `codecor.juno=bash`. agentd attaches the tool only to a **Claude** agent whose mode is one of
`default`, `plan`, `acceptEdits` — the modes where Claude asks before an MCP tool call. Anything else (another
provider, `bypassPermissions`, `dontAsk`, …, or a `toolPolicy` that preapproves any `mcp__juno__*` tool) is refused
at create and at fork with 403 `JUNO_TOOL_REFUSED` ("The Juno shell tool needs a Claude agent in a mode that asks
before each call"), and a later mode change to a forbidden mode is refused the same way (`Manager::set_mode`).

## The tool server: `lagent juno-mcp`

`src/juno_mcp.rs`, a subcommand next to `lagent mcp`, sharing its stdio JSON-RPC loop and protocol version
(`mcp.rs::serve_stdio`). One tool:

- `juno_bash {command: string (1..8000 chars), timeout_seconds?: integer 1..600, default 60}` — runs the command on
  the Juno laptop as root over SSH and returns `{exit_code, stdout, stderr, timed_out, duration_ms}` as text JSON,
  stdout+stderr clipped to 12,000 characters in total (a note when clipped).

The ssh invocation is exactly:

```
ssh -n -i <key> -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes \
    -o UserKnownHostsFile=<known_hosts> -o HostKeyAlias=juno -o ConnectTimeout=8 -o ServerAliveInterval=15 \
    <target> -- <command>
```

with key `~/.config/lucina-agentd/juno_bash_ed25519`, known_hosts `~/.config/lucina-agentd/juno_known_hosts` (a line
`juno ssh-ed25519 …`), and target `$LUCINA_JUNO_TARGET` (default `root@192.168.178.47`). If the key or known_hosts
is missing, or the known_hosts has no `juno` line, the call is refused — never a fallback to other options. The
command is one argv element after `--`, so no `-o` value can be injected by it; the ssh child is killed on timeout.

## Gates on every call

1. **The owner's card** (agentd's permission book): `mcp__juno__*` requests are never preapproved — not by a tool
   policy, not by `is_own_tool` (`manager/permissions.rs`), and "allow always" can never add a rule
   (`updatedPermissions` is dropped for them in `providers/claude.rs::respond_permission`). Every call asks again.
2. **Credentials guard** (before anything runs): a command mentioning `/data/lucinaos/coder/`, `gateway_key`,
   `tunnel_key`, `mistral_key`, `/data/lucinaos/account`, `account.sock`, `/data/lucina/secretary`, `wifi.cfg`,
   `.ssh/`, `authorized_keys`, `/etc/shadow`, `/environ` (i.e. `/proc/*/environ`), `printenv`, `env ` at a word
   start, or `secret` is refused with an error.
3. **`lucina_decide`** (when Lucina is back): if `~/.config/lucina-agentd/juno-decide` exists and is executable,
   it runs before each call with `{"command", "agent_id", "timeout_seconds"}` on stdin (agent id from
   `$LUCINA_AGENT_ID`) under a 330 s limit; the command runs only if it exits 0 with first stdout line `approve`,
   otherwise its message is the refusal. Without the hook the call goes on (the card already gated it) and the
   audit records `decide: not configured`.
   The hook is `deploy/lucina-agentd/juno-decide.sh` (installed on nezopt since 2026-11-07:
   `install -m 0755 deploy/lucina-agentd/juno-decide.sh ~/.config/lucina-agentd/juno-decide`). It asks
   `lucina-rs decide` "Run it" / "Don't run it" (impact medium, not reversible, topic `codecor-juno-bash`) and
   waits up to 300 s. Lucina passes such a question to the owner's phone, so a call needs both his card on Juno
   and his answer there. Only "Run it" approves; a question still pending at 300 s is withdrawn and the call
   refused, and so is any failure of `lucina-rs`.
4. **Output redaction** (before clipping): `sec_[A-Za-z0-9_-]{16,}`, `sk-[A-Za-z0-9]{20,}` and
   `-----BEGIN … PRIVATE KEY-----` … `-----END … KEY-----` blocks become `[redacted]`.

## Audit

One JSON line per call (refused or run) in `~/.local/state/lucina-agentd/juno-bash.jsonl`: `ts`, `agent_id`,
`command`, `decide`, `exit_code`, `timed_out`, `duration_ms` — never the output.

## Files

- `crates/lucina-agentd/src/juno_mcp.rs` — the server, the ssh argument list, the guards, redaction, the decide
  hook, the audit.
- `crates/lucina-agentd/src/mcp.rs` — `serve_stdio`, the shared stdio JSON-RPC loop (`lagent mcp` unchanged).
- `crates/lucina-agentd/src/cli.rs` — the `juno-mcp` subcommand.
- `crates/lucina-agentd/src/provider.rs` — `SessionConfig::juno_tool`.
- `crates/lucina-agentd/src/manager.rs` — the label, `check_juno_tool`, the create/fork/set_mode refusals,
  `ManagerError::JunoToolRefused` (403 `JUNO_TOOL_REFUSED`), `session_config`.
- `crates/lucina-agentd/src/providers/claude.rs` — the second `--mcp-config` server (`juno`) when opted in; juno
  requests never get `updatedPermissions`.
- `crates/lucina-agentd/src/manager/permissions.rs` — `mcp__juno__*` is never preapproved.
- `docs/design/agents-api.md` — the label, the error code, "juno tools always ask".
