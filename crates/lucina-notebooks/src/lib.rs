//! Lucina Notebooks: a NotebookLM-style workspace, ported to Rust from
//! [Open Notebook](https://github.com/lfnovo/open-notebook) by Luis Novo (MIT, see
//! `LICENSE-open-notebook`). Contract: `docs/design/notebooks-api.md`.
//!
//! A notebook groups sources (PDFs, YouTube, web pages, audio, text, files), the owner's
//! Keep notes (linked by slug; the one note store on p1 keeps them) and chats that answer
//! from those only, with citations. Sources get Lucina's own ingest (lucina-server's
//! [`ingest::Extractor`]), one passage index (FTS5 + EmbeddingGemma vectors), prompt-template
//! transformations and kokoro-voiced podcasts.
//!
//! Long work (ingest, insights, podcasts) runs in two background workers. Its state is the
//! rows' `status`, so after a restart (lucina-rs redeploys often) [`Notebooks::start`] picks
//! up whatever was not finished, at most three tries each.

pub mod api;
#[cfg(test)]
mod api_tests;
pub mod chat;
pub mod chunk;
pub mod embed;
pub mod ids;
pub mod ingest;
pub mod keep;
pub mod llm;
pub mod podcast;
pub mod search;
pub mod store;
pub mod transform;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::chunk::Part;
use crate::embed::{Embedder, Kind};
use crate::ids::new_id;
use crate::ingest::Extractor;
use crate::keep::Keep;
use crate::llm::{Delta, Llm, Model};
use crate::podcast::Tts;
use crate::store::{Message, Store};

pub const MAX_TRIES: i64 = 3;
/// Notes are re-read from p1 when their copy is older than this.
const NOTE_REFRESH: chrono::Duration = chrono::Duration::minutes(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Internal(String),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Internal(format!("database: {e}"))
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Internal(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Internal(format!("io: {e}"))
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// `notebooks.db`, `files/`, `podcasts/`, `work/`.
    pub dir: PathBuf,
    pub ffmpeg: PathBuf,
    /// Uploads larger than this are refused.
    pub max_upload: usize,
    /// The model for transformations and podcast scripts (chat picks per message).
    pub background_model: Model,
}

impl Config {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            ffmpeg: PathBuf::from("ffmpeg"),
            max_upload: 200 * 1024 * 1024,
            background_model: Model::Deepseek,
        }
    }
}

pub struct Deps {
    pub extractor: Arc<dyn Extractor>,
    pub embedder: Option<Arc<dyn Embedder>>,
    pub llm: Arc<dyn Llm>,
    pub keep: Arc<dyn Keep>,
    pub tts: Arc<dyn Tts>,
}

#[derive(Clone)]
pub struct Notebooks {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: Config,
    store: Store,
    deps: Deps,
    ingest_wake: Notify,
    podcast_wake: Notify,
}

impl Notebooks {
    pub fn open(cfg: Config, deps: Deps) -> Result<Self, Error> {
        for d in ["files", "podcasts", "work"] {
            std::fs::create_dir_all(cfg.dir.join(d))?;
        }
        let store = Store::open(&cfg.dir.join("notebooks.db"))?;
        Self::with_store(cfg, deps, store)
    }

