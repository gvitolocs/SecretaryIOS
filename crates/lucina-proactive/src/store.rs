//! Durable state: [`PgStore`] (schema `lucina_proactive` in the grocery
//! Postgres) and [`MemStore`] (tests and the 24-hour replay).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;
use uuid::Uuid;

use crate::model::{
    Action, Candidate, Decision, Event, Feedback, FeedbackAction, Kind, Outreach, Priority, Scores,
};

/// Held for a whole tick; dropping it releases the lock.
pub struct TickLock(#[allow(dead_code)] Box<dyn std::any::Any + Send>);

/// `pg_try_advisory_xact_lock` key of the tick lock (the migrations use
/// 7461356).
pub const TICK_LOCK_KEY: i64 = 7_461_357;

#[async_trait::async_trait]
pub trait Store: Send + Sync {
    /// One tick at a time across processes (the `run` daemon and a `once`
    /// run): `None` when another tick holds the lock. The attention budget
    /// and cooldown read the ledger and then send, so two ticks at once
    /// could both pass the gate (docs/verification.md, G3).
    async fn try_tick_lock(&self) -> Result<Option<TickLock>>;
    /// Insert a new event; `false` when its dedup key was seen before.
    async fn insert_event(&self, e: &Event) -> Result<bool>;
    /// The candidate of the event with this dedup key, if it was decided.
    async fn candidate_by_key(&self, dedup_key: &str) -> Result<Option<Candidate>>;
    /// Insert or update (by id).
    async fn save_candidate(&self, c: &Candidate) -> Result<()>;
    /// Every held candidate with its event.
    async fn held(&self) -> Result<Vec<(Candidate, Event)>>;
    /// Pulse material: digest candidates decided since `since`, not yet pulsed.
    async fn digests(&self, since: DateTime<Utc>) -> Result<Vec<(Candidate, Event)>>;
    async fn record_outreach(&self, o: &Outreach) -> Result<()>;
    async fn outreach(&self, id: &str) -> Result<Option<Outreach>>;
    /// Unsolicited outreach since `since` (the daily budget).
    async fn unsolicited_since(&self, since: DateTime<Utc>) -> Result<u32>;
    async fn last_outreach_of_kind(&self, kind: Kind) -> Result<Option<DateTime<Utc>>>;
    /// `false` when it was already recorded (same external id) or the
    /// outreach is unknown.
    async fn record_feedback(&self, f: &Feedback) -> Result<bool>;
    async fn last_feedback(
        &self,
        kind: Kind,
        action: FeedbackAction,
    ) -> Result<Option<DateTime<Utc>>>;
    async fn get_state(&self, source: &str, key: &str) -> Result<Option<Value>>;
    async fn put_state(
        &self,
        source: &str,
        key: &str,
        state: &Value,
        now: DateTime<Utc>,
    ) -> Result<()>;
    async fn outreach_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Outreach>>;
    async fn decisions_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<(Candidate, Event)>>;
}

// ------------------------------------------------------------------ memory

#[derive(Default)]
struct Mem {
    events: HashMap<Uuid, Event>,
    keys: HashMap<String, Uuid>,
    candidates: HashMap<Uuid, Candidate>,
    outreach: Vec<Outreach>,
    feedback: Vec<Feedback>,
    state: HashMap<(String, String), Value>,
}

#[derive(Default)]
pub struct MemStore {
    m: Mutex<Mem>,
    tick: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Mem> {
        self.m.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn with_events(m: &Mem, f: impl Fn(&Candidate) -> bool) -> Vec<(Candidate, Event)> {
        let mut out: Vec<(Candidate, Event)> = m
            .candidates
            .values()
            .filter(|c| f(c))
            .filter_map(|c| {
                m.events.get(&c.event_id).map(|e| {
                    let mut e = e.clone();
                    e.scores = c.scores;
                    (c.clone(), e)
                })
            })
            .collect();
        out.sort_by_key(|(c, _)| c.created_at);
        out
    }
}

#[async_trait::async_trait]
impl Store for MemStore {
    async fn try_tick_lock(&self) -> Result<Option<TickLock>> {
        Ok(self
            .tick
            .clone()
            .try_lock_owned()
            .ok()
            .map(|g| TickLock(Box::new(g))))
    }

    async fn insert_event(&self, e: &Event) -> Result<bool> {
        let mut m = self.lock();
        if m.keys.contains_key(&e.dedup_key) {
            return Ok(false);
        }
        m.keys.insert(e.dedup_key.clone(), e.id);
        m.events.insert(e.id, e.clone());
        Ok(true)
    }
    async fn candidate_by_key(&self, dedup_key: &str) -> Result<Option<Candidate>> {
        let m = self.lock();
        let Some(id) = m.keys.get(dedup_key) else {
            return Ok(None);
        };
        Ok(m.candidates.values().find(|c| c.event_id == *id).cloned())
    }
    async fn save_candidate(&self, c: &Candidate) -> Result<()> {
        self.lock().candidates.insert(c.id, c.clone());
        Ok(())
    }
    async fn held(&self) -> Result<Vec<(Candidate, Event)>> {
        Ok(Self::with_events(&self.lock(), |c| {
            c.decision == Decision::Hold
        }))
    }
    async fn digests(&self, since: DateTime<Utc>) -> Result<Vec<(Candidate, Event)>> {
        Ok(Self::with_events(&self.lock(), |c| {
            c.decision == Decision::Digest && c.pulse_id.is_none() && c.decided_at >= since
        }))
    }
    async fn record_outreach(&self, o: &Outreach) -> Result<()> {
        self.lock().outreach.push(o.clone());
        Ok(())
    }
    async fn outreach(&self, id: &str) -> Result<Option<Outreach>> {
        Ok(self.lock().outreach.iter().find(|o| o.id == id).cloned())
    }
    async fn unsolicited_since(&self, since: DateTime<Utc>) -> Result<u32> {
        Ok(self
            .lock()
            .outreach
            .iter()
            .filter(|o| o.unsolicited && o.sent_at >= since)
            .count() as u32)
    }
    async fn last_outreach_of_kind(&self, kind: Kind) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .lock()
            .outreach
            .iter()
            .filter(|o| o.kind == kind)
            .map(|o| o.sent_at)
            .max())
    }
    async fn record_feedback(&self, f: &Feedback) -> Result<bool> {
        let mut m = self.lock();
        if !m.outreach.iter().any(|o| o.id == f.outreach_id) {
            return Ok(false);
        }
        if f.external_id.is_some() && m.feedback.iter().any(|x| x.external_id == f.external_id) {
            return Ok(false);
        }
        m.feedback.push(f.clone());
        Ok(true)
    }
    async fn last_feedback(
        &self,
        kind: Kind,
        action: FeedbackAction,
    ) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .lock()
            .feedback
            .iter()
            .filter(|f| f.kind == kind && f.action == action)
            .map(|f| f.created_at)
            .max())
    }
    async fn get_state(&self, source: &str, key: &str) -> Result<Option<Value>> {
        Ok(self
            .lock()
            .state
            .get(&(source.to_owned(), key.to_owned()))
            .cloned())
    }
    async fn put_state(
        &self,
        source: &str,
        key: &str,
        state: &Value,
        _now: DateTime<Utc>,
    ) -> Result<()> {
        self.lock()
            .state
            .insert((source.to_owned(), key.to_owned()), state.clone());
        Ok(())
    }
    async fn outreach_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Outreach>> {
        let mut v: Vec<Outreach> = self
            .lock()
            .outreach
            .iter()
            .filter(|o| o.sent_at >= from && o.sent_at < to)
            .cloned()
            .collect();
        v.sort_by_key(|o| o.sent_at);
        Ok(v)
    }
    async fn decisions_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<(Candidate, Event)>> {
        Ok(Self::with_events(&self.lock(), |c| {
            c.created_at >= from && c.created_at < to
        }))
    }
}

