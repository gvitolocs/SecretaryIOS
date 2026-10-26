//! Views: the sidebar (counts, projects, labels, filters, goal, sync) and
//! one list (inbox, today, upcoming, project, label, filter, search,
//! completed), laid out like Todoist's.

use std::collections::{HashMap, HashSet};

use chrono::{Duration, NaiveDate};
use chrono_tz::Tz;

use crate::filter;
use crate::models::*;
use crate::{due_local, ms_to_utc, today_in, UNDO_MS};

pub const DEFAULT_UPCOMING_DAYS: u32 = 7;
pub const MAX_DAYS: u32 = 60;
pub const DEFAULT_COMPLETED_DAYS: u32 = 7;

/// Ids hidden by a soft delete (the task or an ancestor) or completion.
fn hidden(s: &State) -> HashSet<String> {
    let mut out: HashSet<String> = s
        .tasks
        .values()
        .filter(|t| t.deleted_ms.is_some() || t.checked)
        .map(|t| t.id.clone())
        .collect();
    loop {
        let more: Vec<String> = s
            .tasks
            .values()
            .filter(|t| !out.contains(&t.id))
            .filter(|t| t.parent_id.as_ref().is_some_and(|p| out.contains(p)))
            .map(|t| t.id.clone())
            .collect();
        if more.is_empty() {
            return out;
        }
        out.extend(more);
    }
}

/// Tasks that are neither completed nor (soft-)deleted.
pub fn open_tasks(s: &State) -> impl Iterator<Item = &Task> + '_ {
    let hide = hidden(s);
    s.tasks.values().filter(move |t| !hide.contains(&t.id))
}

struct Ctx<'a> {
    s: &'a State,
    tz: Tz,
    today: NaiveDate,
    pending: HashSet<String>,
    open: Vec<&'a Task>,
    children: HashMap<String, Vec<&'a Task>>,
}

impl<'a> Ctx<'a> {
    fn new(s: &'a State, tz: Tz, now: u64) -> Self {
        let open: Vec<&Task> = open_tasks(s).collect();
        let mut children: HashMap<String, Vec<&Task>> = HashMap::new();
        for t in &open {
            if let Some(p) = &t.parent_id {
                children.entry(p.clone()).or_default().push(t);
            }
        }
        for v in children.values_mut() {
            v.sort_by(|a, b| (a.child_order, &a.id).cmp(&(b.child_order, &b.id)));
        }
        Self {
            s,
            tz,
            today: today_in(tz, now),
            pending: s.outbox.iter().filter_map(|o| o.task_id.clone()).collect(),
            open,
            children,
        }
    }

    fn date_of(&self, t: &Task) -> Option<NaiveDate> {
        t.due
            .as_ref()
            .and_then(|d| due_local(d, self.tz))
            .map(|(d, _)| d)
    }

