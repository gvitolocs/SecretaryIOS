# Memory surfaces

Every Lucina turn is classified **once** by the surface it arrives on. Each class has a fixed read/write
policy for personal memory, and anything that cannot be proven fails closed (`Unknown`: no personal memory,
no writes). The idea comes from oh-my-hermes' memory model (MIT); the code is ours.

- Code: `crates/lucina-memory/src/surface.rs` (`classify`, `may_read`, `may_write`, `le`, `TurnSurface`).
- Verus kernel: `verification/verus/memory_surfaces.rs` (run by `scripts/verify-verus.sh`).
- Differential test (production vs a plain-Rust copy of the kernel, proptest):
  `crates/lucina-memory/tests/verus_surfaces.rs`.

## Trust chain (Secretary API)

```
owner web session / iOS device token ─┐
MCP client ── Lucina MCP OAuth token ─┤ (checked by secretary-mcp on p1, which calls with its device credential)
                                      ▼
secretary-server /v1/chat (authenticates the owner, sets channel web|ios)
                                      ▼
lucina-rs /api/secretary/chat, bearer SECRETARY_API_TOKEN (ct_eq in api::secretary)
```

Only secretary-server holds `SECRETARY_API_TOKEN`, so a request that passes the bearer check is the owner or
one of his MCP delegates. `lucina_server::surface::BearerVerified` is the in-crate proof of that check (only
`api::secretary` builds one). MCP-ness arrives as markers (session `mcp-<caller>…` and/or the
`[via Lucina MCP, from <caller>]` prefix, `lucina_intent::mcp::turn_info`). A marker can only **tighten**
a turn (owner → delegate); it never grants anything without the bearer.

## Classes

| Class | Recognised by |
|---|---|
| `OwnerDirect` | web/iOS with the bearer and no MCP markers; Telegram private chat whose chat id equals `LUCINA_OPERATOR_CHAT_ID` (sender, when given, the same); Discord DM (no guild) whose author equals `LUCINA_OWNER_DISCORD_USER_ID`; Alexa linked user equal to the owner's (not configured yet) |
| `OwnerDelegate{caller}` | MCP markers on web/iOS with the bearer (secretary-mcp sends `web`; the channel is client-declared, so the bearer is the proof and the markers only tighten) |
| `Direct{person}` | 1:1 with someone else: `telegram:<id>`, `discord:<id>`, `alexa:<id>` |
| `Channel{channel}` | Telegram group/supergroup/channel `telegram:<chat>`, Discord guild channel `discord:<channel>`, even when the owner wrote the message |
| `Background{origin, owner_addressed}` | internal work (`proactive`, `scheduler`, `cron`); `owner_addressed`: the output goes to the owner alone |
| `Unknown` | everything else: no bearer, MCP markers without the bearer or off web/iOS, missing chat/author/linked ids, background mixed with channel facts, any other channel, a blank configured owner id |

Ids are compared exactly after trimming; an empty or missing configured owner id never matches.

## Policy

| Surface | read owner | read person p | read channel c | write owner | write person p | write channel c |
|---|---|---|---|---|---|---|
| OwnerDirect | yes | yes | yes | yes | yes | yes |
| OwnerDelegate{caller} | yes | no | no | yes, tagged `via=mcp`, `caller` | no | no |
| Direct{person=q} | no | only p = q | no | no | only p = q | no |
| Channel{channel=d} | no | no | only c = d | no | no | only c = d |
| Background{owner_addressed} | only if owner_addressed | no | no | no | no | no |
| Unknown | no | no | no | no | no | no |

Scopes: `OwnerPersonal` (the Honcho owner peer, and every owner block of the turn context), `Person(<platform>:<id>)`
(any other Honcho peer), `Channel(<platform>:<id>)` (reserved: no Rust surface reads channel memory yet).

