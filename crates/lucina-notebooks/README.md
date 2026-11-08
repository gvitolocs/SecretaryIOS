# lucina-notebooks

Lucina Notebooks: a NotebookLM-style workspace for the owner's documents, inside the Rust Lucina.
A notebook groups **sources** (PDFs, YouTube videos, web pages, audio, Office files, text, files
on Lucina's computer), **notes** (his Keep notes, linked by slug; the one note store on p1 keeps
them) and **chats** that answer only from those, citing the passage they used.

API contract: [`docs/design/notebooks-api.md`](../../docs/design/notebooks-api.md).

## Credit

This is a Rust port of **[Open Notebook](https://github.com/lfnovo/open-notebook)** by Luis Novo
(“An Open Source implementation of Notebook LM with more flexibility and features”), released under the
MIT License. Its licence is in [`LICENSE-open-notebook`](LICENSE-open-notebook). Ported from it:

- the model: notebooks, sources shared between notebooks, insights from transformations, chat sessions,
  per-source context (full / summary / off), the delete preview (exclusive vs shared sources);
- the `[type:id]` citation convention and its rules (cite only retrieved ids, never alter them);
- the six built-in transformations (Dense Summary, Simple Summary, Key Insights, Paper Analysis,
  Reflection Questions, Table of Contents), lightly edited, with Dense Summary applied by default;
- the podcast pipeline: episode and speaker profiles, briefing → outline (JSON segments) → dialogue
  (JSON lines) → speech per line → one audio file.

## What is different

| Open Notebook | Lucina Notebooks |
| --- | --- |
| Python, FastAPI, LangChain/LangGraph, SurrealDB, Next.js | Rust: this crate (axum, SQLite with FTS5), inside lucina-server; the web plugin in SecretaryIOS `web/` |
| Its own notes table | The owner's Keep notes on p1, linked and indexed, never copied into a second store |
| 20+ providers via Esperanto | DeepSeek with Lucina's own key, or the local Qwen 3.8 27B; never a free or anonymous route |
| content-core extraction | Lucina's pipelines: PDF pages (with page numbers), the share fetcher, yt-dlp captions or local Whisper, OCR |
| Embedding provider of choice, vector search in SurrealDB | EmbeddingGemma 2 on nezopt's CPU (`lucina-notebooks-embed.service`); hybrid search: bm25 + cosine, reciprocal-rank fusion |
| Chat context built by the client | Retrieval per question (the best ten passages of the notebook), citations checked by the server |
| Podcast TTS providers | kokoro on nezopt (`af_heart`, `am_michael`, `if_sara`, `im_nicola`) and ffmpeg |
| Ask (multi-search strategy graph) | Not ported yet: notebook chat with follow-up retrieval covers it |

## Layout

| File | What |
| --- | --- |
| `store.rs` | SQLite schema and queries: notebooks, sources, note links, passages + FTS5 + vectors, chats, transformations, insights, podcasts |
| `chunk.rs` | ~1200-character passages on paragraph and sentence boundaries, keeping page or start second |
| `embed.rs` | The embedder trait and the OpenAI-compatible client (EmbeddingGemma task prompts) |
| `search.rs` | Hybrid search |
| `chat.rs` | Retrieval-grounded answers and citation checking |
| `transform.rs` | Built-in transformations and running one |
| `podcast.rs` | Profiles, outline, dialogue, kokoro, mixing |
| `ingest.rs` | The extractor trait, PDF pages, text files, YouTube links |
| `keep.rs` | p1's Keep notes |
| `llm.rs` | DeepSeek / local Qwen |
| `api.rs` | The HTTP API |
| `lib.rs` | The service and its two background workers (ingest + insights, podcasts), resumable after restarts |

## Running

lucina-server opens it at startup (`crates/lucina-server/src/notebooks.rs`). State is in
`$LUCINA_NOTEBOOKS_DIR` (default `~/Projects/Lucina/data/notebooks`): `notebooks.db`, `files/<source>/`,
`podcasts/<id>.mp3`, `work/` (scratch). Embeddings: `deploy/lucina-notebooks/lucina-notebooks-embed.service`
(user unit, 127.0.0.1:8897, CPUQuota 300 %).