    fn task(&self, t: &Task, nest: bool) -> TaskView {
        let s = self.s;
        let project = s.projects.get(&t.project_id);
        let due = t.due.as_ref().and_then(|d| {
            let (date, time) = due_local(d, self.tz)?;
            let rel = (date - self.today).num_days();
            Some(DueView {
                date: date.format("%Y-%m-%d").to_string(),
                time: time.map(|x| x.format("%H:%M").to_string()),
                string: d.string.clone(),
                is_recurring: d.is_recurring,
                rel_days: rel,
                overdue: rel < 0,
                provisional: t.provisional,
            })
        });
        let kids = self.children.get(&t.id).map(Vec::as_slice).unwrap_or(&[]);
        let all_kids = s
            .tasks
            .values()
            .filter(|x| x.parent_id.as_deref() == Some(t.id.as_str()) && x.deleted_ms.is_none())
            .count() as u32;
        let done_kids = s
            .completed
            .iter()
            .filter(|c| {
                c.snapshot
                    .as_ref()
                    .is_some_and(|x| x.parent_id.as_deref() == Some(t.id.as_str()))
            })
            .count() as u32;
        TaskView {
            id: t.id.clone(),
            content: t.content.clone(),
            description: t.description.clone(),
            project_id: project.map_or_else(
                || s.inbox_id().unwrap_or(&t.project_id).to_owned(),
                |p| p.id.clone(),
            ),
            project_name: project.map_or_else(|| "Inbox".to_owned(), |p| p.name.clone()),
            project_color: project.map(|p| p.color.clone()).unwrap_or_default(),
            section_id: t.section_id.clone(),
            section_name: t
                .section_id
                .as_ref()
                .and_then(|x| s.sections.get(x))
                .map(|x| x.name.clone()),
            parent_id: t.parent_id.clone(),
            parent_content: t
                .parent_id
                .as_ref()
                .and_then(|p| s.tasks.get(p))
                .map(|p| p.content.clone()),
            priority: t.p(),
            due,
            deadline: t.deadline.clone(),
            labels: t.labels.clone(),
            checked: t.checked,
            subtasks: all_kids + done_kids,
            subtasks_done: done_kids,
            pending: self.pending.contains(&t.id),
            duration_minutes: t.duration.as_ref().map(|d| {
                if d.unit == "day" {
                    d.amount.saturating_mul(1440)
                } else {
                    d.amount
                }
            }),
            children: if nest {
                kids.iter().map(|k| self.task(k, true)).collect()
            } else {
                vec![]
            },
            completed_ms: None,
        }
    }

    /// Today/upcoming order: timed first by time, then priority, then
    /// Todoist's day order.
    fn day_sort(&self, v: &mut [&Task]) {
        v.sort_by_key(|t| {
            let time = t
                .due
                .as_ref()
                .and_then(|d| due_local(d, self.tz))
                .and_then(|(_, t)| t);
            (
                time.is_none(),
                time,
                std::cmp::Reverse(t.priority),
                t.day_order,
                t.child_order,
                t.id.clone(),
            )
        });
    }

    fn roots_in<'b>(&self, it: impl Iterator<Item = &'b &'a Task>) -> Vec<&'a Task>
    where
        'a: 'b,
    {
        let open_ids: HashSet<&str> = self.open.iter().map(|t| t.id.as_str()).collect();
        let mut v: Vec<&Task> = it
            .filter(|t| t.parent_id.as_deref().is_none_or(|p| !open_ids.contains(p)))
            .copied()
            .collect();
        v.sort_by(|a, b| (a.child_order, &a.id).cmp(&(b.child_order, &b.id)));
        v
    }
}

fn group(
    key: &str,
    kind: &str,
    title: &str,
    date: Option<NaiveDate>,
    today: NaiveDate,
    tasks: Vec<TaskView>,
) -> Group {
    Group {
        key: key.into(),
        kind: kind.into(),
        title: title.into(),
        date: date.map(|d| d.format("%Y-%m-%d").to_string()),
        rel_days: date.map(|d| (d - today).num_days()),
        tasks,
    }
}

fn count(groups: &[Group]) -> u32 {
    fn n(t: &TaskView) -> u32 {
        1 + t.children.iter().map(n).sum::<u32>()
    }
    groups.iter().flat_map(|g| g.tasks.iter()).map(n).sum()
}

fn dated_groups(c: &Ctx, days: u32, overdue: bool) -> Vec<Group> {
    let mut groups = Vec::new();
    if overdue {
        let mut od: Vec<&Task> = c
            .open
            .iter()
            .filter(|t| c.date_of(t).is_some_and(|d| d < c.today))
            .copied()
            .collect();
        od.sort_by_key(|t| {
            (
                c.date_of(t),
                std::cmp::Reverse(t.priority),
                t.day_order,
                t.id.clone(),
            )
        });
        if !od.is_empty() {
            groups.push(group(
                "overdue",
                "overdue",
                "Overdue",
                None,
                c.today,
                od.iter().map(|t| c.task(t, false)).collect(),
            ));
        }
    }
    for k in 0..days.max(1) {
        let date = c.today + Duration::days(i64::from(k));
        let mut v: Vec<&Task> = c
            .open
            .iter()
            .filter(|t| c.date_of(t) == Some(date))
            .copied()
            .collect();
        c.day_sort(&mut v);
        groups.push(group(
            &date.format("%Y-%m-%d").to_string(),
            "date",
            "",
            Some(date),
            c.today,
            v.iter().map(|t| c.task(t, false)).collect(),
        ));
    }
    groups
}

