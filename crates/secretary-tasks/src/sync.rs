//! The engine side of the Todoist sync (the HTTP calls live in
//! secretary-server).
//!
//! One cycle: pull (`sync_token`) → [`Engine::apply_sync`] (conflicts are
//! decided here) → push [`Engine::outbox_batch`] with the new token →
//! [`Engine::apply_sync`] again (command results, temp ids, the resulting
//! items).
//!
//! Conflict policy: a local edit not yet pushed carries the task's Todoist
//! `updated_at` from when it was made. If Todoist's copy has changed since,
//! Todoist wins: the queued edits are dropped, Todoist's version is kept and
//! a conflict note says so. A command Todoist rejects (4xx in
//! `sync_status`) is dropped with a note and the next sync is a full one,
//! so the local copy returns to Todoist's truth. 429 and 5xx command results
//! are retried with backoff.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::models::*;
use crate::{push_conflict, Engine, MAX_COMPLETED, MAX_TRASH};

type Result<T> = std::result::Result<T, String>;

/// At most this many commands per request (Todoist's limit).
pub const MAX_BATCH: usize = 100;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub full: bool,
    /// Something a client sees changed (the revision moved).
    pub changed: bool,
    pub pushed_ok: usize,
    pub rejected: usize,
    pub retried: usize,
    pub conflicts: usize,
}

/// Equal except for sync bookkeeping (tokens, timestamps), so a sync that
/// brings nothing new does not move the revision.
pub fn same_content(a: &State, b: &State) -> bool {
    let norm = |s: &State| {
        let mut x = s.clone();
        x.revision = 0;
        x.sync.sync_token = None;
        x.sync.last_sync_ms = None;
        x.sync.last_full_sync_ms = None;
        x.receipts.clear();
        x
    };
    a.receipts.len() == b.receipts.len() && norm(a) == norm(b)
}

