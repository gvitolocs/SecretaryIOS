//! "Save to Lucina": every page of a vetted gallery goes through the service's own spaced image
//! fetcher and cache, is written to the spool directory and handed to lucina-media's loopback
//! `POST /internal/v1/ingest` (`source: "nclient"`, `album: <gallery title>`, `move: true`).
//!
//! It runs as a background job (a gallery can be 200+ pages at one image per second). The job
//! id is random and says nothing about the gallery; `GET /saves/{job}` reports the progress.
//! The gallery is vetted (`Service::gallery`, then `Service::image`, which vets again) before
//! the first page and before every later page: a gallery the filter blocks stops the job, and
//! nothing of a blocked gallery is ever fetched or ingested. Logs carry no ids, titles or
//! page numbers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::http::Http;
use crate::service::{ImageKind, ServiceError};
use crate::upstream::UpstreamError;

/// Largest gallery a job accepts.
pub const MAX_PAGES: usize = 2000;
/// Jobs waiting or running at once.
const MAX_ACTIVE: usize = 8;
/// Finished jobs kept for status queries.
const KEEP_FINISHED: usize = 32;
const FINISHED_TTL: Duration = Duration::from_secs(3600);
const PAGE_TRIES: usize = 3;
const INGEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Consecutive ingest failures (lucina-media unreachable) that end a job.
const MEDIA_DOWN_AFTER: usize = 3;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedPage {
    pub index: usize,
    pub item_id: String,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedPage {
    pub index: usize,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl State {
    fn finished(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Cancelled)
    }
}

struct Progress {
    state: State,
    done: usize,
    saved: Vec<SavedPage>,
    failed: Vec<FailedPage>,
    error: Option<String>,
    finished_at: Option<std::time::Instant>,
}

pub struct Job {
    pub id: String,
    gallery: i32,
    pub total: usize,
    cancel: AtomicBool,
    progress: Mutex<Progress>,
}

impl Job {
    pub fn status(&self) -> Value {
        let p = self.progress.lock().unwrap();
        let mut v = json!({
            "job": self.id,
            "state": p.state,
            "total": self.total,
            "done": p.done,
            "saved": p.saved,
            "failed": p.failed,
        });
        if let Some(e) = &p.error {
            v["error"] = json!(e);
        }
        v
    }

    fn set_state(&self, state: State) {
        let mut p = self.progress.lock().unwrap();
        p.state = state;
        if state.finished() {
            p.finished_at = Some(std::time::Instant::now());
        }
    }

    fn fail(&self, error: &str) {
        let mut p = self.progress.lock().unwrap();
        p.state = State::Failed;
        p.error = Some(error.to_string());
        p.finished_at = Some(std::time::Instant::now());
    }
}

pub struct Saves {
    spool: PathBuf,
    media_url: String,
    http: reqwest::Client,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    /// Serialises the jobs: the site's image rate limit is shared anyway.
    turn: Semaphore,
    /// Pause before the second try of an ingest.
    retry_delay: Duration,
}

#[derive(Debug)]
pub enum StartError {
    Busy,
}

impl Saves {
    /// `media_url` is lucina-media's base URL (loopback). Whatever a crash left in the spool is
    /// removed.
    pub fn new(spool: impl Into<PathBuf>, media_url: impl Into<String>) -> std::io::Result<Saves> {
        let spool = spool.into();
        let _ = std::fs::remove_dir_all(&spool);
        std::fs::create_dir_all(&spool)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Saves {
            spool,
            media_url: media_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(INGEST_TIMEOUT)
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(std::io::Error::other)?,
            jobs: Mutex::new(HashMap::new()),
            turn: Semaphore::new(1),
            retry_delay: Duration::from_secs(2),
        })
    }

    /// Tests: a shorter pause before an ingest is tried again.
    pub fn with_retry_delay(mut self, d: Duration) -> Saves {
        self.retry_delay = d;
        self
    }

    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().get(id).cloned()
    }

    /// Ask a running job to stop after its current page.
    pub fn cancel(&self, id: &str) -> bool {
        match self.get(id) {
            Some(j) => {
                j.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    fn register(&self, gallery: i32, total: usize) -> Result<(Arc<Job>, bool), StartError> {
        let mut jobs = self.jobs.lock().unwrap();
        // The same gallery already waiting or running: that job is the answer.
        if let Some(j) = jobs
            .values()
            .find(|j| j.gallery == gallery && !j.progress.lock().unwrap().state.finished())
        {
            return Ok((j.clone(), false));
        }
        jobs.retain(|_, j| {
            let p = j.progress.lock().unwrap();
            p.finished_at.is_none_or(|t| t.elapsed() < FINISHED_TTL)
        });
        let mut finished: Vec<(String, std::time::Instant)> = jobs
            .iter()
            .filter_map(|(k, j)| Some((k.clone(), j.progress.lock().unwrap().finished_at?)))
            .collect();
        if finished.len() > KEEP_FINISHED {
            finished.sort_by_key(|(_, t)| *t);
            for (k, _) in finished.iter().take(finished.len() - KEEP_FINISHED) {
                jobs.remove(k);
            }
        }
        let active = jobs
            .values()
            .filter(|j| !j.progress.lock().unwrap().state.finished())
            .count();
        if active >= MAX_ACTIVE {
            return Err(StartError::Busy);
        }
        let mut raw = [0u8; 12];
        getrandom::fill(&mut raw).map_err(|_| StartError::Busy)?;
        let job = Arc::new(Job {
            id: hex::encode(raw),
            gallery,
            total,
            cancel: AtomicBool::new(false),
            progress: Mutex::new(Progress {
                state: State::Queued,
                done: 0,
                saved: vec![],
                failed: vec![],
                error: None,
                finished_at: None,
            }),
        });
        jobs.insert(job.id.clone(), job.clone());
        Ok((job, true))
    }

    /// Queue a save of an already vetted gallery. Returns the job (an existing one when this
    /// gallery is already being saved).
    pub fn start(
        &self,
        st: &Arc<Http>,
        gallery: i32,
        total: usize,
    ) -> Result<Arc<Job>, StartError> {
        let (job, fresh) = self.register(gallery, total)?;
        if fresh {
            let st = st.clone();
            let job = job.clone();
            tokio::spawn(async move { run(st, job).await });
        }
        Ok(job)
    }
}

fn extension(ctype: &str) -> &'static str {
    match ctype.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        _ => "jpg",
    }
}