fn list(c: &Ctx, q: &ViewQuery, now: u64) -> ListView {
    let s = c.s;
    let kind = q.kind.as_deref().unwrap_or("today");
    let mut out = ListView {
        kind: kind.into(),
        id: q.id.clone(),
        title: String::new(),
        query: q.query.clone(),
        groups: vec![],
        count: 0,
        warning: None,
    };
    let flat = |c: &Ctx, mut v: Vec<&Task>| -> Vec<Group> {
        v.sort_by_key(|t| {
            (
                c.date_of(t).is_none(),
                c.date_of(t),
                std::cmp::Reverse(t.priority),
                t.child_order,
                t.id.clone(),
            )
        });
        vec![group(
            "none",
            "none",
            "",
            None,
            c.today,
            v.iter().map(|t| c.task(t, false)).collect(),
        )]
    };
    match kind {
        "today" => {
            out.title = "Today".into();
            out.groups = dated_groups(c, 1, true);
        }
        "upcoming" => {
            out.title = "Upcoming".into();
            let days = q.days.unwrap_or(DEFAULT_UPCOMING_DAYS).clamp(1, MAX_DAYS);
            out.groups = dated_groups(c, days, true);
        }
        "inbox" | "project" => {
            let pid = if kind == "inbox" {
                s.inbox_id().map(str::to_owned).unwrap_or_default()
            } else {
                q.id.clone().unwrap_or_default()
            };
            let project = s.projects.get(&pid);
            out.id = Some(pid.clone());
            out.title = project.map_or_else(|| "Inbox".into(), |p| p.name.clone());
            if kind == "project" && project.is_none() {
                out.warning = Some("Project not found".into());
            }
            let in_project: Vec<&Task> = c
                .open
                .iter()
                .filter(|t| t.project_id == pid || (kind == "inbox" && t.project_id.is_empty()))
                .copied()
                .collect();
            let roots = c.roots_in(in_project.iter());
            let mut sections: Vec<&Section> = s
                .sections
                .values()
                .filter(|x| x.project_id == pid)
                .collect();
            sections.sort_by_key(|x| (x.section_order, x.id.clone()));
            let none: Vec<TaskView> = roots
                .iter()
                .filter(|t| {
                    t.section_id
                        .as_ref()
                        .is_none_or(|x| !s.sections.contains_key(x))
                })
                .map(|t| c.task(t, true))
                .collect();
            out.groups
                .push(group("none", "none", "", None, c.today, none));
            for sec in sections {
                let v = roots
                    .iter()
                    .filter(|t| t.section_id.as_deref() == Some(sec.id.as_str()))
                    .map(|t| c.task(t, true))
                    .collect();
                out.groups
                    .push(group(&sec.id, "section", &sec.name, None, c.today, v));
            }
        }
        "label" => {
            let name = q.id.clone().unwrap_or_default();
            out.title = name.clone();
            let v = c
                .open
                .iter()
                .filter(|t| t.labels.iter().any(|l| l.eq_ignore_ascii_case(&name)))
                .copied()
                .collect();
            out.groups = flat(c, v);
        }
        "filter" | "search" => {
            let (title, query) = if kind == "search" {
                let text = q.query.clone().unwrap_or_default();
                (
                    text.clone(),
                    format!(
                        "search: {}",
                        text.replace(['|', '&', '!', '(', ')', ','], " ")
                    ),
                )
            } else if let Some(f) = q.id.as_ref().and_then(|id| s.filters.get(id)) {
                (f.name.clone(), f.query.clone())
            } else {
                let qq = q.query.clone().unwrap_or_default();
                (qq.clone(), qq)
            };
            out.title = title;
            out.query = Some(query.clone());
            let e = filter::parse(&query);
            let ev = filter::Eval {
                state: s,
                tz: c.tz,
                today: c.today,
                unsupported: Default::default(),
            };
            let v: Vec<&Task> = if query.trim().is_empty() || query.trim() == "search:" {
                vec![]
            } else {
                c.open
                    .iter()
                    .filter(|t| ev.matches(&e, t))
                    .copied()
                    .collect()
            };
            let unsupported = ev.unsupported.into_inner();
            if !unsupported.is_empty() {
                out.warning = Some(format!("Not understood here: {}", unsupported.join(", ")));
            }
            out.groups = flat(c, v);
        }
        "completed" => {
            out.title = "Completed".into();
            let days = q.days.unwrap_or(DEFAULT_COMPLETED_DAYS).clamp(1, MAX_DAYS);
            let since = c.today - Duration::days(i64::from(days) - 1);
            let mut by_day: Vec<(NaiveDate, Vec<TaskView>)> = Vec::new();
            for e in s.completed.iter().rev() {
                let d = ms_to_utc(e.completed_ms).with_timezone(&c.tz).date_naive();
                if d < since || e.completed_ms > now + crate::DAY_MS {
                    continue;
                }
                let base = e.snapshot.clone().unwrap_or_else(|| Task {
                    id: e.task_id.clone(),
                    project_id: e.project_id.clone(),
                    content: e.content.clone(),
                    priority: 1,
                    checked: true,
                    ..Task::default()
                });
                let mut tv = c.task(&base, false);
                tv.checked = true;
                tv.completed_ms = Some(e.completed_ms);
                if e.recurring {
                    tv.due = None;
                }
                match by_day.iter_mut().find(|(x, _)| *x == d) {
                    Some((_, v)) => v.push(tv),
                    None => by_day.push((d, vec![tv])),
                }
            }
            by_day.sort_by_key(|x| std::cmp::Reverse(x.0));
            out.groups = by_day
                .into_iter()
                .map(|(d, v)| {
                    group(
                        &d.format("%Y-%m-%d").to_string(),
                        "completed",
                        "",
                        Some(d),
                        c.today,
                        v,
                    )
                })
                .collect();
        }
        _ => {
            out.warning = Some(format!("Unknown view {kind}"));
        }
    }
    out.count = count(&out.groups);
    out
}