    pub fn with_store(cfg: Config, deps: Deps, store: Store) -> Result<Self, Error> {
        for t in transform::builtins() {
            store.upsert_builtin(&t)?;
        }
        Ok(Self {
            inner: Arc::new(Inner { cfg, store, deps, ingest_wake: Notify::new(), podcast_wake: Notify::new() }),
        })
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn cfg(&self) -> &Config {
        &self.inner.cfg
    }

    pub fn deps(&self) -> &Deps {
        &self.inner.deps
    }

    pub fn files_dir(&self, source_id: &str) -> PathBuf {
        self.inner.cfg.dir.join("files").join(source_id)
    }

    pub fn podcast_path(&self, id: &str) -> PathBuf {
        self.inner.cfg.dir.join("podcasts").join(format!("{id}.mp3"))
    }

    fn work_dir(&self, id: &str) -> PathBuf {
        self.inner.cfg.dir.join("work").join(id)
    }

    /// Starts the workers (call once, inside a tokio runtime).
    pub fn start(&self) {
        let me = self.clone();
        tokio::spawn(async move { me.ingest_loop().await });
        let me = self.clone();
        tokio::spawn(async move { me.podcast_loop().await });
    }

    pub fn wake(&self) {
        self.inner.ingest_wake.notify_one();
    }

    /// Does all pending work once, in order (tests, and the CLI's `--once`).
    pub async fn drain(&self) {
        for id in self.store().pending_sources().unwrap_or_default() {
            self.process_source(&id).await;
        }
        for id in self.store().pending_insights().unwrap_or_default() {
            self.process_insight(&id).await;
        }
        for id in self.store().pending_podcasts().unwrap_or_default() {
            self.process_podcast(&id).await;
        }
    }

    // ------------------------------------------------------------ workers

    async fn ingest_loop(&self) {
        loop {
            let mut did = false;
            match self.store().pending_sources() {
                Ok(ids) => {
                    for id in ids {
                        did = true;
                        self.process_source(&id).await;
                    }
                }
                Err(e) => tracing::error!(error = %e, "notebooks: listing pending sources failed"),
            }
            match self.store().pending_insights() {
                Ok(ids) => {
                    for id in ids {
                        did = true;
                        self.process_insight(&id).await;
                    }
                }
                Err(e) => tracing::error!(error = %e, "notebooks: listing pending insights failed"),
            }
            // Passages the embedder could not take earlier.
            if self.inner.deps.embedder.is_some() {
                if let Err(e) = self.embed_missing(None).await {
                    tracing::debug!(error = %e, "notebooks: embedding backlog not done");
                }
            }
            if !did {
                let _ = tokio::time::timeout(Duration::from_secs(300), self.inner.ingest_wake.notified()).await;
            }
        }
    }

    async fn podcast_loop(&self) {
        loop {
            let ids = self.store().pending_podcasts().unwrap_or_default();
            if ids.is_empty() {
                let _ = tokio::time::timeout(Duration::from_secs(600), self.inner.podcast_wake.notified()).await;
                continue;
            }
            for id in ids {
                self.process_podcast(&id).await;
            }
        }
    }

    // ------------------------------------------------------------ sources

    async fn process_source(&self, id: &str) {
        let st = self.store();
        let tries = match st.bump_source_attempts(id) {
            Ok(n) => n,
            Err(_) => return,
        };
        if tries > MAX_TRIES {
            let _ = st.set_source_status(id, "failed", Some("gave up after three tries (restarts or errors)"));
            return;
        }
        let result = self.ingest_source(id).await;
        match result {
            Ok(()) => {
                let _ = st.reset_source_attempts(id);
            }
            Err(e) => {
                tracing::warn!(source = id, error = %e, "notebooks: ingest failed");
                let msg: String = e.to_string().chars().take(400).collect();
                let _ = st.set_source_status(id, "failed", Some(&msg));
            }
        }
        let _ = tokio::fs::remove_dir_all(self.work_dir(id)).await;
    }

    async fn ingest_source(&self, id: &str) -> anyhow::Result<()> {
        let st = self.store();
        let src = st.source(id, false)?;
        // Text is in already when the status is "embedding" (a restart mid-way).
        if src.status != "embedding" {
            st.set_source_status(id, "extracting", None)?;
            let work = self.work_dir(id);
            tokio::fs::create_dir_all(&work).await?;
            let x = self.extract(&src, &work).await?;
            let text = x.text();
            if text.trim().chars().count() < 20 {
                anyhow::bail!("no text could be read from this source");
            }
            if let Some(f) = &x.file {
                let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("original").to_owned();
                let dir = self.files_dir(id);
                tokio::fs::create_dir_all(&dir).await?;
                tokio::fs::copy(f, dir.join(&name)).await?;
                st.set_source_file(id, &name)?;
            }
            // A source added without a title takes the one its content has.
            let title = src.title.trim().is_empty().then(|| {
                x.title
                    .clone()
                    .map(|t| t.trim().to_owned())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| fallback_title(&src.origin))
            });
            st.set_source_content(id, title.as_deref(), &text, x.pages, x.duration_s)?;
            if let Some(k) = &x.kind {
                if src.kind == "url" || src.kind == "file" || src.kind == "sandbox" {
                    st.conn().execute("UPDATE sources SET kind = ?2 WHERE id = ?1", rusqlite::params![id, k])?;
                }
            }
            st.replace_chunks(id, &chunk::chunk(&x.parts))?;
            st.set_source_status(id, "embedding", None)?;
        }
        if let Err(e) = self.embed_missing(Some(id)).await {
            // Searchable by words already; the vectors follow when the embedder is back.
            tracing::warn!(source = id, error = %e, "notebooks: embedding failed, kept for later");
        }
        st.set_source_status(id, "ready", None)?;
        // Transformations asked for at ingest, plus the defaults (Dense Summary).
        let src = st.source(id, false)?;
        let mut wanted = src.transformations.clone();
        for t in st.transformations()? {
            if t.apply_default && !wanted.contains(&t.id) {
                wanted.push(t.id);
            }
        }
        let have: Vec<String> = st.insights(id)?.into_iter().map(|i| i.transformation_id).collect();
        for t in wanted {
            if have.contains(&t) {
                continue;
            }
            if let Ok(t) = st.transformation(&t) {
                st.create_insight(&new_id("i"), id, &t)?;
            }
        }
        Ok(())
    }