// ------------------------------------------------------------------ postgres

/// Migrations in order: (version, name, SQL).
pub const MIGRATIONS: &[(i32, &str, &str)] = &[(
    1,
    "proactive",
    include_str!("../migrations/001_proactive.sql"),
)];

pub struct PgStore {
    pool: PgPool,
}

const EVENT_COLS: &str =
    "e.id AS e_id, e.source, e.kind AS e_kind, e.dedup_key, e.subject, e.signal, e.payload, \
    e.observed_at, e.expires_at AS e_expires_at";
const CAND_COLS: &str = "c.id AS c_id, c.event_id, c.kind AS c_kind, c.urgency, c.relevance, c.confidence, \
    c.actionability, c.score, c.decision, c.reason, c.priority, c.created_at, c.decided_at, c.hold_until, \
    c.expires_at AS c_expires_at, c.pulse_id";
const OUT_COLS: &str =
    "id, candidate_id, kind, channel, message, priority, actions, why, language, dry_run, \
    status, result, sent_at, timeline_id, unsolicited";

fn kind(s: &str) -> Kind {
    Kind::parse(s).unwrap_or(Kind::ThreadOpen)
}

fn candidate_from(r: &PgRow) -> Result<Candidate> {
    let scores = Scores {
        urgency: r.try_get("urgency")?,
        relevance: r.try_get("relevance")?,
        confidence: r.try_get("confidence")?,
        actionability: r.try_get("actionability")?,
    };
    Ok(Candidate {
        id: r.try_get("c_id")?,
        event_id: r.try_get("event_id")?,
        kind: kind(r.try_get::<&str, _>("c_kind")?),
        scores,
        score: r.try_get("score")?,
        decision: Decision::parse(r.try_get::<&str, _>("decision")?),
        reason: r.try_get("reason")?,
        priority: Priority::parse(r.try_get::<&str, _>("priority")?),
        created_at: r.try_get("created_at")?,
        decided_at: r.try_get("decided_at")?,
        hold_until: r.try_get("hold_until")?,
        expires_at: r.try_get("c_expires_at")?,
        pulse_id: r.try_get("pulse_id")?,
    })
}