enum Ingest {
    Saved(String, bool),
    /// lucina-media refused the file (the 4xx text is not kept: it can name the file).
    Refused,
    /// lucina-media could not be reached or answered 5xx.
    Down,
}

async fn ingest(
    saves: &Saves,
    path: &std::path::Path,
    name: &str,
    mime: &str,
    album: &str,
) -> Ingest {
    let body = json!({
        "path": path,
        "name": name,
        "mime": mime,
        "source": "nclient",
        "album": album,
        "move": true,
    });
    let url = format!("{}/internal/v1/ingest", saves.media_url);
    for attempt in 0..2 {
        match saves.http.post(&url).json(&body).send().await {
            Ok(r) if r.status().is_success() => {
                let v: Value = r.json().await.unwrap_or(Value::Null);
                return match v["item_id"].as_str() {
                    Some(id) => {
                        Ingest::Saved(id.to_string(), v["duplicate"].as_bool() == Some(true))
                    }
                    None => Ingest::Refused,
                };
            }
            Ok(r) if r.status().is_client_error() => return Ingest::Refused,
            _ => {
                if attempt == 0 {
                    tokio::time::sleep(saves.retry_delay).await;
                }
            }
        }
    }
    Ingest::Down
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// One page, with a few tries on upstream trouble. `Err` carries the short error code.
async fn fetch_page(
    st: &Http,
    gallery: i32,
    index: usize,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, String), String> {
    let mut last = "upstream_unavailable".to_string();
    for attempt in 0..PAGE_TRIES {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        match st.service.image(gallery, ImageKind::Page(index)).await {
            Ok(v) => return Ok(v),
            // Blocked, gone or no such page: nothing to retry.
            Err(ServiceError::NotFound) => return Err("not_found".into()),
            Err(ServiceError::Upstream(UpstreamError::RateLimited(s))) => {
                last = "rate_limited".into();
                if attempt + 1 < PAGE_TRIES {
                    tokio::time::sleep(Duration::from_secs(s.clamp(1, 60))).await;
                }
            }
            Err(_) => {
                last = "upstream_unavailable".into();
                if attempt + 1 < PAGE_TRIES {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
    Err(last)
}

async fn run(st: Arc<Http>, job: Arc<Job>) {
    let saves = &st.saves;
    let Ok(_turn) = saves.turn.acquire().await else {
        job.fail("unavailable");
        return;
    };
    if job.cancel.load(Ordering::Relaxed) {
        job.set_state(State::Cancelled);
        return;
    }
    job.set_state(State::Running);
    // Vet again: the album name comes from the vetted detail, and a gallery blocked since the
    // request stops here.
    let detail = match st.service.gallery(job.gallery).await {
        Ok(d) => d,
        Err(ServiceError::NotFound) => {
            job.fail("not_found");
            return;
        }
        Err(_) => {
            job.fail("upstream_unavailable");
            return;
        }
    };
    let album = detail.title.clone();
    let dir = saves.spool.join(&job.id);
    if std::fs::create_dir_all(&dir).is_err() {
        job.fail("spool_unavailable");
        return;
    }
    let width = job.total.to_string().len().max(3);
    let mut media_failures = 0usize;
    let mut cancelled = false;
    for index in 0..job.total {
        if job.cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let outcome = fetch_page(&st, job.gallery, index, &job.cancel).await;
        let (bytes, ctype) = match outcome {
            Ok(v) => v,
            Err(e) if e == "cancelled" => {
                cancelled = true;
                break;
            }
            Err(e) if e == "not_found" => {
                // Blocked (or removed) mid-way: stop; the pages already ingested stay.
                job.fail("not_found");
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
            Err(e) => {
                let mut p = job.progress.lock().unwrap();
                p.failed.push(FailedPage { index, error: e });
                p.done += 1;
                continue;
            }
        };
        let ext = extension(&ctype);
        let name = format!("{:0width$}.{ext}", index + 1);
        let path = dir.join(&name);
        if write_private(&path, &bytes).is_err() {
            let mut p = job.progress.lock().unwrap();
            p.failed.push(FailedPage {
                index,
                error: "spool_unavailable".into(),
            });
            p.done += 1;
            continue;
        }
        drop(bytes);
        let mime = format!("image/{}", if ext == "jpg" { "jpeg" } else { ext });
        let res = ingest(saves, &path, &name, &mime, &album).await;
        let _ = std::fs::remove_file(&path); // still there only when the ingest did not take it
        let mut p = job.progress.lock().unwrap();
        p.done += 1;
        match res {
            Ingest::Saved(item_id, duplicate) => {
                media_failures = 0;
                p.saved.push(SavedPage {
                    index,
                    item_id,
                    duplicate,
                });
            }
            Ingest::Refused => {
                media_failures = 0;
                p.failed.push(FailedPage {
                    index,
                    error: "refused".into(),
                });
            }
            Ingest::Down => {
                media_failures += 1;
                p.failed.push(FailedPage {
                    index,
                    error: "media_unavailable".into(),
                });
                if media_failures >= MEDIA_DOWN_AFTER {
                    p.state = State::Failed;
                    p.error = Some("media_unavailable".into());
                    p.finished_at = Some(std::time::Instant::now());
                    drop(p);
                    let _ = std::fs::remove_dir_all(&dir);
                    return;
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    let mut p = job.progress.lock().unwrap();
    p.finished_at = Some(std::time::Instant::now());
    p.state = if cancelled {
        State::Cancelled
    } else if p.saved.is_empty() && !p.failed.is_empty() {
        p.error = Some("nothing_saved".into());
        State::Failed
    } else {
        State::Done
    };
}
