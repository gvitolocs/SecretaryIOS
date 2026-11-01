# Lucina Notebooks: API contract

A NotebookLM-style workspace: a notebook groups **sources** (PDFs, YouTube, web pages, audio, text, files),
**notes** (the existing Keep notes on p1, linked, never copied into a second store) and **chats** that answer
only from those sources and cite them. Ported from [Open Notebook](https://github.com/lfnovo/open-notebook)
by Luis Novo (MIT); see `crates/lucina-notebooks/README.md` for the credit and what changed.

## Where it runs

- `crates/lucina-notebooks` (library) runs inside lucina-server on nezopt (line `lucina-rs`). State:
  `$LUCINA_NOTEBOOKS_DIR` (default `~/Projects/Lucina/data/notebooks`): `notebooks.db` (SQLite: rows, FTS5,
  vectors), `files/<source id>/` (originals), `podcasts/<id>.mp3`.
- lucina-server: `/api/secretary/notebooks/<rest>` → the crate's router at `/<rest>`, after the Secretary bearer
  check. The caller's origin header (`X-Lucina-Origin`) is kept for the log.
- p1 secretary-server: `/v1/notebooks/<rest>` → Link → `/api/secretary/notebooks/<rest>`, same method, query,
  body and status; responses stream (podcast audio supports `Range`). Owner devices and MCP tokens with the
  `notebooks` scope.
- secretary-mcp: `notebook_list`, `notebook_get`, `notebook_create`, `notebook_add_source`, `notebook_ask`,
  `notebook_search`, `notebook_transform`, `notebook_podcast`.

## Ids and citations

| Thing | Id | Example |
| --- | --- | --- |
| notebook | `nb` + 10 hex | `nb4f1c09a2e7` |
| source | `s` + 10 hex | `s0a9be3c112` |
| chunk (passage) | `<source id>-<ordinal>` | `s0a9be3c112-14` |
| chat | `c` + 10 hex | |
| message | `m` + 10 hex | |
| insight | `i` + 10 hex | |
| transformation | `t-` + slug | `t-dense-summary` |
| podcast | `p` + 10 hex | |

Answers cite passages inline as `[source:<chunk id>]` (e.g. `[source:s0a9be3c112-14]`), notes as
`[note:<slug>]` (as everywhere in Lucina) and insights as `[insight:<id>]`. The server drops any citation that
was not among the passages given to the model; every message also returns the resolved `citations` list, so a
client never has to parse ids to show them.

## Errors

`{"error": {"code": "not_found" | "bad_request" | "conflict" | "unavailable" | "internal", "message": "…"}}`
with the matching HTTP status.

## Objects

```jsonc
// Notebook
{ "id": "nb…", "name": "CNS", "description": "", "archived": false,
  "created_at": "2026-10-29T13:00:00Z", "updated_at": "…",
  "counts": { "sources": 3, "notes": 1, "chats": 2, "podcasts": 0 } }

// Source (in a notebook listing it also has "context": "full" | "summary" | "off")
{ "id": "s…", "kind": "pdf" | "youtube" | "web" | "text" | "audio" | "file" | "note",
  "title": "RM Class 2", "origin": "https://… | sandbox:/home/cua/… | upload:RM Class 2.pdf | note:<slug>",
  "status": "queued" | "extracting" | "embedding" | "ready" | "failed",
  "error": null, "chars": 48211, "chunks": 37, "pages": 22, "duration_s": null,
  "has_file": true, "created_at": "…", "updated_at": "…", "notebooks": ["nb…"] }

// Note link (the note itself lives in Keep on p1)
{ "slug": "rm-class-2-summary", "title": "…", "excerpt": "…", "updated_at": "…", "indexed": true }

// Chat, Message, Citation
{ "id": "c…", "notebook_id": "nb…", "title": "Exam prep", "created_at": "…", "updated_at": "…", "messages": 4 }
{ "id": "m…", "role": "user" | "assistant", "text": "… [source:s0a9be3c112-14] …",
  "citations": [ { "ref": "source:s0a9be3c112-14", "kind": "source", "source_id": "s0a9be3c112", "chunk": 14,
                   "title": "RM Class 2", "excerpt": "…", "page": 7, "start_s": null } ],
  "model": "deepseek-flash" | "qwen-local", "created_at": "…" }

// Transformation, Insight
{ "id": "t-dense-summary", "name": "Dense Summary", "description": "…", "prompt": "…",
  "apply_default": true, "builtin": true }
{ "id": "i…", "source_id": "s…", "transformation_id": "t-…", "title": "Dense Summary",
  "status": "queued" | "running" | "ready" | "failed", "text": "…", "created_at": "…" }

// Podcast
{ "id": "p…", "notebook_id": "nb…", "title": "…", "episode_profile": "two-hosts", "speaker_profile": "lucina-duo",
  "briefing": "…", "status": "queued" | "outline" | "script" | "voicing" | "mixing" | "ready" | "failed",
  "error": null, "outline": [ { "name": "…", "description": "…", "size": "short" } ],
  "transcript": [ { "speaker": "Lucina", "text": "…" } ], "duration_s": 312.4, "audio": true,
  "created_at": "…" }
```

## Routes (relative to `/api/secretary/notebooks` on nezopt and `/v1/notebooks` on p1)

| Method | Path | Body / query | Returns |
| --- | --- | --- | --- |
| GET | `/` | `?archived=exclude|include|only` | `{ "notebooks": [Notebook] }` |
| POST | `/` | `{ name, description? }` | Notebook (201) |
| GET | `/{nb}` | | Notebook + `sources`, `notes`, `chats`, `podcasts` |
| PATCH | `/{nb}` | `{ name?, description?, archived? }` | Notebook |
| DELETE | `/{nb}` | `?sources=keep|delete-exclusive` | `{ deleted: true, sources_deleted: n }` |
| POST | `/{nb}/sources` | `{ kind: "url", url }` · `{ kind: "text", title?, text }` · `{ kind: "sandbox", path }` · `{ kind: "note", slug }` · `{ kind: "source", id }` (add an existing source); `transformations?: [id]` | Source (202, ingest runs in the background) |
| POST | `/{nb}/sources/upload` | multipart `file`, `title?` (≤ 200 MB) | Source (202) |
| PATCH | `/{nb}/sources/{src}` | `{ context: "full" | "summary" | "off" }` | Source |
| DELETE | `/{nb}/sources/{src}` | | `{ removed: true, deleted: bool }` (deleted when no other notebook has it) |
| GET | `/sources/{src}` | `?text=1` adds `text` | Source |
| GET | `/sources/{src}/chunks` | `?from=&limit=` or `?around=<ordinal>` | `{ chunks: [{ id, ordinal, text, page?, start_s? }] }` |
| GET | `/sources/{src}/file` | | the original file |
| POST | `/sources/{src}/retry` | | Source (202) |
| GET | `/sources/{src}/insights` | | `{ insights: [Insight] }` |
| POST | `/sources/{src}/insights` | `{ transformation_id }` | Insight (202) |
| POST | `/{nb}/notes` | `{ slug }` (link) or `{ title, text }` (new Keep note, labels `notebook`) | NoteLink (201) |
| DELETE | `/{nb}/notes/{slug}` | | `{ removed: true }` (the Keep note stays) |
| GET | `/{nb}/search` | `?q=&limit=` | `{ hits: [{ ref, kind, source_id?, slug?, title, excerpt, score }] }` |
| GET | `/{nb}/chats` | | `{ chats: [Chat] }` |
| POST | `/{nb}/chats` | `{ title? }` | Chat (201) |
| GET | `/chats/{c}` | | Chat + `messages: [Message]` |
| PATCH | `/chats/{c}` | `{ title }` | Chat |
| DELETE | `/chats/{c}` | | `{ deleted: true }` |
| POST | `/chats/{c}/messages` | `{ text, model?: "deepseek" | "qwen" }`; `Accept: text/event-stream` streams `delta` events then `message` | `{ user: Message, reply: Message }` |
| POST | `/chats/{c}/messages/{m}/note` | `{ title? }` | NoteLink (the reply saved to Keep and linked) |
| POST | `/insights/{i}/note` | | NoteLink |
| DELETE | `/insights/{i}` | | `{ deleted: true }` |
| GET | `/transformations` | | `{ transformations: [Transformation] }` |
| POST | `/transformations` | `{ name, description?, prompt, apply_default? }` | Transformation (201) |
| PATCH | `/transformations/{t}` | any field | Transformation |
| DELETE | `/transformations/{t}` | | `{ deleted: true }` (built-ins: 409) |
| GET | `/podcast-profiles` | | `{ episode_profiles: [...], speaker_profiles: [...] }` |
| GET | `/{nb}/podcasts` | | `{ podcasts: [Podcast] }` |
| POST | `/{nb}/podcasts` | `{ episode_profile?, speaker_profile?, briefing?, minutes? }` | Podcast (202) |
| GET | `/podcasts/{p}` | | Podcast |
| GET | `/podcasts/{p}/audio` | `Range` ok | `audio/mpeg` |
| DELETE | `/podcasts/{p}` | | `{ deleted: true }` |
| GET | `/search` | `?q=&limit=` (every notebook) | as `/{nb}/search` plus `notebook_id` |
| GET | `/health` | | `{ ok, sources, chunks, embedder }` |

## Models

- Chat, transformations and podcast scripts: DeepSeek with Lucina's own key (`DEEPSEEK_API_KEY_LUCINA`, the
  lucina-llm client lucina-rs already uses), or the local Qwen 3.8 27B (`model: "qwen"`) through the gateway
  (`127.0.0.1:11436`, under the GPU lease). Never a free or anonymous route: these are the owner's documents.
- Embeddings: the local embedding model (see `crates/lucina-notebooks/README.md`).
- Speech: kokoro on nezopt.