fn event_from(r: &PgRow, scores: Scores) -> Result<Event> {
    Ok(Event {
        id: r.try_get("e_id")?,
        source: r.try_get("source")?,
        kind: kind(r.try_get::<&str, _>("e_kind")?),
        dedup_key: r.try_get("dedup_key")?,
        observed_at: r.try_get("observed_at")?,
        expires_at: r.try_get("e_expires_at")?,
        subject: r.try_get("subject")?,
        payload: r.try_get("payload")?,
        scores,
        signal: r.try_get("signal")?,
    })
}

fn outreach_from(r: &PgRow) -> Result<Outreach> {
    let actions: Value = r.try_get("actions")?;
    Ok(Outreach {
        id: r.try_get("id")?,
        candidate_id: r.try_get("candidate_id")?,
        kind: kind(r.try_get::<&str, _>("kind")?),
        channel: r.try_get("channel")?,
        message: r.try_get("message")?,
        priority: Priority::parse(r.try_get::<&str, _>("priority")?),
        actions: serde_json::from_value::<Vec<Action>>(actions).unwrap_or_default(),
        why: r.try_get("why")?,
        language: r.try_get("language")?,
        dry_run: r.try_get("dry_run")?,
        status: r.try_get("status")?,
        result: r.try_get("result")?,
        sent_at: r.try_get("sent_at")?,
        timeline_id: r.try_get("timeline_id")?,
        unsolicited: r.try_get("unsolicited")?,
    })
}

impl PgStore {
    /// A lazy pool (no connection until first use).
    pub fn connect_lazy(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .acquire_timeout(Duration::from_secs(8))
            .connect_lazy(url)
            .context("database URL")?;
        Ok(Self { pool })
    }

