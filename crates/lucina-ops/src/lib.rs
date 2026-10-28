//! Lucina's ops record: the keeper of the record of all of the owner's
//! projects.
//!
//! Every Claude session, agent and Lucina herself reports deploys, incidents,
//! decisions, status and findings through [`Ops::record`]. A record is
//! validated, redacted ([`lucina_secrets::redactor`]) and de-duplicated, then
//!
//! 1. appended to the store (`<state_dir>/records.jsonl`, the source of truth),
//! 2. inserted in the day's log (`Log/YYYY-MM-DD.md`) and its project page
//!    (`Projects/<project>.md`) of the Obsidian folder ([`Vault`]), and for
//!    incidents written as an incident page (`Incidents/…`),
//! 3. sent as a message to a dedicated Honcho session (`lucina_ops`), so
//!    Lucina can answer "what changed on X" and "why did we decide Y"
//!    ([`Ops::search`]). A Honcho failure leaves the record `pending`;
//!    [`Ops::sync_pending`] retries.
//!
//! Store and vault writes happen under one advisory lock (`ops.lock`): the
//! server and the CLI may both write. Syncthing conflict copies are moved to
//! `Conflicts/` and the entries they held are put back
//! ([`Ops::sweep_conflicts`]).

pub mod record;
pub mod render;
pub mod store;
pub mod vault;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use lucina_memory::{HonchoClient, NewMessage};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

pub use record::{DecidedBy, Kind, NewRecord, Paths, Recorded, Severity, StoredRecord};
pub use vault::{Vault, VaultError};

use render::ProjectInsert;
use store::{Lock, Store};

pub const DEFAULT_HONCHO_SESSION: &str = "lucina_ops";
pub const DEFAULT_HONCHO_PEER: &str = "lucina-ops";
/// Records [`Ops::sync_pending`] sends per call.
pub const SYNC_BATCH: usize = 50;
pub const MAX_QUERY_CHARS: usize = 500;
pub const MAX_SEARCH_LIMIT: u32 = 50;
const SEARCH_BODY_CHARS: usize = 1500;
/// Score of a Honcho hit (the first; later hits a little less).
const HONCHO_BONUS: f64 = 5.0;

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error("{field}: {message}")]
    Invalid {
        field: &'static str,
        message: String,
    },
    #[error("no such record")]
    NotFound,
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("ops store: {0}")]
    Io(String),
}

