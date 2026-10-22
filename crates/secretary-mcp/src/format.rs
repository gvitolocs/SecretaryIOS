//! Plain-text rendering of secretary-server answers for the model.
//!
//! Tools return compact text (ids included where a follow-up call needs
//! them) rather than raw JSON: it is what every client shows the model, and
//! it keeps token use low.

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::backend::BResult;
use crate::scope::Caller;
use crate::App;

const MAX_ITEM_TEXT: usize = 600;

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// A string or number as text (ids may be either).
pub fn scalar(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Case- and accent-insensitive key for matching names.
pub fn fold(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' | 'å' | 'ã' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'æ' => 'a',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn clip(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

/// Epoch ms or ISO 8601 → `2026-10-21 18:30` in `tz`.
pub fn local_time(v: Option<&Value>, tz: Tz) -> String {
    let dt: Option<DateTime<Utc>> = match v {
        Some(Value::Number(n)) => n.as_i64().and_then(DateTime::from_timestamp_millis),
        Some(Value::String(s)) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
            .or_else(|| {
                s.parse::<i64>()
                    .ok()
                    .and_then(DateTime::from_timestamp_millis)
            }),
        _ => None,
    };
    match dt {
        Some(d) => tz
            .from_utc_datetime(&d.naive_utc())
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => scalar(v),
    }
}

pub fn now_line(tz: Tz) -> String {
    let now = tz.from_utc_datetime(&Utc::now().naive_utc());
    format!(
        "Now is {} ({}).",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        tz.name()
    )
}

// -- chat --------------------------------------------------------------------------

pub fn chat_reply(r: &Value) -> String {
    let mut out = s(r, "reply")
        .unwrap_or("(Lucina sent an empty reply)")
        .to_owned();
    let action = s(r, "action").unwrap_or("reply");
    if let Some(job) = r.get("job").filter(|j| j.is_object()) {
        out.push_str(&format!(
            "\n\n[Lucina started a background job: {action}, id {}. Its result will appear in lucina_timeline.]",
            scalar(job.get("id"))
        ));
    } else if action != "reply" {
        out.push_str(&format!("\n\n[action: {action}]"));
    }
    if let Some(rem) = r.get("reminder").filter(|j| j.is_object()) {
        out.push_str(&format!(
            "\n[reminder id {}: \"{}\" due {}]",
            scalar(rem.get("id")),
            s(rem, "text").unwrap_or(""),
            s(rem, "dueAt").unwrap_or("?")
        ));
    }
    if r.get("duplicate").and_then(Value::as_bool) == Some(true) {
        out.push_str("\n[duplicate: this is the earlier answer to the same message]");
    }
    out
}

// -- timeline ------------------------------------------------------------------------

pub fn timeline(page: &Value, channel: Option<&str>, limit: usize, tz: Tz) -> String {
    let items = page
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines = Vec::new();
    let mut last_id = None;
    for it in &items {
        let role = s(it, "role").unwrap_or("");
        if role != "user" && role != "lucina" {
            continue;
        }
        let ch = s(it, "channel").unwrap_or("other");
        if channel.is_some_and(|c| c != ch) {
            continue;
        }
        if lines.len() >= limit {
            break;
        }
        last_id = Some(scalar(it.get("id")));
        let who = if role == "user" { "owner" } else { "Lucina" };
        let mut text = clip(s(it, "text").unwrap_or(""), MAX_ITEM_TEXT).replace('\n', " ⏎ ");
        if let Some(att) = it
            .get("attachments")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
        {
            let kinds: Vec<String> = att
                .iter()
                .map(|a| s(a, "kind").unwrap_or("file").to_owned())
                .collect();
            text.push_str(&format!(" [attachments: {}]", kinds.join(", ")));
        }
        if it.pointer("/meta/kind").and_then(Value::as_str) == Some("reminder") {
            text.push_str(" [reminder delivery]");
        }
        lines.push(format!(
            "[{} · {ch}] {who}: {text}",
            local_time(it.get("ts"), tz)
        ));
    }
    if lines.is_empty() {
        return "No messages.".into();
    }
    let mut out = lines.join("\n");
    // With a channel filter the server page was larger than what we show,
    // so the cursor is the last item shown.
    let next = if channel.is_some() || lines.len() >= limit && items.len() > lines.len() {
        last_id
    } else {
        Some(scalar(page.get("nextBefore"))).filter(|n| !n.is_empty())
    };
    if let Some(n) = next {
        out.push_str(&format!("\n\nnext_before: {n}"));
    }
    out
}

// -- shopping ------------------------------------------------------------------------

pub fn list_names(arr: &[Value]) -> String {
    let v: Vec<String> = arr
        .iter()
        .map(|l| {
            format!(
                "{} (id {})",
                s(l, "name").unwrap_or("?"),
                scalar(l.get("id"))
            )
        })
        .collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

fn item_line(i: &Value) -> String {
    let mut line = format!("- {}", s(i, "name").unwrap_or("?"));
    let qty = match i.get("quantity") {
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    if let Some(q) = qty {
        line.push_str(&format!(" ×{q}"));
        if let Some(u) = s(i, "unit") {
            line.push_str(&format!(" {u}"));
        }
    }
    if let Some(spec) = s(i, "spec") {
        line.push_str(&format!(" ({spec})"));
    }
    line.push_str(&format!(" [id {}]", scalar(i.get("id"))));
    line
}

pub fn list_state(state: &Value, others: Option<&Value>) -> String {
    let list = state.get("list").cloned().unwrap_or(Value::Null);
    let items = state
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let needed: Vec<&Value> = items
        .iter()
        .filter(|i| s(i, "status") == Some("needed"))
        .collect();
    let recent: Vec<&Value> = items
        .iter()
        .filter(|i| s(i, "status") == Some("recent"))
        .collect();
    let mut out = format!(
        "Shopping list \"{}\" (id {}): {} needed",
        s(&list, "name").unwrap_or("?"),
        scalar(list.get("id")),
        needed.len()
    );
    out.push_str(if needed.is_empty() { ".\n" } else { ":\n" });
    for i in &needed {
        out.push_str(&item_line(i));
        out.push('\n');
    }
    if !recent.is_empty() {
        let names: Vec<&str> = recent
            .iter()
            .take(20)
            .filter_map(|i| s(i, "name"))
            .collect();
        out.push_str(&format!("Recently bought: {}\n", names.join(", ")));
    }
    if let Some(arr) = others.and_then(Value::as_array).filter(|a| a.len() > 1) {
        out.push_str(&format!("Other lists: {}\n", list_names(arr)));
    }
    out.trim_end().to_owned()
}

pub fn legacy_name(i: &Value) -> String {
    let raw = s(i, "rawName");
    let product = s(i, "product");
    match (raw, product) {
        (Some(r), Some(p)) if fold(r) != fold(p) => format!("{r} ({p})"),
        (Some(r), _) => r.to_owned(),
        (None, Some(p)) => p.to_owned(),
        _ => "?".into(),
    }
}

pub fn legacy_list(items: &Value) -> String {
    let arr = items.as_array().cloned().unwrap_or_default();
    let needed: Vec<&Value> = arr
        .iter()
        .filter(|i| s(i, "status") == Some("needed"))
        .collect();
    let bought = arr
        .iter()
        .filter(|i| s(i, "status") == Some("purchased"))
        .count();
    let mut out = format!("Shopping list: {} needed", needed.len());
    out.push_str(if needed.is_empty() { ".\n" } else { ":\n" });
    for i in needed {
        out.push_str(&format!(
            "- {} [id {}]\n",
            legacy_name(i),
            scalar(i.get("id"))
        ));
    }
    if bought > 0 {
        out.push_str(&format!("{bought} item(s) already bought.\n"));
    }
    out.trim_end().to_owned()
}

pub fn ops_result(verb: &str, names: &[String], list_id: &str, r: &Value) -> String {
    let dup = r
        .get("duplicates")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let needed = r.get("items").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter(|i| s(i, "status") == Some("needed"))
            .count()
    });
    let mut out = format!("{verb} on list {list_id}: {}.", names.join(", "));
    if dup > 0 {
        out.push_str(&format!(" ({dup} already applied before.)"));
    }
    if let Some(n) = needed {
        out.push_str(&format!(" The list now has {n} item(s) to buy."));
    }
    out
}

/// Find one item by id, exact (folded) name, or a unique partial name.
pub fn pick_item(
    items: &[Value],
    want: &str,
    pred: impl Fn(&Value) -> bool,
) -> Result<Value, String> {
    let cands: Vec<&Value> = items.iter().filter(|i| pred(i)).collect();
    if let Some(i) = cands.iter().find(|i| scalar(i.get("id")) == want) {
        return Ok((*i).clone());
    }
    let w = fold(want);
    let exact: Vec<&&Value> = cands
        .iter()
        .filter(|i| {
            s(i, "name").is_some_and(|n| fold(n) == w)
                || s(i, "catalogId").is_some_and(|c| fold(c) == w)
        })
        .collect();
    if exact.len() == 1 {
        return Ok((*exact[0]).clone());
    }
    let partial: Vec<&&Value> = if exact.is_empty() {
        cands
            .iter()
            .filter(|i| s(i, "name").is_some_and(|n| fold(n).contains(&w)))
            .collect()
    } else {
        exact
    };
    match partial.len() {
        1 => Ok((*partial[0]).clone()),
        0 => Err(format!(
            "No item matching {want:?} on the list; call shopping_list to see the ids."
        )),
        _ => Err(format!(
            "{want:?} matches several items: {}. Use the id.",
            partial
                .iter()
                .map(|i| item_line(i).trim_start_matches("- ").to_owned())
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

pub fn pick_legacy(arr: &[Value], want: &str) -> Result<Value, String> {
    let needed: Vec<Value> = arr
        .iter()
        .filter(|i| s(i, "status") == Some("needed"))
        .map(|i| {
            let mut v = i.clone();
            if v.get("name").is_none() {
                v["name"] =
                    Value::String(s(i, "rawName").or(s(i, "product")).unwrap_or("").to_owned());
            }
            v
        })
        .collect();
    pick_item(&needed, want, |_| true).or_else(|e| {
        // also match the canonical product name
        let w = fold(want);
        needed
            .iter()
            .find(|i| s(i, "product").is_some_and(|p| fold(p) == w))
            .cloned()
            .ok_or(e)
    })
}

// -- reminders -----------------------------------------------------------------------

fn reminder_line(r: &Value, tz: Tz) -> String {
    format!(
        "- {} — {} [id {}] ({})",
        local_time(r.get("dueAt"), tz),
        s(r, "text").unwrap_or("?"),
        scalar(r.get("id")),
        s(r, "status").unwrap_or("?")
    )
}

pub fn reminders(r: &Value, tz: Tz) -> String {
    let list = r
        .get("reminders")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let zone = s(r, "timeZone")
        .and_then(|z| z.parse::<Tz>().ok())
        .unwrap_or(tz);
    if list.is_empty() {
        return format!("No reminders ({}).", s(r, "scope").unwrap_or("upcoming"));
    }
    let mut out = format!("{} reminder(s), times in {}:\n", list.len(), zone.name());
    for x in &list {
        out.push_str(&reminder_line(x, zone));
        out.push('\n');
    }
    out.trim_end().to_owned()
}

pub fn created_reminder(r: &Value, tz: Tz) -> String {
    let rem = r.get("reminder").cloned().unwrap_or(Value::Null);
    let zone = s(&rem, "timeZone")
        .and_then(|z| z.parse::<Tz>().ok())
        .unwrap_or(tz);
    let created = r.get("created").and_then(Value::as_bool).unwrap_or(true);
    format!(
        "{} reminder \"{}\" for {} ({}), id {}. Lucina will deliver it on Telegram.",
        if created {
            "Created"
        } else {
            "Already had the"
        },
        s(&rem, "text").unwrap_or("?"),
        local_time(rem.get("dueAt"), zone),
        zone.name(),
        scalar(rem.get("id"))
    )
}

// -- gym / body -------------------------------------------------------------------------

pub fn gym_insights(v: &Value) -> String {
    let g = v.get("gym").cloned().unwrap_or(Value::Null);
    let mut out = format!(
        "Workouts this week: {} of {} (goal).",
        scalar(g.get("workouts_this_week")),
        scalar(g.get("weekly_goal"))
    );
    if let Some(ws) = g.get("recent_workouts").and_then(Value::as_array) {
        out.push_str("\nRecent workouts:");
        for w in ws {
            out.push_str(&format!(
                "\n- {} {}: {} sets, {} volume, {} min; {}",
                s(w, "date").unwrap_or("?"),
                s(w, "title").unwrap_or("Workout"),
                scalar(w.get("completed_sets")),
                s(w, "volume_label").unwrap_or("?"),
                scalar(w.get("duration_min")),
                w.get("exercises")
                    .and_then(Value::as_array)
                    .map(|a| a
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default()
            ));
            if let Some(prs) = w
                .get("prs")
                .and_then(Value::as_array)
                .filter(|p| !p.is_empty())
            {
                let p: Vec<String> = prs
                    .iter()
                    .map(|p| {
                        format!(
                            "{} {}",
                            s(p, "name").unwrap_or("?"),
                            s(p, "label").unwrap_or("")
                        )
                    })
                    .collect();
                out.push_str(&format!(" — PR: {}", p.join(", ")));
            }
        }
    }
    if let Some(wv) = g.get("weekly_volume").and_then(Value::as_array) {
        let weeks: Vec<String> = wv
            .iter()
            .rev()
            .take(4)
            .map(|w| {
                format!(
                    "week of {}: {} workouts, {}",
                    s(w, "week_start").unwrap_or("?"),
                    scalar(w.get("workouts")),
                    s(w, "volume_label").unwrap_or("?")
                )
            })
            .collect();
        if !weeks.is_empty() {
            out.push_str(&format!(
                "\nWeekly volume (latest first): {}",
                weeks.join("; ")
            ));
        }
    }
    out
}

pub fn gym_state(v: &Value) -> String {
    let view = v.get("view").cloned().unwrap_or(Value::Null);
    let sum = view.get("summary").cloned().unwrap_or(Value::Null);
    let mut out = format!(
        "Workouts this week: {} of {} (goal). Total: {} workouts, {} completed sets, {} volume.",
        scalar(sum.get("weekly_workouts")),
        scalar(sum.get("weekly_goal")),
        scalar(sum.get("workouts")),
        scalar(sum.get("completed_sets")),
        s(&sum, "volume").unwrap_or("?")
    );
    if view.pointer("/state/active").is_some_and(|a| a.is_object()) {
        out.push_str(&format!(
            "\nA workout is in progress: {} sets, {} volume, {}.",
            scalar(sum.get("active_completed_sets")),
            s(&sum, "active_volume").unwrap_or("?"),
            s(&sum, "active_duration").unwrap_or("?")
        ));
    }
    if let Some(recs) = view
        .get("records")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
    {
        out.push_str("\nBest sets:");
        for r in recs.iter().take(12) {
            out.push_str(&format!(
                "\n- {}: {} × {}{}",
                s(r, "name").unwrap_or("?"),
                s(r, "weight").unwrap_or("?"),
                scalar(r.get("reps")),
                s(r, "estimated_1rm")
                    .map(|e| format!(" (e1RM {e})"))
                    .unwrap_or_default()
            ));
        }
    }
    out
}

pub fn body_state(v: &Value, tz: Tz) -> String {
    let view = v.get("view").cloned().unwrap_or(Value::Null);
    let labels = view.get("stat_labels").cloned().unwrap_or(Value::Null);
    let count = view
        .pointer("/stats/count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if count == 0 {
        return "No body weight entries yet.".into();
    }
    let l = |k: &str| s(&labels, k).unwrap_or("n/a").to_owned();
    let mut out = format!(
        "Body weight: current {}, 7-day average {}, weekly change {}, 30-day trend {}, 90-day trend {}, min {}, max {} ({count} entries).",
        l("current"),
        l("avg7"),
        l("weekly_change"),
        l("trend30"),
        l("trend90"),
        l("min"),
        l("max")
    );
    if labels.get("goal").is_some() {
        let reached = view.pointer("/stats/goal_reached").and_then(Value::as_bool) == Some(true);
        out.push_str(&format!(
            "\nGoal {}: {} to go{}.",
            l("goal"),
            l("goal_distance"),
            if reached { " (reached)" } else { "" }
        ));
    }
    if let Some(entries) = view.get("entries").and_then(Value::as_array) {
        let ls = view.get("labels").cloned().unwrap_or(Value::Null);
        out.push_str("\nLatest entries:");
        for e in entries.iter().take(7) {
            let id = scalar(e.get("id"));
            let w = ls
                .get(&id)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    e.get("weight_grams")
                        .and_then(Value::as_i64)
                        .map(|g| format!("{:.1} kg", g as f64 / 1000.0))
                        .unwrap_or_default()
                });
            let note = s(e, "note")
                .map(|n| format!(" — {}", clip(n, 80)))
                .unwrap_or_default();
            out.push_str(&format!(
                "\n- {} {w}{note}",
                local_time(e.get("measured_ms"), tz)
            ));
        }
    }
    out
}

// -- status ------------------------------------------------------------------------------

pub fn status(
    health: &BResult<Value>,
    token: Option<BResult<bool>>,
    caller: &Caller,
    app: &App,
) -> String {
    let mut out = format!(
        "Lucina MCP {} — connected as {} (scopes: {}).",
        crate::VERSION,
        caller.name,
        caller.scopes
    );
    match health {
        Ok(h) => {
            out.push_str(&format!(
                "\nSecretary server {}: up.",
                s(h, "version").unwrap_or("?")
            ));
            let l = h.get("lucina").cloned().unwrap_or(Value::Null);
            if l.get("reachable").and_then(Value::as_bool) == Some(true) {
                out.push_str(&format!(
                    " Lucina: reachable ({} ms).",
                    scalar(l.get("latencyMs"))
                ));
            } else {
                out.push_str(&format!(
                    " Lucina: NOT reachable ({}).",
                    s(&l, "error").unwrap_or("unknown")
                ));
            }
        }
        Err(e) => out.push_str(&format!("\nSecretary server: unreachable ({e}).")),
    }
    match token {
        None => out.push_str("\nBackend device token: not configured."),
        Some(Ok(true)) => out.push_str("\nBackend device token: accepted."),
        Some(Ok(false)) => out.push_str("\nBackend device token: REJECTED (revoked?)."),
        Some(Err(e)) => out.push_str(&format!("\nBackend device token: could not check ({e}).")),
    }
    out.push_str(&format!(
        "\nOwner time zone: {}. {}",
        app.cfg.timezone.name(),
        now_line(app.cfg.timezone)
    ));
    out
}

// -- calendar ----------------------------------------------------------------------

/// `GET /v1/calendar/events` → one line per event, in `tz`.
pub fn calendar(r: &Value, tz: Tz) -> String {
    let events = r
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let day = |v: Option<&Value>| -> String {
        let t = v
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        t.map(|t| t.with_timezone(&tz).format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    };
    let mut out = format!(
        "Calendar {} to {} ({}):",
        day(r.get("from")),
        r.get("to")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| (t.with_timezone(&tz) - chrono::Duration::seconds(1))
                .format("%Y-%m-%d")
                .to_string())
            .unwrap_or_default(),
        tz.name()
    );
    if events.is_empty() {
        out.push_str("\nNo events.");
    }
    for e in events.iter().take(200) {
        let title = s(e, "title").unwrap_or("(no title)");
        let when = if e.get("allDay").and_then(Value::as_bool) == Some(true) {
            format!("{} (all day)", s(e, "start").unwrap_or(""))
        } else {
            let start = local_time(e.get("start"), tz);
            let end = local_time(e.get("end"), tz);
            let end_short = if end.get(..10) == start.get(..10) {
                end.get(11..).unwrap_or(&end).to_owned()
            } else {
                end
            };
            format!("{start}–{end_short}")
        };
        let mut line = format!("\n- {when} {title}");
        if let Some(c) = s(e, "calendar") {
            line.push_str(&format!(" ({c})"));
        }
        if let Some(l) = s(e, "location") {
            line.push_str(&format!(" @ {l}"));
        }
        line.push_str(&format!(" [id {}]", s(e, "id").unwrap_or("?")));
        out.push_str(&line);
    }
    if events.len() > 200 {
        out.push_str(&format!("\n… and {} more.", events.len() - 200));
    }
    if let Some(g) = r.pointer("/sources/google").filter(|g| g.is_object()) {
        if s(g, "status") == Some("reconnect") {
            out.push_str("\nNote: Google Calendar needs reconnecting in the Secretary app (Links); Google events may be out of date.");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folding_and_times() {
        assert_eq!(fold("  Caffè   Lungo "), "caffe lungo");
        let tz: Tz = "Europe/Rome".parse().unwrap();
        assert_eq!(
            local_time(Some(&json!(1_761_064_200_000_i64)), tz),
            "2025-10-21 18:30"
        );
        assert_eq!(
            local_time(Some(&json!("2026-10-22T07:30:00.000Z")), tz),
            "2026-10-22 09:30"
        );
        assert_eq!(local_time(Some(&json!("garbage")), tz), "garbage");
    }

    #[test]
    fn picking() {
        let items = vec![
            json!({"id": "a", "name": "Latte", "status": "needed"}),
            json!({"id": "b", "name": "Latte di soia", "status": "needed"}),
            json!({"id": "c", "name": "Pane", "status": "recent"}),
        ];
        let needed = |i: &Value| i["status"] == "needed";
        assert_eq!(pick_item(&items, "latte", needed).unwrap()["id"], "a");
        assert_eq!(pick_item(&items, "b", needed).unwrap()["id"], "b");
        assert_eq!(pick_item(&items, "soia", needed).unwrap()["id"], "b");
        assert!(pick_item(&items, "pane", needed).is_err());
        let both = vec![
            json!({"id": "a", "name": "Mele rosse", "status": "needed"}),
            json!({"id": "b", "name": "Mele verdi", "status": "needed"}),
        ];
        assert!(pick_item(&both, "mele", needed)
            .unwrap_err()
            .contains("several"));
    }

    #[test]
    fn timeline_render() {
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let page = json!({"items": [
            {"id": "tl_2", "ts": 1_761_064_200_000_i64, "channel": "telegram", "role": "lucina", "text": "Fatto."},
            {"id": "tl_1", "ts": 1_761_064_100_000_i64, "channel": "web", "role": "user", "text": "Ciao\nLucina"},
            {"id": "tl_0", "ts": 1, "channel": "web", "role": "system", "text": "x"}
        ], "nextBefore": "tl_0"});
        let t = timeline(&page, None, 20, tz);
        assert!(
            t.contains("[2025-10-21 18:30 · telegram] Lucina: Fatto."),
            "{t}"
        );
        assert!(t.contains("owner: Ciao ⏎ Lucina"));
        assert!(!t.contains("system"));
        assert!(t.ends_with("next_before: tl_0"));
        let t = timeline(&page, Some("web"), 20, tz);
        assert!(
            !t.contains("telegram") && t.ends_with("next_before: tl_1"),
            "{t}"
        );
    }
}