    async fn extract(&self, src: &store::Source, work: &Path) -> anyhow::Result<ingest::Extracted> {
        let ex = &self.inner.deps.extractor;
        match src.kind.as_str() {
            "text" => Ok(ingest::Extracted {
                kind: Some("text".into()),
                parts: vec![Part { text: self.store().source_text(&src.id)?, ..Default::default() }],
                ..Default::default()
            }),
            "url" | "web" | "youtube" => ex.url(&src.origin, work).await,
            "sandbox" => {
                let path = src.origin.strip_prefix("sandbox:").unwrap_or(&src.origin);
                let local = ex.sandbox(path, work).await?;
                let name = local.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_owned();
                let mut x = ex.file(&local, &name, work).await?;
                x.file.get_or_insert(local);
                Ok(x)
            }
            _ => {
                let name = src.file_name.clone().ok_or_else(|| anyhow::anyhow!("the uploaded file is missing"))?;
                let path = self.files_dir(&src.id).join(&name);
                ex.file(&path, &name, work).await
            }
        }
    }

    /// Embeds passages without vectors (of one owner, or any), in batches.
    pub async fn embed_missing(&self, owner: Option<&str>) -> anyhow::Result<usize> {
        let Some(e) = self.inner.deps.embedder.clone() else {
            return Ok(0);
        };
        let mut done = 0;
        loop {
            let batch = self.store().unembedded(owner, 64)?;
            if batch.is_empty() {
                return Ok(done);
            }
            let title = owner.map(|o| self.store().owner_title(o).unwrap_or_default()).unwrap_or_default();
            let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
            let vecs = e.embed(Kind::Document, &texts, &title).await?;
            let pairs: Vec<(String, Vec<f32>)> = batch.iter().map(|c| c.id.clone()).zip(vecs).collect();
            self.store().set_vectors(&pairs)?;
            done += pairs.len();
        }
    }

    // ------------------------------------------------------------ insights

    async fn process_insight(&self, id: &str) {
        let st = self.store();
        let Ok(tries) = st.bump_insight_attempts(id) else { return };
        if tries > MAX_TRIES {
            let _ = st.set_insight(id, "failed", None, Some("gave up after three tries"));
            return;
        }
        if let Err(e) = self.run_insight(id).await {
            tracing::warn!(insight = id, error = %e, "notebooks: transformation failed");
            let msg: String = e.to_string().chars().take(400).collect();
            let _ = st.set_insight(id, "failed", None, Some(&msg));
        }
    }

    async fn run_insight(&self, id: &str) -> anyhow::Result<()> {
        let st = self.store();
        let ins = st.insight(id)?;
        st.set_insight(id, "running", None, None)?;
        let t = st.transformation(&ins.transformation_id)?;
        let src = st.source(&ins.source_id, false)?;
        let text = with_pages(st, &ins.source_id)?;
        let out = transform::run(self.inner.deps.llm.as_ref(), self.inner.cfg.background_model, &t, &src.title, &text).await?;
        st.replace_chunks(id, &chunk::chunk(&[Part { text: out.clone(), ..Default::default() }]))?;
        st.set_insight(id, "ready", Some(&out), None)?;
        if let Err(e) = self.embed_missing(Some(id)).await {
            tracing::warn!(insight = id, error = %e, "notebooks: insight embedding failed, kept for later");
        }
        Ok(())
    }