pub fn completed_today(s: &State, tz: Tz, now: u64) -> u32 {
    let today = today_in(tz, now);
    let local = s
        .completed
        .iter()
        .filter(|c| ms_to_utc(c.completed_ms).with_timezone(&tz).date_naive() == today)
        .count() as u32;
    let remote = match &s.sync.remote_completed {
        Some((d, n)) if *d == today.format("%Y-%m-%d").to_string() => *n,
        _ => 0,
    };
    local.max(remote)
}

pub fn view(s: &State, q: &ViewQuery, tz: Tz, now: u64) -> View {
    let c = Ctx::new(s, tz, now);
    let inbox = s.inbox_id().map(str::to_owned);
    let mut per_project: HashMap<&str, u32> = HashMap::new();
    let mut per_label: HashMap<String, u32> = HashMap::new();
    let (mut today_n, mut overdue_n, mut upcoming_n, mut inbox_n) = (0, 0, 0, 0);
    for t in &c.open {
        *per_project.entry(t.project_id.as_str()).or_default() += 1;
        for l in &t.labels {
            *per_label.entry(l.to_lowercase()).or_default() += 1;
        }
        if t.project_id.is_empty() || Some(&t.project_id) == inbox.as_ref() {
            inbox_n += 1;
        }
        if let Some(d) = c.date_of(t) {
            if d < c.today {
                overdue_n += 1;
            } else if d == c.today {
                today_n += 1;
            } else if d < c.today + Duration::days(7) {
                upcoming_n += 1;
            }
        }
    }
    let mut projects: Vec<&Project> = s.projects.values().collect();
    projects.sort_by_key(|p| (!p.inbox_project, p.child_order, p.id.clone()));
    let projects = projects
        .into_iter()
        .map(|p| {
            let mut secs: Vec<&Section> = s
                .sections
                .values()
                .filter(|x| x.project_id == p.id)
                .collect();
            secs.sort_by_key(|x| (x.section_order, x.id.clone()));
            ProjectView {
                id: p.id.clone(),
                name: p.name.clone(),
                color: p.color.clone(),
                parent_id: p.parent_id.clone(),
                inbox: p.inbox_project,
                favorite: p.is_favorite,
                count: per_project.get(p.id.as_str()).copied().unwrap_or(0),
                sections: secs
                    .into_iter()
                    .map(|x| SectionView {
                        id: x.id.clone(),
                        name: x.name.clone(),
                    })
                    .collect(),
            }
        })
        .collect();
    let mut labels: Vec<NamedCount> = {
        let mut v: Vec<&Label> = s.labels.values().collect();
        v.sort_by_key(|l| (l.item_order, l.id.clone()));
        v.into_iter()
            .map(|l| NamedCount {
                id: l.id.clone(),
                name: l.name.clone(),
                color: l.color.clone(),
                favorite: l.is_favorite,
                count: per_label.get(&l.name.to_lowercase()).copied().unwrap_or(0),
                query: None,
            })
            .collect()
    };
    // labels used on tasks but not in the personal list (shared labels)
    let mut extra: Vec<String> = c
        .open
        .iter()
        .flat_map(|t| t.labels.iter())
        .filter(|l| !s.labels.values().any(|x| x.name.eq_ignore_ascii_case(l)))
        .cloned()
        .collect();
    extra.sort_by_key(|l| l.to_lowercase());
    extra.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    for l in extra {
        labels.push(NamedCount {
            id: format!("name:{l}"),
            count: per_label.get(&l.to_lowercase()).copied().unwrap_or(0),
            name: l,
            color: String::new(),
            favorite: false,
            query: None,
        });
    }
    let filters = {
        let mut v: Vec<&Filter> = s.filters.values().collect();
        v.sort_by_key(|f| (f.item_order, f.id.clone()));
        v.into_iter()
            .map(|f| {
                let e = filter::parse(&f.query);
                let ev = filter::Eval {
                    state: s,
                    tz,
                    today: c.today,
                    unsupported: Default::default(),
                };
                NamedCount {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    color: f.color.clone(),
                    favorite: f.is_favorite,
                    count: c.open.iter().filter(|t| ev.matches(&e, t)).count() as u32,
                    query: Some(f.query.clone()),
                }
            })
            .collect()
    };
    let done = completed_today(s, tz, now);
    let goal = s.settings.daily_goal;
    View {
        revision: s.revision,
        today: c.today.format("%Y-%m-%d").to_string(),
        timezone: tz.name().to_owned(),
        counts: Counts {
            inbox: inbox_n,
            today: today_n + overdue_n,
            overdue: overdue_n,
            upcoming: upcoming_n,
        },
        goal: Goal {
            daily_goal: goal,
            completed_today: done,
            reached: goal > 0 && done >= goal,
        },
        inbox_id: inbox,
        projects,
        labels,
        filters,
        sync: SyncView {
            status: s.sync.status.clone(),
            connected: s.sync.status != SyncStatus::Disconnected,
            account: s.sync.account.clone(),
            last_sync_ms: s.sync.last_sync_ms,
            pending: s.outbox.len() as u32,
            error: s.sync.last_error.clone(),
            retry_after_ms: s.sync.retry_after_ms,
        },
        conflicts: s.conflicts.iter().rev().take(20).cloned().collect(),
        undo: s
            .tasks
            .values()
            .filter(|t| t.deleted_ms.is_some_and(|d| d + UNDO_MS * 3 > now))
            .map(|t| t.id.clone())
            .collect(),
        list: list(&c, q, now),
    }
}