`le(a, b)` ("a is at most as permissive as b") is a preorder: `Unknown` is the bottom, `OwnerDirect` the top,
`Background{_, false}` ≤ every non-Unknown class, `Background{_, true}` ≤ owner-addressed background,
`OwnerDelegate`, `OwnerDirect`; `OwnerDelegate`, `Direct`, `Channel` ≤ `OwnerDirect`. `may_read` and `may_write`
are monotone under it, and `TurnSurface::tighten` only moves down (a loosening attempt is refused and logged).

## Enforcement

| Where | What |
|---|---|
| `lucina-memory` `Memory::build_context` / `remember` / `search` | take the `TurnSurface`; the scope comes from the peer (owner peer = OwnerPersonal). Denied read: empty, one info log (class, scope kind, op; no ids or content). Denied write: `MemoryError::Denied`, no Honcho call. Delegate writes add `metadata.via = "mcp"`, `metadata.caller` to the user message (ids, peers, sessions, workspace unchanged). |
| `lucina-server` `chat::handle_chat` | classifies once (`surface::chat_surface`), refuses (403) any class but OwnerDirect/OwnerDelegate before any memory read or timeline write, passes the surface to `run_turn`, `Turn`, `tools_turn` (tools `Origin` from the surface), `context::prepare*`, `remember_turn`, `remember_note`, `spawn_job` → `remember_job` (a job keeps its turn's surface). |
| `lucina-server` `context::prepare_with` | without owner read: no presence, calendar, notes, tasks, shopping, recent turns (also kept from the intent classifier), delivered reminders or Honcho; metadata and the game block stay. Owner and delegate output is unchanged. |
| `lucina-server` `api::agenda` (briefing) | `surface::app_surface` (authenticated app request = OwnerDirect). |
| `lucina-proactive` `HonchoMemory` | `Background{Proactive, owner_addressed: true}`: owner search allowed (its messages go to the owner's app timeline / Telegram chat), writes impossible (the memory is also read-only). |

`Memory::client()` (the raw Honcho client) is not surface-gated; nothing outside lucina-memory's own tests uses it.

Fixed on the way: MCP turns in tools mode (`LUCINA_PLANNER=tools`) were written to Honcho without the MCP
label (`tools_turn` built its remember `Turn` with `mcp: None`); they now carry the caller in the label and
the metadata.

## Not covered yet

- Telegram, Discord and Alexa turns are still served by the Node Lucina (`lucina-nez`); the classifier and
  the tests cover their inputs, enforcement arrives with the port of those entry points.
- Channel scope is reserved: no Rust surface reads or writes channel memory.
- Data compatibility: no stored format changed (`timeline.jsonl`, reminders, grocery Postgres, Honcho ids
  and peers); only Honcho message metadata gains `via`/`caller` on MCP writes.

## Formal verification

Verus (`memory_surfaces.rs`, 34 verified): S1 Unknown and Channel never read or write the owner scope;
S2 Background never writes; S3 missing/contradictory inputs give Unknown, MCP markers never give OwnerDirect,
a Discord guild message or a Telegram group is never OwnerDirect, OwnerDirect needs a positive proof;
S4 `le` reflexive, transitive, bottom/top, `may_read`/`may_write` monotone; S5 `tighten` never loosens.

No Stateright model: classification is a pure function of one turn's inputs and the policy a pure function
of (surface, scope); there is no protocol between processes (a spawned job carries an immutable copy of its
turn's surface).

## Open decisions (owner)

1. Discord DMs from the owner's user id: `OwnerDirect` (as implemented), or `Direct` until a second factor?
2. What MCP callers may read beyond the owner's personal memory: other people's and channel records are
   denied today.
3. Should MCP turns be written to Honcho at all (today: yes, tagged with the caller)?
4. Alexa: how it links to the owner (no `alexa_user_id` config yet, so an Alexa turn is never OwnerDirect).
5. Background work not addressed to the owner alone (e.g. a future group digest): no owner memory today.