    pub fn queue_insight(&self, source_id: &str, transformation_id: &str) -> Result<store::Insight, Error> {
        let st = self.store();
        let src = st.source(source_id, false)?;
        if src.status != "ready" {
            return Err(Error::Conflict("the source is not ready yet".into()));
        }
        let t = st.transformation(transformation_id)?;
        let i = st.create_insight(&new_id("i"), source_id, &t)?;
        self.wake();
        Ok(i)
    }

    // ------------------------------------------------------------ notes

    /// Reads a Keep note from p1 and (re)indexes it if it changed.
    pub async fn sync_note(&self, slug: &str) -> Result<Option<keep::KeepNote>, Error> {
        let Some(n) = self.inner.deps.keep.get(slug).await.map_err(|e| Error::Unavailable(format!("notes: {e}")))? else {
            return Ok(None);
        };
        let st = self.store();
        let owner = format!("note:{slug}");
        let unchanged = st.note_updated(slug)? == n.updated_at && !st.chunks_of(&owner, 0, 1)?.is_empty();
        st.note_synced(slug, &n.title, &keep::excerpt(&n.content), n.updated_at.as_deref())?;
        if !unchanged {
            let text = if n.title.is_empty() { n.content.clone() } else { format!("{}\n\n{}", n.title, n.content) };
            st.replace_chunks(&owner, &chunk::chunk(&[Part { text, ..Default::default() }]))?;
            if let Err(e) = self.embed_missing(Some(&owner)).await {
                tracing::warn!(note = slug, error = %e, "notebooks: note embedding failed, kept for later");
            }
        }
        Ok(Some(n))
    }

    pub async fn link_note(&self, notebook_id: &str, slug: &str) -> Result<store::NoteLink, Error> {
        self.store().notebook(notebook_id)?;
        let n = self
            .inner
            .deps
            .keep
            .get(slug)
            .await
            .map_err(|e| Error::Unavailable(format!("notes: {e}")))?
            .ok_or(Error::NotFound("note"))?;
        self.store().link_note(notebook_id, &n.slug, &n.title, &keep::excerpt(&n.content), None)?;
        self.sync_note(&n.slug).await?;
        self.note_link(notebook_id, &n.slug)
    }

    pub async fn create_note(&self, notebook_id: &str, title: &str, text: &str) -> Result<store::NoteLink, Error> {
        let nb = self.store().notebook(notebook_id)?;
        let labels = vec!["notebook".to_owned(), nb.name.clone()];
        let n = self
            .inner
            .deps
            .keep
            .create(title, text, &labels)
            .await
            .map_err(|e| Error::Unavailable(format!("notes: {e}")))?;
        self.store().link_note(notebook_id, &n.slug, &n.title, &keep::excerpt(&n.content), None)?;
        self.sync_note(&n.slug).await?;
        self.note_link(notebook_id, &n.slug)
    }

    fn note_link(&self, notebook_id: &str, slug: &str) -> Result<store::NoteLink, Error> {
        self.store()
            .notes(notebook_id)?
            .into_iter()
            .find(|n| n.slug == slug)
            .ok_or(Error::NotFound("note"))
    }

    /// Refreshes a notebook's notes whose copy is old (best effort: p1 may be away).
    pub async fn refresh_notes(&self, notebook_id: &str) {
        let before = (chrono::Utc::now() - NOTE_REFRESH).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        for slug in self.store().stale_notes(notebook_id, &before).unwrap_or_default() {
            if let Err(e) = self.sync_note(&slug).await {
                tracing::debug!(note = %slug, error = %e, "notebooks: note refresh skipped");
                break;
            }
        }
    }

    // ------------------------------------------------------------ chat

    pub async fn search(&self, notebook_id: Option<&str>, q: &str, limit: usize) -> Result<Vec<search::Hit>, Error> {
        let owners = match notebook_id {
            Some(nb) => {
                self.store().notebook(nb)?;
                self.store().scope(nb)?
            }
            None => self.store().all_owners()?,
        };
        search::search(self.store(), self.inner.deps.embedder.as_deref(), &owners, q, limit).await
    }