fn s(v: &Value, k: &str) -> Option<String> {
    match v.get(k)? {
        Value::String(x) => Some(x.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn b(v: &Value, k: &str) -> bool {
    match v.get(k) {
        Some(Value::Bool(x)) => *x,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

fn due_of(v: &Value) -> Option<Due> {
    let d = v.get("due")?;
    if d.is_null() {
        return None;
    }
    Some(Due {
        date: s(d, "date")?,
        string: s(d, "string").unwrap_or_default(),
        lang: s(d, "lang").unwrap_or_default(),
        is_recurring: b(d, "is_recurring"),
        timezone: s(d, "timezone"),
    })
}

/// A Todoist item object → a task.
pub fn task_of(v: &Value) -> Option<Task> {
    Some(Task {
        id: s(v, "id")?,
        project_id: s(v, "project_id").unwrap_or_default(),
        section_id: s(v, "section_id").filter(|x| !x.is_empty()),
        parent_id: s(v, "parent_id").filter(|x| !x.is_empty()),
        content: s(v, "content").unwrap_or_default(),
        description: s(v, "description").unwrap_or_default(),
        priority: (i(v, "priority").clamp(1, 4)) as u8,
        due: due_of(v),
        deadline: v.get("deadline").and_then(|d| s(d, "date")),
        labels: v
            .get("labels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        child_order: i(v, "child_order"),
        day_order: i(v, "day_order"),
        checked: b(v, "checked"),
        is_collapsed: b(v, "is_collapsed"),
        duration: v.get("duration").filter(|d| !d.is_null()).and_then(|d| {
            Some(TaskDuration {
                amount: u32::try_from(d.get("amount")?.as_u64()?).ok()?,
                unit: s(d, "unit")?,
            })
        }),
        added_at: s(v, "added_at"),
        updated_at: s(v, "updated_at"),
        completed_at: s(v, "completed_at"),
        deleted_ms: None,
        provisional: false,
    })
}

fn rfc3339_ms(t: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(t)
        .ok()
        .and_then(|d| u64::try_from(d.timestamp_millis()).ok())
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn rename(st: &mut State, from: &str, to: &str) {
    if let Some(mut t) = st.tasks.remove(from) {
        t.id = to.to_owned();
        st.tasks.insert(to.to_owned(), t);
    }
    for t in st.tasks.values_mut() {
        if t.parent_id.as_deref() == Some(from) {
            t.parent_id = Some(to.to_owned());
        }
    }
    for o in &mut st.outbox {
        if o.task_id.as_deref() == Some(from) {
            o.task_id = Some(to.to_owned());
        }
        if o.temp_id.as_deref() == Some(from) {
            o.temp_id = None;
        }
        if let Some(args) = o.args.as_object_mut() {
            for k in ["id", "parent_id", "project_id", "section_id"] {
                if args.get(k).and_then(Value::as_str) == Some(from) {
                    args.insert(k.into(), json!(to));
                }
            }
        }
    }
    for c in &mut st.completed {
        if c.task_id == from {
            c.task_id = to.to_owned();
        }
        if let Some(x) = c.snapshot.as_mut().filter(|x| x.id == from) {
            x.id = to.to_owned();
        }
    }
    for c in &mut st.conflicts {
        if c.task_id.as_deref() == Some(from) {
            c.task_id = Some(to.to_owned());
        }
    }
}

fn backoff_ms(attempts: u32) -> u64 {
    (30_000u64 << attempts.min(7)).min(3_600_000)
}

impl Engine {
    pub fn sync_token(&self) -> Option<String> {
        self.state().sync.sync_token.clone()
    }

    pub fn has_ready_ops(&self, now: u64) -> bool {
        !self.outbox_batch(now).is_empty()
    }

    /// When the next queued command becomes sendable (undo windows, backoff).
    pub fn next_op_due(&self) -> Option<u64> {
        self.state().outbox.iter().map(|o| o.not_before_ms).min()
    }

    /// Todoist `commands` ready to send, in order. A task with a command
    /// still waiting (undo window, backoff) holds its later commands back.
    pub fn outbox_batch(&self, now: u64) -> Vec<Value> {
        let mut blocked: HashSet<&str> = HashSet::new();
        let mut out = Vec::new();
        for o in &self.state().outbox {
            let key = o.task_id.as_deref();
            if o.not_before_ms > now {
                if let Some(k) = key {
                    blocked.insert(k);
                }
                continue;
            }
            if key.is_some_and(|k| blocked.contains(k)) {
                continue;
            }
            let mut c = json!({"type": o.kind, "uuid": o.uuid, "args": o.args});
            if let Some(t) = &o.temp_id {
                c["temp_id"] = json!(t);
            }
            out.push(c);
            if out.len() >= MAX_BATCH {
                break;
            }
        }
        out
    }

    /// Merge one `/api/v1/sync` answer (a pull, or a push with its
    /// `sync_status` and `temp_id_mapping`).
    pub fn apply_sync(&mut self, resp: &Value, now: u64) -> Result<SyncReport> {
        let mut st = self.state().clone();
        let mut rep = SyncReport {
            full: b(resp, "full_sync"),
            ..SyncReport::default()
        };
        let mut needs_full = false;
        let mut ok_ops: Vec<Op> = Vec::new();

        // 1. command results
        if let Some(status) = resp.get("sync_status").and_then(Value::as_object) {
            for (uuid, result) in status {
                let Some(pos) = st.outbox.iter().position(|o| &o.uuid == uuid) else {
                    continue;
                };
                if result.as_str() == Some("ok") {
                    ok_ops.push(st.outbox.remove(pos));
                    rep.pushed_ok += 1;
                    continue;
                }
                let http = result
                    .get("http_code")
                    .and_then(Value::as_u64)
                    .unwrap_or(400);
                let msg = s(result, "error").unwrap_or_else(|| "rejected by Todoist".into());
                if http == 429 || http >= 500 {
                    let o = &mut st.outbox[pos];
                    o.attempts += 1;
                    let wait = result
                        .get("error_extra")
                        .and_then(|e| e.get("retry_after"))
                        .and_then(Value::as_u64)
                        .map(|x| x * 1000)
                        .unwrap_or_else(|| backoff_ms(o.attempts));
                    o.not_before_ms = now + wait;
                    o.last_error = Some(msg);
                    rep.retried += 1;
                    continue;
                }
                let op = st.outbox.remove(pos);
                rep.rejected += 1;
                needs_full = true;
                let content = op
                    .task_id
                    .as_ref()
                    .and_then(|id| st.tasks.get(id))
                    .map(|t| t.content.clone())
                    .or_else(|| {
                        op.args
                            .get("content")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                if op.kind == "item_add" {
                    // never created on Todoist: drop the local copy and its edits
                    if let Some(id) = op.temp_id.clone().or(op.task_id.clone()) {
                        st.tasks.remove(&id);
                        st.outbox
                            .retain(|o| o.task_id.as_deref() != Some(id.as_str()));
                    }
                }
                push_conflict(
                    &mut st,
                    op.task_id.as_deref(),
                    &content,
                    "rejected",
                    &format!("Todoist did not accept this change ({}): {msg}", op.kind),
                    now,
                );
                rep.conflicts += 1;
            }
        }

        // 2. temp ids
        if let Some(map) = resp.get("temp_id_mapping").and_then(Value::as_object) {
            for (tmp, real) in map {
                if let Some(real) = real.as_str() {
                    rename(&mut st, tmp, real);
                }
            }
        }
        for op in &ok_ops {
            if op.kind == "item_delete" {
                if let Some(id) = &op.task_id {
                    if let Some(mut t) = st.tasks.remove(id) {
                        t.deleted_ms.get_or_insert(now);
                        st.trash.push(t);
                    }
                    let gone = crate::descendants(&st, id);
                    for g in gone {
                        st.tasks.remove(&g);
                    }
                }
            }
        }
        if st.trash.len() > MAX_TRASH {
            let drop = st.trash.len() - MAX_TRASH;
            st.trash.drain(..drop);
        }

        // 3. items
        let items = arr(resp, "items");
        let mut seen: HashSet<String> = HashSet::new();
        for v in items {
            let Some(remote) = task_of(v) else { continue };
            let id = remote.id.clone();
            seen.insert(id.clone());
            let deleted = b(v, "is_deleted");
            let pending: Vec<Op> = st
                .outbox
                .iter()
                .filter(|o| o.task_id.as_deref() == Some(id.as_str()))
                .cloned()
                .collect();
            let local = st.tasks.get(&id).cloned();
            let content = local
                .as_ref()
                .map_or(remote.content.clone(), |t| t.content.clone());
            if deleted || remote.checked {
                let only_ours = pending.iter().all(|o| {
                    (deleted && o.kind == "item_delete") || (!deleted && o.kind == "item_close")
                });
                if !pending.is_empty() && !only_ours {
                    let (kind, msg) = if deleted {
                        (
                            "deleted_remotely",
                            "Deleted in Todoist; your edit here was dropped.",
                        )
                    } else {
                        (
                            "completed_remotely",
                            "Completed in Todoist; your edit here was dropped.",
                        )
                    };
                    push_conflict(&mut st, Some(&id), &content, kind, msg, now);
                    rep.conflicts += 1;
                }
                st.outbox
                    .retain(|o| o.task_id.as_deref() != Some(id.as_str()));
                st.tasks.remove(&id);
                if remote.checked && !deleted && !st.completed.iter().any(|c| c.task_id == id) {
                    st.completed.push(Completed {
                        task_id: id.clone(),
                        content: remote.content.clone(),
                        project_id: remote.project_id.clone(),
                        completed_ms: remote
                            .completed_at
                            .as_deref()
                            .and_then(rfc3339_ms)
                            .unwrap_or(now),
                        recurring: false,
                        source: "todoist".into(),
                        snapshot: None,
                        children: vec![],
                        previous_due: None,
                    });
                }
                continue;
            }
            if !pending.is_empty() {
                let changed = pending
                    .iter()
                    .any(|o| o.base_updated_at.is_some() && o.base_updated_at != remote.updated_at);
                if changed {
                    push_conflict(
                        &mut st,
                        Some(&id),
                        &content,
                        "changed_remotely",
                        "Changed in Todoist before your edit here was sent; Todoist's version was kept.",
                        now,
                    );
                    rep.conflicts += 1;
                    st.outbox
                        .retain(|o| o.task_id.as_deref() != Some(id.as_str()));
                    st.tasks.insert(id, remote);
                } else if local.is_none() {
                    st.tasks.insert(id, remote);
                }
                // else: Todoist has not moved; the local edit stays until pushed
                continue;
            }
            st.tasks.insert(id, remote);
        }
        if rep.full {
            // a full sync lists every active task: the rest is gone
            let gone: Vec<String> = st
                .tasks
                .values()
                .filter(|t| !t.is_local() && !seen.contains(&t.id))
                .map(|t| t.id.clone())
                .collect();
            for id in gone {
                let pending = st
                    .outbox
                    .iter()
                    .any(|o| o.task_id.as_deref() == Some(id.as_str()) && o.kind != "item_delete");
                if pending {
                    let content = st
                        .tasks
                        .get(&id)
                        .map(|t| t.content.clone())
                        .unwrap_or_default();
                    push_conflict(
                        &mut st,
                        Some(&id),
                        &content,
                        "deleted_remotely",
                        "No longer in Todoist (deleted or completed there); your edit here was dropped.",
                        now,
                    );
                    rep.conflicts += 1;
                }
                st.outbox
                    .retain(|o| o.task_id.as_deref() != Some(id.as_str()));
                st.tasks.remove(&id);
            }
        }

        // recurrence kept?
        for op in ok_ops.iter().filter(|o| o.expect_recurring) {
            if let Some(t) = op.task_id.as_ref().and_then(|id| st.tasks.get(id)) {
                if t.due
                    .as_ref()
                    .is_some_and(|d| !d.is_recurring && !d.date.is_empty())
                {
                    let c = t.content.clone();
                    let id = t.id.clone();
                    push_conflict(
                        &mut st,
                        Some(&id),
                        &c,
                        "recurrence_lost",
                        "Todoist no longer repeats this task after the change; check its date in Todoist.",
                        now,
                    );
                    rep.conflicts += 1;
                }
            }
        }

        // 4. projects, sections, labels, filters
        macro_rules! merge {
            ($key:literal, $map:ident, $build:expr, $gone:expr) => {
                if rep.full && resp.get($key).is_some() {
                    st.$map.clear();
                }
                for v in arr(resp, $key) {
                    let Some(id) = s(v, "id") else { continue };
                    if b(v, "is_deleted") || $gone(v) {
                        st.$map.remove(&id);
                    } else if let Some(x) = $build(v, id.clone()) {
                        st.$map.insert(id, x);
                    }
                }
            };
        }
        merge!(
            "projects",
            projects,
            |v: &Value, id: String| Some(Project {
                id,
                name: s(v, "name")?,
                color: s(v, "color").unwrap_or_default(),
                parent_id: s(v, "parent_id").filter(|x| !x.is_empty()),
                child_order: i(v, "child_order"),
                inbox_project: b(v, "inbox_project"),
                is_favorite: b(v, "is_favorite"),
                view_style: s(v, "view_style").unwrap_or_default(),
            }),
            |v: &Value| b(v, "is_archived")
        );
        merge!(
            "sections",
            sections,
            |v: &Value, id: String| Some(Section {
                id,
                project_id: s(v, "project_id")?,
                name: s(v, "name")?,
                section_order: i(v, "section_order"),
                is_collapsed: b(v, "is_collapsed"),
            }),
            |v: &Value| b(v, "is_archived")
        );
        merge!(
            "labels",
            labels,
            |v: &Value, id: String| Some(Label {
                id,
                name: s(v, "name")?,
                color: s(v, "color").unwrap_or_default(),
                item_order: i(v, "item_order"),
                is_favorite: b(v, "is_favorite"),
            }),
            |_: &Value| false
        );
        merge!(
            "filters",
            filters,
            |v: &Value, id: String| Some(Filter {
                id,
                name: s(v, "name")?,
                query: s(v, "query")?,
                color: s(v, "color").unwrap_or_default(),
                item_order: i(v, "item_order"),
                is_favorite: b(v, "is_favorite"),
            }),
            |_: &Value| false
        );
        // tasks whose project vanished locally keep showing (in the inbox)

        // 5. user and stats
        if let Some(u) = resp.get("user").filter(|u| u.is_object()) {
            let set = &mut st.settings;
            if let Some(g) = u.get("daily_goal").and_then(Value::as_u64) {
                set.daily_goal = g.min(1000) as u32;
            }
            if let Some(g) = u.get("weekly_goal").and_then(Value::as_u64) {
                set.weekly_goal = g.min(10_000) as u32;
            }
            if let Some(l) = s(u, "lang") {
                set.lang = l;
            }
            if let Some(d) = u.get("start_day").and_then(Value::as_u64) {
                set.start_day = d.clamp(1, 7) as u8;
            }
            if let Some(d) = u.get("next_week").and_then(Value::as_u64) {
                set.next_week = d.clamp(1, 7) as u8;
            }
            if let Some(z) = u.get("tz_info").and_then(|z| s(z, "timezone")) {
                set.timezone = Some(z);
            }
            if let Some(a) = s(u, "email").or_else(|| s(u, "full_name")) {
                st.sync.account = Some(a);
            }
        }
        if let Some(day) = resp
            .get("stats")
            .and_then(|x| x.get("days_items"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        {
            if let (Some(d), Some(n)) = (
                s(day, "date"),
                day.get("total_completed").and_then(Value::as_u64),
            ) {
                st.sync.remote_completed = Some((d, n.min(100_000) as u32));
            }
        }

        // 6. bookkeeping
        if let Some(tok) = s(resp, "sync_token") {
            st.sync.sync_token = Some(tok);
        }
        if needs_full {
            st.sync.sync_token = None;
        }
        st.sync.last_sync_ms = Some(now);
        if rep.full {
            st.sync.last_full_sync_ms = Some(now);
        }
        st.sync.status = SyncStatus::Ok;
        st.sync.last_error = None;
        st.sync.retry_after_ms = None;
        rep.changed = self.commit(st)?;
        Ok(rep)
    }

    /// Merge Todoist's completed tasks (`/api/v1/tasks/completed/…`
    /// `items`) into the history.
    pub fn apply_completed(&mut self, items: &[Value], now: u64) -> Result<bool> {
        let mut st = self.state().clone();
        for v in items {
            let Some(id) = s(v, "id").or_else(|| s(v, "task_id")) else {
                continue;
            };
            let Some(at) = s(v, "completed_at").as_deref().and_then(rfc3339_ms) else {
                continue;
            };
            if let Some(c) = st
                .completed
                .iter_mut()
                .find(|c| c.task_id == id && c.completed_ms.abs_diff(at) < 10 * 60_000)
            {
                c.source = "todoist".into();
                continue;
            }
            st.completed.push(Completed {
                task_id: id,
                content: s(v, "content").unwrap_or_default(),
                project_id: s(v, "project_id").unwrap_or_default(),
                completed_ms: at,
                recurring: v.get("due").is_some_and(|d| b(d, "is_recurring")),
                source: "todoist".into(),
                snapshot: None,
                children: vec![],
                previous_due: None,
            });
        }
        st.completed.sort_by_key(|c| c.completed_ms);
        if st.completed.len() > MAX_COMPLETED {
            let drop = st.completed.len() - MAX_COMPLETED;
            st.completed.drain(..drop);
        }
        let _ = now;
        self.commit(st)
    }

    /// Record a sync failure or state (`error`, `rate_limited`,
    /// `reconnect`, `syncing`).
    pub fn set_sync_status(
        &mut self,
        status: SyncStatus,
        error: Option<String>,
        retry_after_ms: Option<u64>,
    ) -> Result<bool> {
        let mut st = self.state().clone();
        st.sync.status = status;
        st.sync.last_error = error;
        st.sync.retry_after_ms = retry_after_ms;
        self.commit(st)
    }

    /// A Todoist account was linked. Another account than before drops the
    /// old account's data (local-only tasks and their queued adds stay).
    pub fn connect(&mut self, account: Option<String>) -> Result<bool> {
        let mut st = self.state().clone();
        let switched = st.sync.account.is_some() && account.is_some() && st.sync.account != account;
        if switched {
            drop_remote(&mut st);
        }
        st.sync.account = account.or(st.sync.account);
        st.sync.status = SyncStatus::Syncing;
        st.sync.last_error = None;
        st.sync.retry_after_ms = None;
        self.commit(st)
    }

    /// The Todoist link was removed: Todoist's data goes, local-only tasks
    /// stay.
    pub fn disconnect(&mut self) -> Result<bool> {
        let mut st = self.state().clone();
        drop_remote(&mut st);
        st.sync = SyncMeta::default();
        self.commit(st)
    }
}

fn drop_remote(st: &mut State) {
    st.tasks.retain(|_, t| t.is_local());
    for t in st.tasks.values_mut() {
        t.project_id.clear();
        t.section_id = None;
        t.parent_id = t.parent_id.take().filter(|p| p.starts_with(TEMP_PREFIX));
    }
    let keep: HashSet<String> = st.tasks.keys().cloned().collect();
    st.outbox.retain(|o| {
        o.task_id.as_ref().is_some_and(|id| keep.contains(id))
            || o.temp_id.as_ref().is_some_and(|id| keep.contains(id))
    });
    for o in &mut st.outbox {
        if let Some(a) = o.args.as_object_mut() {
            a.remove("project_id");
            a.remove("section_id");
        }
    }
    st.projects.clear();
    st.sections.clear();
    st.labels.clear();
    st.filters.clear();
    st.completed
        .retain(|c| c.source == "local" && c.task_id.starts_with(TEMP_PREFIX));
    st.trash.clear();
    st.conflicts.clear();
    st.sync.sync_token = None;
    st.sync.remote_completed = None;
}
