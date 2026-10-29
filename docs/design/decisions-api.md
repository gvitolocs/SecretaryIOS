# Decisions API: asking Lucina for a go-ahead (contract v1, 2026-10-28)

Owner rule (2026-10-28): Claude sessions and other agents do not ask the owner for go-aheads. They ask
Lucina, who decides from what he told her (memory, his own messages, earlier decisions) with JEV, or asks
him herself on his phone. A go-ahead is a structured request, never free text through `ask_lucina`:
free text goes through intent routing, which started a Harness job and a reminder on the first real use.

Three hops, three deploy lines:

| Hop | Line | What |
|---|---|---|
| secretary-mcp (p1) | `server-live` | MCP tools `lucina_decide`, `lucina_decision_status`, `lucina_decision_withdraw`, `lucina_job_status` |
| secretary-server (p1) | `server-live` | `/v1/decisions*` forwarded to Lucina with the device credential |
| lucina-rs (nezopt) | `lucina-rs` | `/api/secretary/decisions*`: the decision itself, the record, the owner's push |

## lucina-rs: `POST /api/secretary/decisions`

Body (snake_case, the MCP arguments as they are, plus `asking_client`):

| Field | Type | Rules |
|---|---|---|
| `question` | string | required, 1-2000 chars |
| `options` | string[] | required, 2-6 entries, each 1-200 chars, unique (case-insensitive, trimmed) |
| `recommendation` | string | optional; one of `options` (case-insensitive, trimmed) or its 1-based number (`"2"`) |
| `context` | string | optional, up to 8000 chars |
| `reversible` | bool | required |
| `impact` | `low` \| `medium` \| `high` | required |
| `urgency` | `low` \| `normal` \| `high` | optional, default `normal` |
| `topic` | string | optional, up to 64 chars: groups decisions on the same subject (`gallery-categories`) |
| `asking_session` | string | optional, up to 120 chars: which session asks (`coordinator`, a session id) |
| `asking_client` | string | set by secretary-mcp from the MCP token (`claude-code`, `chatgpt`, …), never by the agent; 1-32 chars `[a-z0-9-]` |

Anything else is ignored. A bad body is `400 bad_request` with a message naming the field.

Response `200` (also for `GET /api/secretary/decisions/{id}`):

```json
{
  "decision_id": "dc-3f9a1c2b7d4e",
  "record_id": "dc-3f9a1c2b7d4e",
  "status": "decided",
  "decision": "go",
  "chosen_option": "Deploy now",
  "chosen_index": 1,
  "reason": "Reversible and low impact, and he told me on 27 Oct to ship small fixes without asking.",
  "decided_by": "lucina",
  "confidence": 0.86,
  "asked_owner": false,
  "owner_only": false,
  "question": "…", "options": ["Deploy now", "Wait"], "recommendation": "Deploy now",
  "impact": "low", "reversible": true, "urgency": "normal", "topic": null,
  "asking_client": "claude-code", "asking_session": "coordinator",
  "created_at": "2026-10-28T18:02:11.120Z", "decided_at": "2026-10-28T18:02:13.501Z",
  "expires_at": null,
  "evidence": [{"kind": "owner_message", "text": "…"}, {"kind": "earlier_decision", "text": "…"}]
}
```

- `status`: `decided` | `pending` (she asked the owner and waits) | `expired` (no answer in 24 h).
- `decision`: `go` (the recommendation was chosen), `change` (another option was chosen), `owner_reply`
  (the owner answered in his own words: `chosen_option` is null and `reason` is his text), or null while
  pending/expired.
- `decided_by`: `lucina` | `owner` | null. `confidence`: Lucina's (0-1); 1.0 for the owner's answer.
- `chosen_index`: 1-based, null when there is no chosen option.
- `owner_only`: the question is one only the owner may answer (money, purchases, passwords or payment
  details, accepting terms, messages to other people in his name): always `pending`, never decided by Lucina.
  Claude sessions still need the owner's own words in their chat for those (their safety rules).