    pub fn with_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Apply the migrations not applied yet (one transaction each, under an
    /// advisory lock). Returns the versions applied now.
    pub async fn migrate(&self) -> Result<Vec<i32>> {
        let mut conn = self.pool.acquire().await?;
        sqlx::raw_sql(
            "CREATE SCHEMA IF NOT EXISTS lucina_proactive; \
             CREATE TABLE IF NOT EXISTS lucina_proactive.schema_migrations ( \
               version integer PRIMARY KEY, name text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())",
        )
        .execute(&mut *conn)
        .await?;
        drop(conn);
        let mut applied = Vec::new();
        for (version, name, sql) in MIGRATIONS {
            let mut tx = self.pool.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(7461356)")
                .execute(&mut *tx)
                .await?;
            let done: Option<i32> = sqlx::query_scalar(
                "SELECT version FROM lucina_proactive.schema_migrations WHERE version = $1",
            )
            .bind(version)
            .fetch_optional(&mut *tx)
            .await?;
            if done.is_some() {
                tx.rollback().await?;
                continue;
            }
            sqlx::raw_sql(sql)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("migration {version}"))?;
            sqlx::query(
                "INSERT INTO lucina_proactive.schema_migrations (version, name) VALUES ($1, $2)",
            )
            .bind(version)
            .bind(name)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            applied.push(*version);
        }
        Ok(applied)
    }