    /// One chat turn: stores the question, answers it from the notebook, stores the answer.
    pub async fn ask(
        &self,
        chat_id: &str,
        question: &str,
        model: Model,
        on_delta: Option<Delta>,
    ) -> Result<(Message, Message), Error> {
        let question = question.trim();
        if question.is_empty() {
            return Err(Error::BadRequest("the message is empty".into()));
        }
        let st = self.store();
        let chat = st.chat(chat_id)?;
        let nb = st.notebook(&chat.notebook_id)?;
        let history = st.messages(chat_id)?;
        self.refresh_notes(&nb.id).await;
        let q = chat::retrieval_query(question, &history);
        let hits = self.search(Some(&nb.id), &q, chat::PASSAGES).await?;
        let passages = chat::passages(st, &hits)?;
        let user = Message {
            id: new_id("m"),
            role: "user".into(),
            text: question.to_owned(),
            citations: vec![],
            model: None,
            created_at: ids::now(),
        };
        st.add_message(chat_id, &user)?;
        let a = chat::answer(self.inner.deps.llm.as_ref(), model, &nb.name, &nb.description, &history, &passages, question, on_delta)
            .await
            .map_err(|e| Error::Unavailable(format!("the model could not answer: {e}")))?;
        let reply = Message {
            id: new_id("m"),
            role: "assistant".into(),
            text: a.text,
            citations: a.citations,
            model: Some(a.model),
            created_at: ids::now(),
        };
        st.add_message(chat_id, &reply)?;
        // A new chat takes its first question as its title.
        if history.is_empty() && chat.title == "New chat" {
            let title: String = question.chars().take(60).collect();
            let _ = st.rename_chat(chat_id, &title);
        }
        Ok((user, reply))
    }

    // ------------------------------------------------------------ podcasts

    pub fn queue_podcast(
        &self,
        notebook_id: &str,
        episode: Option<&str>,
        speakers: Option<&str>,
        briefing: Option<&str>,
        minutes: Option<f64>,
    ) -> Result<store::Podcast, Error> {
        let nb = self.store().notebook(notebook_id)?;
        let ep = podcast::episode_profile(episode.unwrap_or("deep-dive"))
            .ok_or_else(|| Error::BadRequest("unknown episode profile".into()))?;
        let sp_id = speakers.unwrap_or(&ep.speaker_profile).to_owned();
        podcast::speaker_profile(&sp_id).ok_or_else(|| Error::BadRequest("unknown speaker profile".into()))?;
        let ready = self.store().notebook_sources(notebook_id)?.iter().any(|s| s.status == "ready" && s.context.as_deref() != Some("off"));
        if !ready && self.store().notes(notebook_id)?.is_empty() {
            return Err(Error::Conflict("the notebook has no ready sources or notes yet".into()));
        }
        let minutes = minutes.unwrap_or(5.0).clamp(1.0, 30.0);
        let briefing = briefing.map(str::trim).filter(|b| !b.is_empty()).map(str::to_owned).unwrap_or(ep.briefing.clone());
        let p = self.store().create_podcast(&new_id("p"), notebook_id, &nb.name, &ep.id, &sp_id, &briefing, minutes)?;
        self.inner.podcast_wake.notify_one();
        Ok(p)
    }