impl OpsError {
    pub fn code(&self) -> &'static str {
        match self {
            OpsError::Invalid { .. } => "bad_request",
            OpsError::NotFound => "record_not_found",
            OpsError::Vault(v) => v.code(),
            OpsError::Io(_) => "internal_error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpsConfig {
    /// The synced folder itself (`…/lucina-vault/Lucina`).
    pub vault_dir: PathBuf,
    /// `records.jsonl` and `ops.lock`.
    pub state_dir: PathBuf,
    /// The owner's time zone (log dates and times).
    pub time_zone: Tz,
    pub honcho_session: String,
    pub honcho_peer: String,
    /// Records go to Honcho (memory mode `readwrite`); search reads it either way.
    pub honcho_write: bool,
}

impl OpsConfig {
    pub fn new(
        vault_dir: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
        time_zone: Tz,
    ) -> Self {
        Self {
            vault_dir: vault_dir.into(),
            state_dir: state_dir.into(),
            time_zone,
            honcho_session: DEFAULT_HONCHO_SESSION.into(),
            honcho_peer: DEFAULT_HONCHO_PEER.into(),
            honcho_write: true,
        }
    }
}

/// `ops_search` / `GET /ops/search`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchQuery {
    pub q: String,
    pub project: Option<String>,
    pub kind: Option<Kind>,
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    /// 1..=50, default 10.
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchItem {
    pub id: String,
    pub kind: Kind,
    pub project: String,
    pub title: String,
    pub body: String,
    pub cause: Option<String>,
    pub fix: Option<String>,
    pub follow_ups: Vec<String>,
    pub at: String,
    pub commit: Option<String>,
    pub host: Option<String>,
    pub severity: Option<Severity>,
    pub source: String,
    pub links: Vec<String>,
    pub paths: Paths,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<DecidedBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_by: Option<String>,
    pub score: f64,
    pub matched: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub items: Vec<SearchItem>,
    /// `ok` | `off` | `error`
    pub honcho: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub attempted: usize,
    pub synced: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedConflict {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepReport {
    pub moved: Vec<MovedConflict>,
    /// Records whose missing entries were put back.
    pub reinserted: usize,
}

/// The ops record.
pub struct Ops {
    cfg: OpsConfig,
    vault: Vault,
    store: Store,
    honcho: Option<HonchoClient>,
    peers_added: AtomicBool,
}

impl std::fmt::Debug for Ops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ops")
            .field("vault", &self.vault.root())
            .field("state_dir", &self.cfg.state_dir)
            .field("honcho", &self.honcho.is_some())
            .finish()
    }
}

fn hex16(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Ops {
    /// Open the vault (it must exist) and the store (created when missing).
    pub fn open(cfg: OpsConfig, honcho: Option<HonchoClient>) -> Result<Self, OpsError> {
        let vault = Vault::open(&cfg.vault_dir)?;
        let store = Store::open(&cfg.state_dir)?;
        Ok(Self {
            cfg,
            vault,
            store,
            honcho,
            peers_added: AtomicBool::new(false),
        })
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn config(&self) -> &OpsConfig {
        &self.cfg
    }

    fn honcho_writer(&self) -> Option<&HonchoClient> {
        self.honcho.as_ref().filter(|_| self.cfg.honcho_write)
    }

    fn honcho_state(&self, r: &StoredRecord) -> &'static str {
        if r.honcho_synced {
            "ok"
        } else if self.honcho_writer().is_some() {
            "pending"
        } else {
            "off"
        }
    }

    /// The projects that have records, sorted.
    pub fn projects(&self) -> Result<Vec<String>, OpsError> {
        self.store.refresh()?;
        let mut v: Vec<String> = self.store.all().into_iter().map(|r| r.project).collect();
        v.sort();
        v.dedup();
        Ok(v)
    }

    pub fn get(&self, id: &str) -> Result<Option<StoredRecord>, OpsError> {
        if !record::is_record_id(id) {
            return Ok(None);
        }
        self.store.refresh()?;
        Ok(self.store.get(id))
    }

    /// Record one event. `source`: who reports (`mcp:claude`, `lucina`, `cli`, `app`).
    pub async fn record(&self, new: NewRecord, source: &str) -> Result<Recorded, OpsError> {
        self.record_at(new, source, Utc::now()).await
    }

    /// [`Ops::record`] with an explicit clock (tests).
    pub async fn record_at(
        &self,
        new: NewRecord,
        source: &str,
        now: DateTime<Utc>,
    ) -> Result<Recorded, OpsError> {
        let mut v = record::validate(new, now)?;
        let redacted = record::redact(&mut v)?;
        let source = clean_source(source);
        let key = record::dedup_key(
            v.kind,
            &v.project,
            &v.title,
            v.commit.as_deref(),
            v.dedup_key.as_deref(),
        );
        let id = record::record_id(&key);
        let rec = {
            let lock = self.store.lock()?;
            self.store.refresh()?;
            if let Some(existing) = self.store.get(&id) {
                return Ok(Recorded {
                    id,
                    duplicate: true,
                    redacted,
                    honcho: self.honcho_state(&existing).into(),
                    paths: existing.paths,
                });
            }
            let at = v.at.with_timezone(&self.cfg.time_zone).fixed_offset();
            let mut rec = StoredRecord {
                id: id.clone(),
                kind: v.kind,
                project: v.project,
                title: v.title,
                body: v.body,
                links: v.links,
                commit: v.commit,
                host: v.host,
                severity: v.severity,
                timeline: v.timeline,
                cause: v.cause,
                fix: v.fix,
                follow_ups: v.follow_ups,
                at,
                recorded_at: now,
                source,
                dedup_key: v.dedup_key,
                decided_by: v.decided_by,
                question: v.question,
                asked_by: v.asked_by,
                redacted,
                honcho_synced: false,
                paths: Paths {
                    log: render::log_path(&at),
                    project: render::project_path(""),
                    incident: None,
                },
            };
            rec.paths.project = render::project_path(&rec.project);
            if rec.kind == Kind::Incident {
                rec.paths.incident = Some(self.free_incident_path(&rec)?);
            }
            // Conflict copies of the touched pages first (their repair must
            // not see this record yet), then the store, then the pages.
            self.sweep_pages(&lock, &rec, now);
            self.store.append(&lock, &rec)?;
            self.write_pages(&rec)?;
            rec
        };
        let honcho = match self.honcho_writer() {
            None => "off",
            Some(client) => match self.send_honcho(client, &rec).await {
                Ok(()) => {
                    let lock = self.store.lock()?;
                    self.store
                        .mark_synced(&lock, std::slice::from_ref(&rec.id))?;
                    "ok"
                }
                Err(e) => {
                    tracing::warn!(id = %rec.id, error = %e, "ops: Honcho write failed; record pending");
                    "pending"
                }
            },
        };
        Ok(Recorded {
            id,
            duplicate: false,
            redacted,
            honcho: honcho.into(),
            paths: rec.paths,
        })
    }

    /// An incident page name not taken by another record.
    fn free_incident_path(&self, rec: &StoredRecord) -> Result<String, OpsError> {
        for n in 1..1000 {
            let p = render::incident_path(&rec.at, &rec.project, &rec.title, n);
            match self.vault.read(&p)? {
                None => return Ok(p),
                Some(page) if render::incident_owner(&page) == Some(rec.id.as_str()) => {
                    return Ok(p)
                }
                Some(_) => {}
            }
        }
        Err(OpsError::Io("no free incident page name".into()))
    }

    fn sweep_pages(&self, lock: &Lock, r: &StoredRecord, now: DateTime<Utc>) {
        let mut pages = vec![r.paths.log.clone(), r.paths.project.clone()];
        pages.extend(r.paths.incident.clone());
        for p in &pages {
            if let Err(e) = self.sweep_page(lock, p, now) {
                tracing::warn!(page = %p, error = %e, "ops: conflict check failed");
            }
        }
    }

    /// The vault writes of a new record (under the lock): the log, the
    /// project page, the incident page.
    fn write_pages(&self, r: &StoredRecord) -> Result<(), OpsError> {
        self.insert_log(r)?;
        let page = self.vault.read(&r.paths.project)?;
        let updated =
            render::update_project_page(page.as_deref(), &r.project, &ProjectInsert::all(r));
        self.vault.write(&r.paths.project, &updated)?;
        if let Some(inc) = &r.paths.incident {
            self.vault.write(inc, &render::incident_page(r))?;
        }
        Ok(())
    }

    fn insert_log(&self, r: &StoredRecord) -> Result<(), OpsError> {
        let page = self.vault.read(&r.paths.log)?;
        let updated = render::insert_log_entry(
            page.as_deref(),
            &render::log_header(&r.at),
            &r.at.format("%H:%M").to_string(),
            &render::log_entry(r),
        );
        self.vault.write(&r.paths.log, &updated)?;
        Ok(())
    }

    async fn send_honcho(&self, client: &HonchoClient, r: &StoredRecord) -> Result<(), String> {
        let (session, peer) = (&self.cfg.honcho_session, &self.cfg.honcho_peer);
        client.ensure_peer(peer).await.map_err(|e| e.to_string())?;
        client
            .ensure_session(session)
            .await
            .map_err(|e| e.to_string())?;
        if !self.peers_added.load(Ordering::Relaxed) {
            client
                .add_peers(session, &[peer.as_str()])
                .await
                .map_err(|e| e.to_string())?;
            self.peers_added.store(true, Ordering::Relaxed);
        }
        let msg = NewMessage {
            peer_id: peer.clone(),
            content: render::honcho_content(r),
            metadata: json!({
                "ops_id": r.id,
                "kind": r.kind.as_str(),
                "project": r.project,
                "severity": r.severity.map(Severity::as_str),
                "commit": r.commit,
                "host": r.host,
                "source": r.source,
                "at": r.at.to_rfc3339(),
                "decided_by": r.decided_by.map(record::DecidedBy::as_str),
                "asked_by": r.asked_by,
            }),
        };
        client
            .add_messages(session, &[msg])
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Send records Honcho has not got yet (oldest first, at most 50).
    pub async fn sync_pending(&self) -> Result<SyncReport, OpsError> {
        let Some(client) = self.honcho_writer() else {
            return Ok(SyncReport::default());
        };
        self.store.refresh()?;
        let mut pending: Vec<StoredRecord> = self
            .store
            .all()
            .into_iter()
            .filter(|r| !r.honcho_synced)
            .collect();
        pending.sort_by_key(|r| r.recorded_at);
        pending.truncate(SYNC_BATCH);
        let mut report = SyncReport {
            attempted: pending.len(),
            ..Default::default()
        };
        let mut synced = Vec::new();
        for r in &pending {
            match self.send_honcho(client, r).await {
                Ok(()) => synced.push(r.id.clone()),
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(id = %r.id, error = %e, "ops: Honcho sync failed");
                    // Honcho is down: the rest waits for the next round.
                    break;
                }
            }
        }
        report.synced = synced.len();
        report.failed = report.attempted - report.synced;
        if !synced.is_empty() {
            let lock = self.store.lock()?;
            self.store.mark_synced(&lock, &synced)?;
        }
        Ok(report)
    }

    // ------------------------------------------------------------ conflicts

    /// Move every Syncthing conflict copy in the vault to `Conflicts/` and put
    /// back the entries its page lost.
    pub fn sweep_conflicts(&self) -> Result<SweepReport, OpsError> {
        self.sweep_conflicts_at(Utc::now())
    }

    pub fn sweep_conflicts_at(&self, now: DateTime<Utc>) -> Result<SweepReport, OpsError> {
        let lock = self.store.lock()?;
        self.store.refresh()?;
        let mut found = Vec::new();
        self.walk("", &mut found)?;
        let mut report = SweepReport::default();
        for (rel, original) in found {
            self.handle_conflict(&lock, &rel, &original, now, &mut report)?;
        }
        Ok(report)
    }

    /// Conflict copies of one page (the pages a record touches).
    fn sweep_page(&self, lock: &Lock, page: &str, now: DateTime<Utc>) -> Result<(), OpsError> {
        let (dir, name) = page.rsplit_once('/').unwrap_or(("", page));
        let stem = name.strip_suffix(".md").unwrap_or(name);
        let Some(entries) = self.vault.list_dir(dir)? else {
            return Ok(());
        };
        let mut report = SweepReport::default();
        for (n, kind) in entries {
            if kind != vault::Entry::File {
                continue;
            }
            if conflict_original(&n).as_deref() == Some(name) && n.starts_with(stem) {
                let rel = if dir.is_empty() {
                    n
                } else {
                    format!("{dir}/{n}")
                };
                self.handle_conflict(lock, &rel, page, now, &mut report)?;
            }
        }
        Ok(())
    }

    fn walk(&self, dir: &str, out: &mut Vec<(String, String)>) -> Result<(), OpsError> {
        let Some(entries) = self.vault.list_dir(dir)? else {
            return Ok(());
        };
        for (name, kind) in entries {
            if name.starts_with('.') || (dir.is_empty() && name == vault::CONFLICTS_DIR) {
                continue;
            }
            let rel = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            match kind {
                vault::Entry::Symlink => {
                    tracing::warn!(path = %rel, "ops: symlink in the vault skipped");
                }
                vault::Entry::Dir => self.walk(&rel, out)?,
                vault::Entry::File => {
                    if let Some(orig) = conflict_original(&name) {
                        let orig = if dir.is_empty() {
                            orig
                        } else {
                            format!("{dir}/{orig}")
                        };
                        out.push((rel, orig));
                    }
                }
                vault::Entry::Other => {}
            }
        }
        Ok(())
    }

    fn handle_conflict(
        &self,
        _lock: &Lock,
        rel: &str,
        original: &str,
        now: DateTime<Utc>,
        report: &mut SweepReport,
    ) -> Result<(), OpsError> {
        let local = now.with_timezone(&self.cfg.time_zone).fixed_offset();
        let dest = format!(
            "{}/{}/{}",
            vault::CONFLICTS_DIR,
            local.format("%Y-%m-%d"),
            rel.replace('/', "__")
        );
        let to = self.vault.rename(rel, &dest)?;
        tracing::info!(from = %rel, to = %to, "ops: sync conflict moved");
        report.moved.push(MovedConflict {
            from: rel.to_owned(),
            to: to.clone(),
        });
        let managed = [vault::LOG_DIR, vault::PROJECTS_DIR, vault::INCIDENTS_DIR]
            .iter()
            .any(|d| original.starts_with(&format!("{d}/")));
        if managed {
            report.reinserted += self.repair_page(original)?;
        }
        // One line in today's log (not a store record).
        let log = render::log_path(&local);
        let line = format!(
            "- {} **status** · sync conflict on {}: the other copy is in [[{}]] · lucina {}",
            local.format("%H:%M"),
            original,
            render::link_target(&to),
            render::marker(&format!("conflict-{}", hex16(&to)))
        );
        let page = self.vault.read(&log)?;
        let updated = render::insert_log_entry(
            page.as_deref(),
            &render::log_header(&local),
            &local.format("%H:%M").to_string(),
            &[line],
        );
        self.vault.write(&log, &updated)?;
        Ok(())
    }

    /// Put back the store's entries a page lacks. Returns how many records
    /// needed it.
    fn repair_page(&self, page: &str) -> Result<usize, OpsError> {
        let mut records: Vec<StoredRecord> = self
            .store
            .all()
            .into_iter()
            .filter(|r| {
                r.paths.log == page
                    || r.paths.project == page
                    || r.paths.incident.as_deref() == Some(page)
            })
            .collect();
        records.sort_by_key(|r| r.at);
        let mut n = 0;
        for r in &records {
            let current = self.vault.read(page)?;
            let text = current.as_deref().unwrap_or("");
            if r.paths.log == page {
                if !text.contains(&render::marker(&r.id)) {
                    self.insert_log(r)?;
                    n += 1;
                }
            } else if r.paths.project == page {
                let ins = ProjectInsert::missing(r, text);
                if !ins.is_empty() {
                    let updated = render::update_project_page(current.as_deref(), &r.project, &ins);
                    self.vault.write(page, &updated)?;
                    n += 1;
                }
            } else if !text.contains(&render::marker(&r.id)) {
                self.vault.write(page, &render::incident_page(r))?;
                n += 1;
            }
        }
        Ok(n)
    }

    // ------------------------------------------------------------ search

    pub async fn search(&self, q: SearchQuery) -> Result<SearchResult, OpsError> {
        let text = q.q.trim().to_owned();
        if text.chars().count() > MAX_QUERY_CHARS {
            return Err(OpsError::Invalid {
                field: "q",
                message: format!("must be at most {MAX_QUERY_CHARS} characters"),
            });
        }
        let limit = q.limit.unwrap_or(10);
        if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
            return Err(OpsError::Invalid {
                field: "limit",
                message: format!("must be 1 to {MAX_SEARCH_LIMIT}"),
            });
        }
        let project = q
            .project
            .as_deref()
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty());
        self.store.refresh()?;
        let records: Vec<StoredRecord> = self
            .store
            .all()
            .into_iter()
            .filter(|r| project.as_deref().is_none_or(|p| r.project == p))
            .filter(|r| q.kind.is_none_or(|k| r.kind == k))
            .filter(|r| q.since.is_none_or(|d| r.at.date_naive() >= d))
            .filter(|r| q.until.is_none_or(|d| r.at.date_naive() <= d))
            .collect();
        let terms = tokens(&text);
        let mut scored: HashMap<String, (f64, Vec<String>)> = HashMap::new();
        for r in &records {
            let s = if terms.is_empty() {
                0.0
            } else {
                local_score(r, &terms)
            };
            if terms.is_empty() || s > 0.0 {
                scored.insert(r.id.clone(), (s, vec!["vault".into()]));
            }
        }
        let honcho = match (&self.honcho, text.is_empty()) {
            (None, _) | (_, true) => "off",
            (Some(client), false) => {
                match client
                    .search_session(&self.cfg.honcho_session, &text, limit)
                    .await
                {
                    Ok(hits) => {
                        let ids: HashMap<&str, ()> =
                            records.iter().map(|r| (r.id.as_str(), ())).collect();
                        for (rank, h) in hits.iter().enumerate() {
                            let Some(id) = h
                                .metadata
                                .as_ref()
                                .and_then(|m| m.get("ops_id"))
                                .and_then(|v| v.as_str())
                            else {
                                continue;
                            };
                            if !ids.contains_key(id) {
                                continue;
                            }
                            let bonus = (HONCHO_BONUS - rank as f64 * 0.1).max(1.0);
                            let e = scored.entry(id.to_owned()).or_insert((0.0, Vec::new()));
                            if !e.1.iter().any(|m| m == "honcho") {
                                e.0 += bonus;
                                e.1.push("honcho".into());
                            }
                        }
                        "ok"
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ops: Honcho search failed");
                        "error"
                    }
                }
            }
        };
        let mut items: Vec<SearchItem> = records
            .into_iter()
            .filter_map(|r| {
                let (score, matched) = scored.remove(&r.id)?;
                Some(SearchItem {
                    id: r.id,
                    kind: r.kind,
                    project: r.project,
                    title: r.title,
                    body: render::clip(&r.body, SEARCH_BODY_CHARS),
                    cause: r.cause,
                    fix: r.fix,
                    follow_ups: r.follow_ups,
                    at: r.at.to_rfc3339(),
                    commit: r.commit,
                    host: r.host,
                    severity: r.severity,
                    source: r.source,
                    links: r.links,
                    paths: r.paths,
                    decided_by: r.decided_by,
                    question: r.question,
                    asked_by: r.asked_by,
                    score,
                    matched,
                })
            })
            .collect();
        items.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.at.cmp(&a.at))
        });
        items.truncate(limit as usize);
        Ok(SearchResult {
            items,
            honcho: honcho.into(),
        })
    }
}