    async fn pairs(
        &self,
        filter: &str,
        binds: &[DateTime<Utc>],
    ) -> Result<Vec<(Candidate, Event)>> {
        let sql = format!(
            "SELECT {CAND_COLS}, {EVENT_COLS} FROM lucina_proactive.candidates c \
             JOIN lucina_proactive.events e ON e.id = c.event_id WHERE {filter} ORDER BY c.created_at"
        );
        let mut q = sqlx::query(&sql);
        for b in binds {
            q = q.bind(*b);
        }
        let rows = q.fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| {
                let c = candidate_from(r)?;
                let e = event_from(r, c.scores)?;
                Ok((c, e))
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl Store for PgStore {
    async fn try_tick_lock(&self) -> Result<Option<TickLock>> {
        // A transaction-level lock: it lives as long as the transaction,
        // which the guard holds open; dropping the guard rolls it back and
        // releases the lock (also when the tick fails or panics).
        let mut tx = self.pool.begin().await?;
        let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(TICK_LOCK_KEY)
            .fetch_one(&mut *tx)
            .await?;
        Ok(got.then(|| TickLock(Box::new(tx))))
    }

    async fn insert_event(&self, e: &Event) -> Result<bool> {
        let r = sqlx::query(
            "INSERT INTO lucina_proactive.events \
             (id, source, kind, dedup_key, subject, signal, payload, observed_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (dedup_key) DO NOTHING",
        )
        .bind(e.id)
        .bind(&e.source)
        .bind(e.kind.as_str())
        .bind(&e.dedup_key)
        .bind(&e.subject)
        .bind(&e.signal)
        .bind(&e.payload)
        .bind(e.observed_at)
        .bind(e.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    async fn candidate_by_key(&self, dedup_key: &str) -> Result<Option<Candidate>> {
        let sql = format!(
            "SELECT {CAND_COLS} FROM lucina_proactive.candidates c \
             JOIN lucina_proactive.events e ON e.id = c.event_id WHERE e.dedup_key = $1"
        );
        let row = sqlx::query(&sql)
            .bind(dedup_key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(candidate_from).transpose()
    }

    async fn save_candidate(&self, c: &Candidate) -> Result<()> {
        sqlx::query(
            "INSERT INTO lucina_proactive.candidates \
             (id, event_id, kind, urgency, relevance, confidence, actionability, score, decision, reason, \
              priority, created_at, decided_at, hold_until, expires_at, pulse_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
             ON CONFLICT (id) DO UPDATE SET score = EXCLUDED.score, decision = EXCLUDED.decision, \
               reason = EXCLUDED.reason, priority = EXCLUDED.priority, decided_at = EXCLUDED.decided_at, \
               hold_until = EXCLUDED.hold_until, pulse_id = EXCLUDED.pulse_id, urgency = EXCLUDED.urgency, \
               relevance = EXCLUDED.relevance, confidence = EXCLUDED.confidence, \
               actionability = EXCLUDED.actionability",
        )
        .bind(c.id)
        .bind(c.event_id)
        .bind(c.kind.as_str())
        .bind(c.scores.urgency)
        .bind(c.scores.relevance)
        .bind(c.scores.confidence)
        .bind(c.scores.actionability)
        .bind(c.score)
        .bind(c.decision.as_str())
        .bind(&c.reason)
        .bind(c.priority.as_str())
        .bind(c.created_at)
        .bind(c.decided_at)
        .bind(c.hold_until)
        .bind(c.expires_at)
        .bind(&c.pulse_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn held(&self) -> Result<Vec<(Candidate, Event)>> {
        self.pairs("c.decision = 'hold'", &[]).await
    }

    async fn digests(&self, since: DateTime<Utc>) -> Result<Vec<(Candidate, Event)>> {
        self.pairs(
            "c.decision = 'digest' AND c.pulse_id IS NULL AND c.decided_at >= $1",
            &[since],
        )
        .await
    }

    async fn record_outreach(&self, o: &Outreach) -> Result<()> {
        sqlx::query(&format!(
            "INSERT INTO lucina_proactive.outreach ({OUT_COLS}) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)"
        ))
        .bind(&o.id)
        .bind(o.candidate_id)
        .bind(o.kind.as_str())
        .bind(&o.channel)
        .bind(&o.message)
        .bind(o.priority.as_str())
        .bind(serde_json::to_value(&o.actions)?)
        .bind(&o.why)
        .bind(&o.language)
        .bind(o.dry_run)
        .bind(&o.status)
        .bind(&o.result)
        .bind(o.sent_at)
        .bind(&o.timeline_id)
        .bind(o.unsolicited)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn outreach(&self, id: &str) -> Result<Option<Outreach>> {
        let row = sqlx::query(&format!(
            "SELECT {OUT_COLS} FROM lucina_proactive.outreach WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(outreach_from).transpose()
    }

    async fn unsolicited_since(&self, since: DateTime<Utc>) -> Result<u32> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM lucina_proactive.outreach WHERE unsolicited AND sent_at >= $1",
        )
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        Ok(n.max(0) as u32)
    }

    async fn last_outreach_of_kind(&self, kind: Kind) -> Result<Option<DateTime<Utc>>> {
        Ok(
            sqlx::query_scalar(
                "SELECT max(sent_at) FROM lucina_proactive.outreach WHERE kind = $1",
            )
            .bind(kind.as_str())
            .fetch_one(&self.pool)
            .await?,
        )
    }

    async fn record_feedback(&self, f: &Feedback) -> Result<bool> {
        let r = sqlx::query(
            "INSERT INTO lucina_proactive.feedback (id, outreach_id, action, kind, source, external_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (external_id) DO NOTHING",
        )
        .bind(f.id)
        .bind(&f.outreach_id)
        .bind(f.action.as_str())
        .bind(f.kind.as_str())
        .bind(&f.source)
        .bind(&f.external_id)
        .bind(f.created_at)
        .execute(&self.pool)
        .await;
        match r {
            Ok(r) => Ok(r.rows_affected() == 1),
            // Unknown outreach (foreign key): nothing to attach it to.
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23503") => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    async fn last_feedback(
        &self,
        kind: Kind,
        action: FeedbackAction,
    ) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar(
            "SELECT max(created_at) FROM lucina_proactive.feedback WHERE kind = $1 AND action = $2",
        )
        .bind(kind.as_str())
        .bind(action.as_str())
        .fetch_one(&self.pool)
        .await?)
    }

    async fn get_state(&self, source: &str, key: &str) -> Result<Option<Value>> {
        Ok(sqlx::query_scalar(
            "SELECT state FROM lucina_proactive.source_state WHERE source = $1 AND key = $2",
        )
        .bind(source)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn put_state(
        &self,
        source: &str,
        key: &str,
        state: &Value,
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO lucina_proactive.source_state (source, key, state, updated_at) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (source, key) DO UPDATE SET state = EXCLUDED.state, updated_at = EXCLUDED.updated_at",
        )
        .bind(source)
        .bind(key)
        .bind(state)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn outreach_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Outreach>> {
        let rows = sqlx::query(&format!(
            "SELECT {OUT_COLS} FROM lucina_proactive.outreach WHERE sent_at >= $1 AND sent_at < $2 ORDER BY sent_at"
        ))
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(outreach_from).collect()
    }

    async fn decisions_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<(Candidate, Event)>> {
        self.pairs("c.created_at >= $1 AND c.created_at < $2", &[from, to])
            .await
    }
}