    /// The notebook as podcast material: summaries (or the start of the text) and notes.
    pub fn podcast_context(&self, notebook_id: &str, budget: usize) -> Result<String, Error> {
        let st = self.store();
        let mut parts: Vec<String> = Vec::new();
        for s in st.notebook_sources(notebook_id)? {
            if s.status != "ready" || s.context.as_deref() == Some("off") {
                continue;
            }
            let summary = st
                .insights(&s.id)?
                .into_iter()
                .find(|i| i.transformation_id == transform::DENSE_SUMMARY && i.status == "ready")
                .map(|i| i.text);
            let body = match summary {
                Some(t) => t,
                None => st.source_text(&s.id)?.chars().take(12_000).collect(),
            };
            parts.push(format!("## Source: {}\n{}", s.title, body));
        }
        for n in st.notes(notebook_id)? {
            let text: String = st
                .chunks_of(&format!("note:{}", n.slug), 0, 20)?
                .into_iter()
                .map(|c| c.text)
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("## Note: {}\n{}", n.title, text));
        }
        let mut out = String::new();
        for p in parts {
            if out.chars().count() + p.chars().count() > budget {
                let room = budget.saturating_sub(out.chars().count());
                out.extend(p.chars().take(room));
                break;
            }
            out.push_str(&p);
            out.push_str("\n\n");
        }
        Ok(out)
    }

    async fn process_podcast(&self, id: &str) {
        let st = self.store();
        let Ok(tries) = st.bump_podcast_attempts(id) else { return };
        if tries > MAX_TRIES {
            let _ = st.set_podcast_status(id, "failed", Some("gave up after three tries"));
            return;
        }
        match self.make_podcast(id).await {
            Ok(()) => {
                let _ = tokio::fs::remove_dir_all(self.work_dir(id)).await;
            }
            Err(e) => {
                tracing::warn!(podcast = id, error = %e, "notebooks: podcast failed");
                let msg: String = e.to_string().chars().take(400).collect();
                let _ = st.set_podcast_status(id, "failed", Some(&msg));
            }
        }
    }

    async fn make_podcast(&self, id: &str) -> anyhow::Result<()> {
        let st = self.store();
        let p = st.podcast(id)?;
        let ep = podcast::episode_profile(&p.episode_profile).ok_or_else(|| anyhow::anyhow!("unknown episode profile"))?;
        let sp = podcast::speaker_profile(&p.speaker_profile).ok_or_else(|| anyhow::anyhow!("unknown speaker profile"))?;
        let model = self.inner.cfg.background_model;
        let llm = self.inner.deps.llm.as_ref();
        let mut lines: Vec<podcast::Line> = serde_json::from_value(p.transcript.clone()).unwrap_or_default();
        if lines.is_empty() {
            st.set_podcast_status(id, "outline", None)?;
            let context = self.podcast_context(&p.notebook_id, 60_000)?;
            let (title, segs) = podcast::outline(llm, model, &context, &p.briefing, &sp, ep.segments).await?;
            st.set_podcast_script(id, Some(&title).filter(|t| !t.is_empty()).map(String::as_str), &json!(segs), &json!([]))?;
            st.set_podcast_status(id, "script", None)?;
            for i in 0..segs.len() {
                let mut more = podcast::script_segment(llm, model, &context, &p.briefing, &sp, &segs, i, &lines, p.minutes).await?;
                lines.append(&mut more);
            }
            st.set_podcast_script(id, None, &json!(segs), &json!(lines))?;
        }
        st.set_podcast_status(id, "voicing", None)?;
        let out = self.podcast_path(id);
        let secs = podcast::voice_and_mix(self.inner.deps.tts.as_ref(), &self.inner.cfg.ffmpeg, &sp, &lines, &self.work_dir(id), &out).await?;
        st.set_podcast_duration(id, secs)?;
        st.set_podcast_status(id, "ready", None)?;
        Ok(())
    }

    pub fn health(&self) -> Result<Value, Error> {
        let (chunks, vectors) = self.store().count_chunks()?;
        Ok(json!({
            "ok": true,
            "sources": self.store().count_sources()?,
            "chunks": chunks,
            "vectors": vectors,
            "embedder": self.inner.deps.embedder.as_ref().map(|e| e.name()),
        }))
    }
}

/// A title from where a source came from: the file name, or the link's host and path.
pub fn fallback_title(origin: &str) -> String {
    let o = origin.trim_start_matches("sandbox:").trim_start_matches("upload:");
    if let Ok(u) = reqwest::Url::parse(o) {
        let path = u.path().trim_matches('/');
        let host = u.host_str().unwrap_or("");
        return if path.is_empty() { host.to_owned() } else { format!("{host}/{path}").chars().take(120).collect() };
    }
    let name = o.rsplit('/').next().unwrap_or(o);
    name.rsplit_once('.').map(|(n, _)| n).filter(|n| !n.is_empty()).unwrap_or(name).to_owned()
}

/// A source's text for a transformation: PDFs with page markers, so a table of contents can
/// say where things are.
fn with_pages(st: &Store, source_id: &str) -> Result<String, Error> {
    let chunks = st.chunks_of(source_id, 0, 100_000)?;
    if chunks.iter().all(|c| c.page.is_none()) {
        return st.source_text(source_id);
    }
    let mut out = String::new();
    let mut page = None;
    for c in chunks {
        if c.page != page {
            page = c.page;
            if let Some(p) = page {
                out.push_str(&format!("\n[p. {p}]\n"));
            }
        }
        out.push_str(&c.text);
        out.push('\n');
    }
    Ok(out)
}