- `evidence`: at most 6 short lines (each ≤ 300 chars) of what she based it on, best tier first
  (`tier`: 1 his own words, 2 his own actions, 3 her earlier decisions, 4 agents' records). Every line must
  share the question's keywords (on topic). Tier 2 (`kind: owner_action`, last 30 days, newest first, at most 4):
  - secrets he saved through the Add-secret sheet (timeline items `meta.kind: "secret_receipt"`);
  - broker grants he made from the apps and Lucina relayed (`LUCINA_SECRET_GRANTS_FILE`, default
    `<LUCINA_DATA_DIR>/secret-grants.jsonl`, mode 0600: one line per relayed grant with `ts`, `op`
    (`put_secret`, `create_project`, …), `project`, `environment`, `path`, `key`, `project_name`, `slug`,
    `caller` when the grant names one, `outcome` `ok`|`error` and the broker's `error` code; never the
    value, its hash, the grant or a token). Catalog browsing and unsuccessful relays are not evidence;
  - root steps he approved or denied on his iPhone (`LUCINA_ROOT_AUDIT_FILE` (also `LUCINA_ROOT_AUDIT`), default
    `<LUCINA_DATA_DIR>/root-exec-audit.jsonl`; `requested` alone is not his act). Steps that touch
    `/etc/infisical-broker/` (or a reason that names the broker or broker.toml) read "Giuseppe approved
    a broker settings change: …", with caller, section and grant names from readable staged `.toml`
    files referenced by the steps.
    Consecutive approved copies are compared by name only; when no named change can be recovered,
    the line uses the requester's reason. Approval describes his decision, not whether the steps succeeded.
  The broker's own iPhone approvals go from the broker to secretary-server directly; Lucina keeps no
  record of them, and the broker's audit log is not readable by her.

When she decides herself / asks him:
- Always asks him (pending): `impact: high`, `reversible: false`, `owner_only`, conflicting evidence
  (an earlier owner decision or message that goes against the recommendation), or confidence below the
  threshold (low impact 0.6, medium 0.8).
- Otherwise she decides.

Asking him: a Lucina message in his main chat with one button per option (rich `actions`, kind `reply`,
text `Decision dc-…: <n>. <label>`), and a push (`/v1/notify`, category `lucina_message`). His tap (or the
same text typed on Telegram) resolves it; any other text after `Decision dc-…:` is an `owner_reply`.
Only the owner's own turns resolve decisions: an MCP turn writing that text does nothing.

Recording: every request, decision and answer is appended to `<LUCINA_DATA_DIR>/decisions/decisions.jsonl`
(one JSON object per event, the full decision object after each change; the last line per id wins), and the
decision shows on the timeline in the asking client's MCP conversation (`meta.kind: "decision"`). Once
`ops_record` ships, decisions are also written there as `kind=decision`.

`GET /api/secretary/decisions/{id}`: the object above, `404 decision_not_found`.
`GET /api/secretary/decisions?status=pending|decided|expired&limit=1..100` (default 20): `{"items": [...]}`, newest first.

## secretary-server (p1)

`POST /v1/decisions`, `GET /v1/decisions/{id}`, `GET /v1/decisions` behind the device-token middleware, forwarded
to Lucina like `/v1/jobs/{id}`. The POST may take up to 40 s (memory search, DeepSeek, JEV): forward with a
60 s timeout. Body and query are passed through unchanged (the body is a JSON object ≤ 32 KB).
`DELETE /v1/decisions/{id}?reason=` forwards to `DELETE /api/secretary/decisions/{id}?reason=` (the asker takes
back a pending decision); `reason` is optional, trimmed, at most 500 characters.

## secretary-mcp tools (scope `chat`)

`lucina_decide` — arguments exactly the body fields above except `asking_client`, plus
`wait_seconds` (0-100, default 0: while the answer is `pending`, poll `GET /v1/decisions/{id}` every 3 s
until it is not pending or the time is up). Unknown fields are refused. `asking_client` = the caller's
name. Answer text: one line in words, then the JSON object:

- decided: `Lucina decided: GO with "<chosen_option>" (confidence 0.86). Reason: …  Decision dc-…`
  (`CHANGE to "<chosen_option>"` when `decision` is `change`; `The owner decided: …` when `decided_by` is owner;
  `The owner answered: "<reason>"` for `owner_reply`).
- pending: `Pending: Lucina asked the owner on his phone (decision dc-…). Do not act on this yet; check with
  lucina_decision_status {"decision_id": "dc-…"}.`
- expired: `Expired: the owner did not answer within 24 h (decision dc-…). Ask again or leave it.`
- withdrawn: `Withdrawn: decision dc-… needs no answer. <reason>`

`lucina_decision_status` — `{decision_id, wait_seconds?}` (same polling), same answer text.

`lucina_decision_withdraw` — `{decision_id, reason?}`: `DELETE /v1/decisions/{id}?reason=`; takes back a decision
still pending (asked by mistake, settled elsewhere), and the owner's buttons then answer that it was withdrawn.
Same answer text.

`lucina_job_status` — `{job_id}`: `GET /v1/jobs/{id}`; answers the status (`queued`, `running`, `done`,
`error`), the action, the result or error, and the times.

`ask_lucina` — new optional `allow`: array of `reminders` | `notes` | `tasks` | `shopping`. Agent clients
(everyone except the owner's own assistant apps, see the MCP policy) get answers only from `ask_lucina`
("consult"): no Harness or coding jobs, computer tasks, reminders, posts or list writes. `allow` lets one
message act on that kind of request. secretary-mcp encodes it in the prefix:
`[via Lucina MCP, from claude-code; allow=reminders,notes] …`.

## lucina-rs chat response fields for MCP callers

- `consult: true` on an agent's consult turn; `notRun: "<action>"` and `hint: "<tool to use>"` when
  Lucina recognised an action she did not run.
- `job: {id, status}` as before: secretary-mcp tells the agent to follow it with `lucina_job_status`.