/// `x.sync-conflict-20261028-140000-ABCDEFG.md` -> `x.md`.
pub fn conflict_original(name: &str) -> Option<String> {
    let base = name.strip_suffix(".md")?;
    let i = base.rfind(".sync-conflict-")?;
    let (stem, rest) = (&base[..i], &base[i + ".sync-conflict-".len()..]);
    let mut parts = rest.split('-');
    let (d, t, dev) = (parts.next()?, parts.next()?, parts.next()?);
    let ok = !stem.is_empty()
        && parts.next().is_none()
        && d.len() == 8
        && d.bytes().all(|b| b.is_ascii_digit())
        && t.len() == 6
        && t.bytes().all(|b| b.is_ascii_digit())
        && dev.len() == 7
        && dev
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    ok.then(|| format!("{stem}.md"))
}

/// `[a-z0-9:-]{1,40}`, else `app`.
pub fn clean_source(s: &str) -> String {
    let s = s.trim();
    if (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b':' || b == b'-')
    {
        s.to_owned()
    } else {
        "app".into()
    }
}

/// Lowercased, ASCII-folded words.
fn tokens(s: &str) -> Vec<String> {
    let folded: String = s.to_lowercase().chars().map(render::fold_char).collect();
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

fn count(field: &str, terms: &[String]) -> f64 {
    let toks = tokens(field);
    terms
        .iter()
        .map(|t| toks.iter().filter(|w| w.starts_with(t.as_str())).count())
        .sum::<usize>() as f64
}

fn local_score(r: &StoredRecord, terms: &[String]) -> f64 {
    let mut rest = r.body.clone();
    for s in r
        .cause
        .iter()
        .chain(r.fix.iter())
        .chain(r.follow_ups.iter())
        .chain(r.links.iter())
        .chain(r.question.iter())
        .chain(r.asked_by.iter())
    {
        rest.push(' ');
        rest.push_str(s);
    }
    3.0 * count(&r.title, terms) + 2.0 * count(&r.project, terms) + count(&rest, terms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_names() {
        assert_eq!(
            conflict_original("x.sync-conflict-20261028-140000-ABCDEFG.md").as_deref(),
            Some("x.md")
        );
        assert_eq!(
            conflict_original("2026-10-28.sync-conflict-20261028-140000-AB12CDE.md").as_deref(),
            Some("2026-10-28.md")
        );
        for bad in [
            "x.md",
            "x.sync-conflict-2026102-140000-ABCDEFG.md",
            "x.sync-conflict-20261028-140000-abcdefg.md",
            ".sync-conflict-20261028-140000-ABCDEFG.md",
            "x.sync-conflict-20261028-140000-ABCDEFG.txt",
        ] {
            assert!(conflict_original(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn sources() {
        assert_eq!(clean_source("mcp:claude"), "mcp:claude");
        assert_eq!(clean_source("Evil Name"), "app");
        assert_eq!(clean_source(""), "app");
    }

    #[test]
    fn scoring() {
        assert_eq!(
            tokens("Perché il Deploy-Lucina?"),
            ["perche", "il", "deploy", "lucina"]
        );
    }
}
