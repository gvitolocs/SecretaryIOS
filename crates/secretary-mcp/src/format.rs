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
            "\n\n[Lucina started a background job: {action}, id {}. Follow it with lucina_job_status {{\"job_id\": \"{}\"}}; the result also appears in lucina_timeline.]",
            scalar(job.get("id")), scalar(job.get("id"))
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
    if r.get("consult").and_then(Value::as_bool) == Some(true) {
        if let Some(not_run) = s(r, "notRun") {
            let hint = s(r, "hint").map(|h| format!(" {h}")).unwrap_or_default();
            out.push_str(&format!("\n\n[Lucina only answered: this is a consult turn, so she did not run {not_run}.{hint}]"));
        }
    }
    if r.get("duplicate").and_then(Value::as_bool) == Some(true) {
        out.push_str("\n[duplicate: this is the earlier answer to the same message]");
    }
    out
}

/// The answer to `secret_request`: names only (Lucina's answer never holds a value).
pub fn secret_request(r: &Value) -> String {
    let name = s(r, "name").unwrap_or("?");
    let place = format!(
        "{} / {} / {}",
        s(r, "project").unwrap_or("?"),
        s(r, "environment").unwrap_or("?"),
        s(r, "path").unwrap_or("/")
    );
    let head = if s(r, "status") == Some("already_asked") {
        format!("Already asked: the owner has (or just answered) a card for `{name}` in {place}.")
    } else if s(r, "status") == Some("queued") {
        format!("Queued: the owner answers one card at a time; the card for `{name}` in {place} follows the one he has open.")
    } else {
        format!("Asked the owner for `{name}` in {place}: a card in his Lucina chat and a push to his phone.")
    };
    let project = if r["createProject"] == Value::Bool(true) {
        " The card also offers to create the project, which may not exist yet."
    } else {
        ""
    };
    format!(
        "{head}{project} The value never comes back here: once he saves it, lucina_timeline shows a receipt \"Saved `{name}` in …\"; read it with your own broker caller after he gives that caller access."
    )
}

/// Contract decision summary followed by the complete record.
pub fn decision(r: &Value) -> String {
    let id = s(r, "decision_id").unwrap_or("?");
    let reason = s(r, "reason").unwrap_or("");
    let line = match s(r, "status").unwrap_or("") {
        "pending" => format!("Pending: Lucina asked the owner on his phone (decision {id}). Do not act on this yet; check with lucina_decision_status {{\"decision_id\": \"{id}\"}}."),
        "expired" => format!("Expired: the owner did not answer within 24 h (decision {id}). Ask again or leave it."),
        "withdrawn" => format!("Withdrawn: decision {id} needs no answer. {reason}"),
        _ if s(r, "decision") == Some("owner_reply") => format!("The owner answered: \"{reason}\""),
        _ => {
            let who = if s(r, "decided_by") == Some("owner") { "The owner" } else { "Lucina" };
            let choice = s(r, "chosen_option").unwrap_or("");
            let action = if s(r, "decision") == Some("change") { "CHANGE to" } else { "GO with" };
            let confidence = r.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
            format!("{who} decided: {action} \"{choice}\" (confidence {confidence:.2}). Reason: {reason}  Decision {id}")
        }
    };
    // Keep the human summary on one line even when the owner replies with newlines.
    format!(
        "{}\n\n```json\n{}\n```",
        line.replace(['\n', '\r'], " "),
        serde_json::to_string_pretty(r).unwrap_or_default()
    )
}

/// Job summary and full payload, including result/error and lifecycle timestamps.
pub fn job(r: &Value) -> String {
    format!(
        "Job {}: {}, action {}.\n\n```json\n{}\n```",
        scalar(r.get("id")),
        s(r, "status").unwrap_or("?"),
        s(r, "action").unwrap_or("?"),
        serde_json::to_string_pretty(r).unwrap_or_default()
    )
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
        let who =
            if role == "user" && it.pointer("/meta/via").and_then(Value::as_str) == Some("mcp") {
                let caller = it
                    .pointer("/meta/caller")
                    .and_then(Value::as_str)
                    .filter(|c| !c.trim().is_empty())
                    .unwrap_or("mcp");
                match it
                    .pointer("/meta/conversation")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                {
                    Some(c) => format!("{caller} ({c})"),
                    None => caller.to_owned(),
                }
            } else if role == "user" {
                "owner".to_owned()
            } else {
                "Lucina".to_owned()
            };
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
        let source = if it.pointer("/meta/via").and_then(Value::as_str) == Some("mcp") {
            match it.pointer("/meta/caller").and_then(Value::as_str) {
                Some(caller) if !caller.trim().is_empty() => format!("mcp:{caller}"),
                _ => "mcp".to_owned(),
            }
        } else {
            ch.to_owned()
        };
        lines.push(format!(
            "[{} · {source}] {who}: {text}",
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

/// The answer to a `shopping_add` batch. `items` pairs each op's `opId` with
/// the name shown to the caller. Ops in `applied` or `duplicates` count as
/// added; `rejected` ones are named with their reason, and Lucina's question
/// (`needs_confirmation`) is passed on for the owner.
pub fn add_result(items: &[(String, String)], list_id: &str, r: &Value) -> String {
    let ids = |k: &str| -> Vec<&str> {
        r.get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    };
    let (applied, dups) = (ids("applied"), ids("duplicates"));
    let added: Vec<&str> = items
        .iter()
        .filter(|(op, _)| applied.contains(&op.as_str()) || dups.contains(&op.as_str()))
        .map(|(_, n)| n.as_str())
        .collect();
    let mut out = if added.is_empty() {
        format!("Nothing was added to list {list_id}.")
    } else {
        format!("Added on list {list_id}: {}.", added.join(", "))
    };
    let dup = items
        .iter()
        .filter(|(op, _)| dups.contains(&op.as_str()))
        .count();
    if dup > 0 {
        out.push_str(&format!(" ({dup} already applied before.)"));
    }
    let needed = r.get("items").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter(|i| s(i, "status") == Some("needed"))
            .count()
    });
    if let Some(n) = needed {
        out.push_str(&format!(" The list now has {n} item(s) to buy."));
    }
    for rej in r
        .get("rejected")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let op = s(rej, "opId").unwrap_or_default();
        let name = items
            .iter()
            .find(|(id, _)| id == op)
            .map(|(_, n)| n.as_str())
            .or_else(|| s(rej, "text"))
            .unwrap_or(op);
        let error = s(rej, "error").unwrap_or("rejected");
        match (error, s(rej, "question")) {
            ("needs_confirmation", Some(q)) => {
                out.push_str(&format!("\nNot added: {name}. Lucina asks: {q} "));
                match s(rej, "suggestion").filter(|x| !x.trim().is_empty()) {
                    Some(sug) => out.push_str(&format!(
                        "Ask the owner; if they agree, call shopping_add again with the suggested item ({sug})."
                    )),
                    None => out.push_str("Ask the owner what they meant."),
                }
            }
            _ => out.push_str(&format!("\nNot added: {name} ({error}).")),
        }
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
    out.push_str("\nUse lucina_decide for go-aheads; ask_lucina is answers-only for agents.");
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

// -- notes -------------------------------------------------------------------------

/// "3/7 done" for checklist items (`{text, checked, indent}`).
pub fn progress(items: &[Value]) -> String {
    let done = items
        .iter()
        .filter(|i| i.get("checked").and_then(Value::as_bool) == Some(true))
        .count();
    format!("{done}/{} done", items.len())
}

/// Numbered checklist lines, as note_update's `check` / `uncheck` / `removeItems` count them.
pub fn checklist(items: &[Value]) -> String {
    let mut out: Vec<String> = items
        .iter()
        .enumerate()
        .take(300)
        .map(|(i, it)| {
            let mark = if it.get("checked").and_then(Value::as_bool) == Some(true) {
                "x"
            } else {
                " "
            };
            let indent = if it.get("indent").and_then(Value::as_u64).unwrap_or(0) > 0 {
                "   "
            } else {
                ""
            };
            format!(
                "{indent}{}. [{mark}] {}",
                i + 1,
                s(it, "text").unwrap_or("")
            )
        })
        .collect();
    if items.len() > 300 {
        out.push(format!("… and {} more items.", items.len() - 300));
    }
    out.join("\n")
}

fn flag(n: &Value, key: &str) -> bool {
    n.get(key).and_then(Value::as_bool) == Some(true)
}

fn byte_size(b: u64) -> String {
    match b {
        0..=1023 => format!("{b} B"),
        1024..=1_048_575 => format!("{} KB", b / 1024),
        _ => format!("{:.1} MB", b as f64 / 1_048_576.0),
    }
}

/// The reminder of a note or summary: `2026-11-03 09:00 (weekly)`.
fn reminder_text(n: &Value, tz: Tz) -> Option<String> {
    let (at, rec) = match n.get("reminder").filter(|r| r.is_object()) {
        Some(r) => (r.get("at"), s(r, "recurrence")),
        None => (n.get("reminderAt").filter(|v| !v.is_null()), None),
    };
    let at = at?;
    let mut t = local_time(Some(at), tz);
    if let Some(rec) = rec {
        t.push_str(&format!(" ({rec})"));
    }
    Some(t)
}

/// Checklist progress of a summary (`itemsTotal`, `itemsDone`) or a full note (`items`).
fn list_progress(n: &Value) -> Option<String> {
    if let Some(items) = n.get("items").and_then(Value::as_array) {
        return Some(progress(items));
    }
    if flag(n, "isList") {
        let total = n.get("itemsTotal").and_then(Value::as_u64).unwrap_or(0);
        let done = n.get("itemsDone").and_then(Value::as_u64).unwrap_or(0);
        return Some(format!("{done}/{total} done"));
    }
    None
}

fn note_head(n: &Value, tz: Tz) -> String {
    // Display names (`labels`) when the answer has them, else the stored tags.
    let labels: Vec<&str> = ["labels", "tags"]
        .iter()
        .find_map(|k| n.get(*k).and_then(Value::as_array))
        .map(|t| t.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let title = s(n, "title").filter(|t| !t.trim().is_empty());
    let mut line = format!(
        "\"{}\" (slug {}, id {})",
        title.unwrap_or("untitled"),
        s(n, "slug").unwrap_or("?"),
        s(n, "id").unwrap_or("?"),
    );
    if flag(n, "pinned") {
        line.push_str(" [pinned]");
    }
    if flag(n, "archived") {
        line.push_str(" [archived]");
    }
    if n.get("deletedAt").is_some_and(|d| !d.is_null()) {
        line.push_str(" [in the trash]");
    }
    line.push_str(&format!(
        " · by {} · updated {}",
        s(n, "source").unwrap_or("owner"),
        local_time(n.get("updatedAt"), tz)
    ));
    if let Some(c) = s(n, "color").filter(|c| *c != "default") {
        line.push_str(&format!(" · colour {c}"));
    }
    if !labels.is_empty() {
        line.push_str(&format!(" · labels: {}", labels.join(", ")));
    }
    if let Some(p) = list_progress(n) {
        line.push_str(&format!(" · checklist {p}"));
    }
    if let Some(r) = reminder_text(n, tz) {
        line.push_str(&format!(" · reminder {r}"));
    }
    line
}

/// Attachments, links and collaborators of a full note, one line each.
fn note_extras(n: &Value) -> String {
    let mut out = String::new();
    let list = |k: &str| {
        n.get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let atts = list("attachments");
    if !atts.is_empty() {
        let parts: Vec<String> = atts
            .iter()
            .map(|a| {
                format!(
                    "{} ({}, {})",
                    s(a, "name").or_else(|| s(a, "file")).unwrap_or("file"),
                    s(a, "mimetype").unwrap_or("?"),
                    byte_size(a.get("size").and_then(Value::as_u64).unwrap_or(0))
                )
            })
            .collect();
        out.push_str(&format!("\nAttachments: {}", parts.join("; ")));
    }
    let links = list("annotations");
    if !links.is_empty() {
        let parts: Vec<String> = links
            .iter()
            .map(|l| match s(l, "title").filter(|t| !t.is_empty()) {
                Some(t) => format!("{t} <{}>", s(l, "url").unwrap_or("?")),
                None => format!("<{}>", s(l, "url").unwrap_or("?")),
            })
            .collect();
        out.push_str(&format!("\nLinks: {}", parts.join("; ")));
    }
    let people: Vec<String> = list("collaborators")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if !people.is_empty() {
        out.push_str(&format!("\nShared with: {}", people.join(", ")));
    }
    out
}

/// One note with its content from `offset` (characters), at most `max`. A
/// checklist note shows its numbered items instead of the derived content.
pub fn note(n: &Value, offset: usize, max: usize, tz: Tz) -> String {
    let mut out = format!("Note {}", note_head(n, tz));
    out.push_str(&note_extras(n));
    if let Some(items) = n.get("items").and_then(Value::as_array) {
        out.push_str(&format!("\nChecklist, {}:\n", progress(items)));
        if items.is_empty() {
            out.push_str("(no items)");
        } else {
            out.push_str(&checklist(items));
        }
        return out;
    }
    let content = s(n, "content").unwrap_or("");
    let total = content.chars().count();
    let part: String = content.chars().skip(offset).take(max).collect();
    out.push_str(&format!("\n{total} characters."));
    if offset > 0 || offset + max < total {
        let end = (offset + max).min(total);
        out.push_str(&format!(" Showing characters {offset}-{end}."));
    }
    out.push_str("\n\n");
    if total == 0 {
        out.push_str("(empty)");
    } else if part.is_empty() {
        out.push_str(&format!("(nothing at offset {offset})"));
    } else {
        out.push_str(&part);
    }
    if offset + max < total {
        out.push_str(&format!(
            "\n\n[…truncated: call note_get again with offset={} for the rest]",
            offset + max
        ));
    }
    out
}

/// `GET /v1/notes` answer as a list; `what` describes the filters (" matching \"x\"").
pub fn notes_list(v: &Value, what: &str, tz: Tz) -> String {
    let notes = v
        .get("notes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if notes.is_empty() {
        return format!("No notes{what}.");
    }
    let mut out = format!("{} note(s){what}:\n", notes.len());
    for n in &notes {
        let size = n.get("size").and_then(Value::as_u64).unwrap_or(0);
        out.push_str(&format!("- {} · {size} bytes\n", note_head(n, tz)));
        if let Some(e) = s(n, "excerpt").filter(|e| !e.is_empty()) {
            out.push_str(&format!("  {}\n", clip(e, 240)));
        }
    }
    out.push_str("Read one with note_get (slug or id).");
    out
}

/// `GET /v1/notes/labels`.
pub fn note_labels(v: &Value) -> String {
    let labels = v
        .get("labels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if labels.is_empty() {
        return "No labels yet.".to_owned();
    }
    let mut out = format!("{} label(s):\n", labels.len());
    for l in &labels {
        let n = l.get("count").and_then(Value::as_u64).unwrap_or(0);
        out.push_str(&format!(
            "- {} ({n} note{})\n",
            s(l, "name").or_else(|| s(l, "tag")).unwrap_or("?"),
            if n == 1 { "" } else { "s" }
        ));
    }
    out.push_str("Find a label's notes with notes_search label=<name>.");
    out
}

// -- tasks -------------------------------------------------------------------------

fn due_label(d: &Value) -> String {
    let date = s(d, "date").unwrap_or("");
    let rel = d.get("rel_days").and_then(Value::as_i64).unwrap_or(0);
    let mut out = match rel {
        0 => "today".to_owned(),
        1 => "tomorrow".to_owned(),
        r if r < 0 => format!("overdue since {date} ({} d)", -r),
        _ => date.to_owned(),
    };
    if let Some(t) = s(d, "time") {
        out.push_str(&format!(" {t}"));
    }
    if d.get("is_recurring").and_then(Value::as_bool) == Some(true) {
        out.push_str(&format!(" ↻ {}", s(d, "string").unwrap_or("")));
    }
    out
}

fn task_line(t: &Value, depth: usize) -> String {
    let mut line = format!(
        "\n{}- {}",
        "  ".repeat(depth),
        clip(s(t, "content").unwrap_or("(untitled)"), 300)
    );
    if t.get("checked").and_then(Value::as_bool) == Some(true) {
        line.push_str(" ✓");
    }
    if let Some(p) = s(t, "project_name") {
        line.push_str(&format!(" · {p}"));
    }
    if let Some(d) = t.get("due").filter(|d| d.is_object()) {
        line.push_str(&format!(" · {}", due_label(d)));
    }
    if let Some(p @ 1..=3) = t.get("priority").and_then(Value::as_u64) {
        line.push_str(&format!(" · p{p}"));
    }
    if let Some(ls) = t
        .get("labels")
        .and_then(Value::as_array)
        .filter(|l| !l.is_empty())
    {
        let names: Vec<String> = ls
            .iter()
            .filter_map(Value::as_str)
            .map(|l| format!("@{l}"))
            .collect();
        line.push_str(&format!(" · {}", names.join(" ")));
    }
    if t.get("pending").and_then(Value::as_bool) == Some(true) {
        line.push_str(" (not yet in Todoist)");
    }
    line.push_str(&format!(" [id {}]", scalar(t.get("id"))));
    for c in t
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        line.push_str(&task_line(c, depth + 1));
    }
    line
}

/// `GET /v1/tasks/state` → the list, grouped like the app.
pub fn tasks(r: &Value) -> String {
    let v = r.get("view").unwrap_or(&Value::Null);
    let list = v.get("list").unwrap_or(&Value::Null);
    let title = s(list, "title").unwrap_or("Tasks");
    let mut out = format!("{title} (today is {}", s(v, "today").unwrap_or("?"));
    if let Some(g) = v.get("goal") {
        out.push_str(&format!(
            "; done today {}/{}",
            g.get("completed_today")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            g.get("daily_goal").and_then(Value::as_u64).unwrap_or(0)
        ));
    }
    out.push_str("):");
    let mut n = 0;
    for g in list
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let tasks = g
            .get("tasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if tasks.is_empty() {
            continue;
        }
        let head = match s(g, "kind") {
            Some("overdue") => "Overdue".to_owned(),
            Some("date") | Some("completed") => {
                let date = s(g, "date").unwrap_or("");
                match g.get("rel_days").and_then(Value::as_i64) {
                    Some(0) => format!("Today {date}"),
                    Some(1) => format!("Tomorrow {date}"),
                    Some(-1) => format!("Yesterday {date}"),
                    _ => date.to_owned(),
                }
            }
            Some("section") => s(g, "title").unwrap_or("Section").to_owned(),
            _ => String::new(),
        };
        if !head.is_empty() {
            out.push_str(&format!("\n{head}:"));
        }
        for t in tasks.iter().take(200) {
            out.push_str(&task_line(t, 0));
            n += 1;
        }
    }
    if n == 0 {
        out.push_str("\nNothing here.");
    }
    if let Some(w) = s(list, "warning") {
        out.push_str(&format!("\nNote: {w}"));
    }
    if let Some(sy) = v.get("sync") {
        match s(sy, "status") {
            Some("disconnected") => out.push_str("\nTodoist is not connected (the owner connects it in Secretary → Links); these are local tasks only."),
            Some("reconnect") => out.push_str("\nTodoist needs reconnecting in Secretary → Links; changes wait until then."),
            Some("error") | Some("rate_limited") => out.push_str(&format!(
                "\nTodoist sync is retrying: {}",
                s(sy, "error").unwrap_or("temporary error")
            )),
            _ => {}
        }
    }
    out
}

/// `(id, content)` of open tasks in a state answer whose title contains
/// every word of `words` (accent- and case-insensitive).
pub fn task_matches(r: &Value, words: &str) -> Vec<(String, String)> {
    fn walk(t: &Value, out: &mut Vec<(String, String)>) {
        out.push((
            scalar(t.get("id")),
            s(t, "content").unwrap_or("").to_owned(),
        ));
        for c in t
            .get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            walk(c, out);
        }
    }
    let mut all = Vec::new();
    for g in r
        .pointer("/view/list/groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for t in g
            .get("tasks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            walk(t, &mut all);
        }
    }
    let want: Vec<String> = fold(words).split_whitespace().map(str::to_owned).collect();
    let mut hits: Vec<(String, String)> = all
        .into_iter()
        .filter(|(_, c)| {
            let f = fold(c);
            want.iter().all(|w| f.contains(w.as_str()))
        })
        .collect();
    hits.dedup_by(|a, b| a.0 == b.0);
    hits
}

fn find_task<'a>(r: &'a Value, id: &str) -> Option<&'a Value> {
    fn walk<'a>(t: &'a Value, id: &str) -> Option<&'a Value> {
        if scalar(t.get("id")) == id {
            return Some(t);
        }
        t.get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|c| walk(c, id))
    }
    r.pointer("/view/list/groups")?
        .as_array()?
        .iter()
        .flat_map(|g| {
            g.get("tasks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find_map(|t| walk(t, id))
}

pub fn task_added(r: &Value) -> String {
    let p = r.get("parsed").unwrap_or(&Value::Null);
    let mut out = format!("Added \"{}\"", s(p, "content").unwrap_or("?"));
    if let Some(d) = s(p, "due_string") {
        out.push_str(&format!(", due \"{d}\""));
        if let Some(date) = s(p, "due_date") {
            out.push_str(&format!(" ({})", date.replace('T', " ")));
        }
        if p.get("is_recurring").and_then(Value::as_bool) == Some(true) {
            out.push_str(", recurring");
        }
    }
    if let Some(pr) = s(p, "project") {
        out.push_str(&format!(", project {pr}"));
    }
    if let Some(n) = p.get("priority").and_then(Value::as_u64) {
        out.push_str(&format!(", p{n}"));
    }
    out.push_str(&format!(
        ". Task id {} (temporary until Todoist confirms it); it syncs to Todoist within a minute.",
        scalar(r.get("task_id"))
    ));
    out
}

pub fn task_completed(r: &Value, id: &str) -> String {
    match find_task(r, id).and_then(|t| t.get("due")).filter(|d| d.get("is_recurring").and_then(Value::as_bool) == Some(true)) {
        Some(d) => format!(
            "Done. It is a recurring task: next time {} (Todoist confirms the date at the next sync).",
            due_label(d)
        ),
        None => format!("Marked task {id} done; Todoist gets it within a minute. The owner can undo it in the app."),
    }
}

// -- notebooks ---------------------------------------------------------------------

/// Characters of a passage shown in a citation or a search hit.
const MAX_EXCERPT: usize = 240;
/// Characters of an insight's text shown in a listing.
const MAX_INSIGHT: usize = 3000;

fn items<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn noun(n: u64, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

fn mmss(secs: f64) -> String {
    let t = secs.max(0.0).round() as u64;
    format!("{}:{:02}", t / 60, t % 60)
}

fn nb_counts(n: &Value) -> String {
    let get = |k: &str| n.get("counts").and_then(|c| c.get(k)).and_then(Value::as_u64).unwrap_or(0);
    format!(
        "{}, {}, {}, {}",
        noun(get("sources"), "source"),
        noun(get("notes"), "note"),
        noun(get("chats"), "chat"),
        noun(get("podcasts"), "podcast")
    )
}

/// `GET /v1/notebooks`.
pub fn notebooks(v: &Value) -> String {
    let list = items(v, "notebooks");
    if list.is_empty() {
        return "No notebooks. Create one with notebook_create.".to_owned();
    }
    let mut out = format!("{} notebook(s):\n", list.len());
    for n in list {
        let archived = if n.get("archived").and_then(Value::as_bool) == Some(true) { " [archived]" } else { "" };
        out.push_str(&format!(
            "- {} · {}{archived} · {}\n",
            s(n, "id").unwrap_or("?"),
            s(n, "name").unwrap_or("(unnamed)"),
            nb_counts(n)
        ));
        if let Some(d) = s(n, "description") {
            out.push_str(&format!("  {}\n", clip(d, 160)));
        }
    }
    out.push_str("Open one with notebook_get.");
    out
}

fn source_line(src: &Value) -> String {
    let status = s(src, "status").unwrap_or("?");
    let mut line = format!(
        "- {} · {} · {} · {status}",
        s(src, "id").unwrap_or("?"),
        s(src, "kind").unwrap_or("?"),
        s(src, "title").unwrap_or("(untitled)")
    );
    if status == "failed" {
        if let Some(e) = s(src, "error") {
            line.push_str(&format!(" ({})", clip(e, 120)));
        }
    }
    let num = |k: &str| src.get(k).and_then(Value::as_u64);
    let mut facts = Vec::new();
    if let Some(p) = num("pages") {
        facts.push(noun(p, "page"));
    }
    if let Some(c) = num("chunks") {
        facts.push(noun(c, "passage"));
    }
    if let Some(d) = src.get("duration_s").and_then(Value::as_f64) {
        facts.push(mmss(d));
    }
    if let Some(c) = s(src, "context") {
        facts.push(format!("context {c}"));
    }
    if !facts.is_empty() {
        line.push_str(&format!(" · {}", facts.join(", ")));
    }
    line
}

/// `GET /v1/notebooks/{nb}` as compact text: the notebook with its sources, notes, chats and podcasts.
pub fn notebook(v: &Value, tz: Tz) -> String {
    let archived = if v.get("archived").and_then(Value::as_bool) == Some(true) { " [archived]" } else { "" };
    let mut out = format!("{} ({}){archived}\n{}", s(v, "name").unwrap_or("(unnamed)"), s(v, "id").unwrap_or("?"), nb_counts(v));
    if v.get("updated_at").is_some() {
        out.push_str(&format!(" · updated {}", local_time(v.get("updated_at"), tz)));
    }
    if let Some(d) = s(v, "description") {
        out.push_str(&format!("\n{d}"));
    }
    let sources = items(v, "sources");
    if sources.is_empty() {
        out.push_str("\n\nNo sources yet: add one with notebook_add_source.");
    } else {
        out.push_str(&format!("\n\nSources ({}):", sources.len()));
        for src in sources {
            out.push_str(&format!("\n{}", source_line(src)));
        }
    }
    let notes = items(v, "notes");
    if !notes.is_empty() {
        out.push_str(&format!("\n\nNotes ({}):", notes.len()));
        for n in notes {
            let indexed = if n.get("indexed").and_then(Value::as_bool) == Some(false) { " (not indexed yet)" } else { "" };
            out.push_str(&format!("\n- {} · {}{indexed}", s(n, "slug").unwrap_or("?"), s(n, "title").unwrap_or("(untitled)")));
        }
    }
    let chats = items(v, "chats");
    if !chats.is_empty() {
        out.push_str(&format!("\n\nChats ({}):", chats.len()));
        for c in chats {
            let n = c.get("messages").and_then(Value::as_u64).unwrap_or(0);
            out.push_str(&format!("\n- {} · {} · {}", s(c, "id").unwrap_or("?"), s(c, "title").unwrap_or("(untitled)"), noun(n, "message")));
        }
    }
    let podcasts = items(v, "podcasts");
    if !podcasts.is_empty() {
        out.push_str(&format!("\n\nPodcasts ({}):", podcasts.len()));
        for p in podcasts {
            out.push_str(&format!("\n- {}", podcast_head(p)));
        }
    }
    out.push_str("\n\nAsk it with notebook_ask; answers cite [source:<source id>-<n>].");
    out
}

/// `POST /v1/notebooks/{nb}/sources` (202).
pub fn notebook_source_added(v: &Value) -> String {
    format!(
        "Added source {} ({}, {}): {}. It is read in the background (extract, split, embed); \
         notebook_get shows when it is ready, and questions can use it from then on.",
        s(v, "id").unwrap_or("?"),
        s(v, "kind").unwrap_or("?"),
        s(v, "title").unwrap_or("untitled"),
        s(v, "status").unwrap_or("queued")
    )
}

/// One citation of an answer, as a line of the numbered list.
fn citation_line(n: usize, c: &Value) -> String {
    let rf = s(c, "ref").map(str::to_owned).unwrap_or_else(|| match (s(c, "source_id"), c.get("chunk").and_then(Value::as_u64)) {
        (Some(id), Some(k)) => format!("source:{id}-{k}"),
        _ => s(c, "kind").unwrap_or("?").to_owned(),
    });
    let mut line = format!("{n}. [{rf}] {}", s(c, "title").unwrap_or("(untitled)"));
    if let Some(p) = c.get("page").and_then(Value::as_u64) {
        line.push_str(&format!(", page {p}"));
    }
    if let Some(t) = c.get("start_s").and_then(Value::as_f64) {
        line.push_str(&format!(", at {}", mmss(t)));
    }
    if let Some(e) = s(c, "excerpt") {
        line.push_str(&format!(": \"{}\"", clip(&e.split_whitespace().collect::<Vec<_>>().join(" "), MAX_EXCERPT)));
    }
    line
}

/// The `reply` of `POST /v1/notebooks/chats/{c}/messages`: the answer, its numbered citations and the chat to continue.
pub fn notebook_answer(chat_id: &str, reply: &Value) -> String {
    let mut out = s(reply, "text").unwrap_or("(the notebook gave no answer)").trim().to_owned();
    let cites = items(reply, "citations");
    if cites.is_empty() {
        out.push_str("\n\nNo citations.");
    } else {
        out.push_str("\n\nCitations:");
        for (i, c) in cites.iter().enumerate() {
            out.push_str(&format!("\n{}", citation_line(i + 1, c)));
        }
    }
    out.push_str(&format!("\n\nChat {chat_id}: pass it as chat_id to ask a follow-up."));
    out
}

/// `GET /v1/notebooks/{nb}/search` (or `/search` for every notebook).
pub fn notebook_hits(v: &Value, query: &str) -> String {
    let hits = items(v, "hits");
    if hits.is_empty() {
        return format!("Nothing in the notebooks matches \"{query}\".");
    }
    let mut out = format!("{} hit(s) for \"{query}\":\n", hits.len());
    for h in hits {
        let score = h.get("score").and_then(Value::as_f64).map(|x| format!(" · score {x:.2}")).unwrap_or_default();
        let nb = s(h, "notebook_id").map(|n| format!(" · notebook {n}")).unwrap_or_default();
        out.push_str(&format!("- [{}] {}{score}{nb}\n", s(h, "ref").unwrap_or("?"), s(h, "title").unwrap_or("(untitled)")));
        if let Some(e) = s(h, "excerpt") {
            out.push_str(&format!("  {}\n", clip(&e.split_whitespace().collect::<Vec<_>>().join(" "), MAX_EXCERPT)));
        }
    }
    out.push_str("Ask a question over the sources with notebook_ask.");
    out
}

/// An insight as the one line `notebook_transform` answers, plus its text once ready.
pub fn notebook_insight(i: &Value) -> String {
    let mut out = format!(
        "Insight {} ({}, transformation {}) on source {}: {}.",
        s(i, "id").unwrap_or("?"),
        s(i, "title").unwrap_or("untitled"),
        s(i, "transformation_id").unwrap_or("?"),
        s(i, "source_id").unwrap_or("?"),
        s(i, "status").unwrap_or("?")
    );
    match s(i, "text") {
        Some(t) => out.push_str(&format!("\n\n{}", clip(t.trim(), MAX_INSIGHT))),
        None if matches!(s(i, "status"), Some("queued" | "running")) => out.push_str(" Call notebook_transform with this source_id again to read it when it is ready."),
        None => {}
    }
    out
}

/// `GET /v1/notebooks/transformations`.
pub fn notebook_transformations(v: &Value) -> String {
    let list = items(v, "transformations");
    if list.is_empty() {
        return "No transformations are defined.".to_owned();
    }
    let mut out = String::from("Transformations (pass the id as transformation_id):\n");
    for t in list {
        let default = if t.get("apply_default").and_then(Value::as_bool) == Some(true) { " [applied to new sources]" } else { "" };
        out.push_str(&format!("- {} · {}{default}", s(t, "id").unwrap_or("?"), s(t, "name").unwrap_or("?")));
        if let Some(d) = s(t, "description") {
            out.push_str(&format!(": {}", clip(d, 160)));
        }
        out.push('\n');
    }
    out.trim_end().to_owned()
}

/// `GET /v1/notebooks/sources/{src}/insights`.
pub fn notebook_insights(v: &Value) -> String {
    let list = items(v, "insights");
    if list.is_empty() {
        return "No insights on this source yet.".to_owned();
    }
    list.iter().map(notebook_insight).collect::<Vec<_>>().join("\n\n")
}

fn podcast_head(p: &Value) -> String {
    let mut line = format!("{} · {} · {}", s(p, "id").unwrap_or("?"), s(p, "title").unwrap_or("(untitled)"), s(p, "status").unwrap_or("?"));
    if let Some(d) = p.get("duration_s").and_then(Value::as_f64) {
        line.push_str(&format!(" · {}", mmss(d)));
    }
    line
}

/// A podcast: where it stands, what it covers, and whether the audio can be played.
pub fn notebook_podcast(p: &Value) -> String {
    let mut out = format!("Podcast {}", podcast_head(p));
    if let (Some(e), Some(sp)) = (s(p, "episode_profile"), s(p, "speaker_profile")) {
        out.push_str(&format!("\n{e}, voices {sp}"));
    }
    if s(p, "status") == Some("failed") {
        out.push_str(&format!("\nFailed: {}", s(p, "error").unwrap_or("no reason given")));
    }
    let outline = items(p, "outline");
    if !outline.is_empty() {
        out.push_str("\nOutline:");
        for (i, o) in outline.iter().enumerate() {
            out.push_str(&format!("\n{}. {}", i + 1, s(o, "name").unwrap_or("?")));
        }
    }
    let lines = items(p, "transcript").len();
    if lines > 0 {
        out.push_str(&format!("\nScript: {}.", noun(lines as u64, "line")));
    }
    out.push_str(match (s(p, "status"), p.get("audio").and_then(Value::as_bool)) {
        (_, Some(true)) => "\nThe audio is ready: the owner plays it in the Secretary app.",
        (Some("failed"), _) => "",
        _ => "\nNot ready yet: call notebook_podcast with this podcast_id again in a minute or two.",
    });
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
    fn timeline_mcp_sources_keep_stored_channel_filter() {
        let page = json!({"items": [
            {"id": "a", "channel": "web", "role": "user", "text": "Hi", "meta": {"via": "mcp", "caller": "claude-code"}},
            {"id": "b", "channel": "web", "role": "lucina", "text": "Hello", "meta": {"via": "mcp", "caller": "www"}},
            {"id": "c", "channel": "web", "role": "lucina", "text": "Missing", "meta": {"via": "mcp"}},
            {"id": "d", "channel": "web", "role": "user", "text": "Empty", "meta": {"via": "mcp", "caller": "  "}},
            {"id": "e", "channel": "web", "role": "user", "text": "App", "meta": {"caller": "test"}}
        ]});
        let t = timeline(&page, Some("web"), 20, chrono_tz::UTC);
        assert!(t.contains(" · mcp:claude-code] claude-code: Hi"), "{t}");
        assert!(t.contains(" · mcp:www] Lucina: Hello"), "{t}");
        assert!(t.contains(" · mcp] Lucina: Missing"), "{t}");
        assert!(t.contains(" · mcp] mcp: Empty"), "{t}");
        assert!(t.contains(" · web] owner: App"), "{t}");
        assert_eq!(
            timeline(&page, Some("mcp"), 20, chrono_tz::UTC),
            "No messages."
        );
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

    #[test]
    fn add_result_names_added_and_rejected_items() {
        let items: Vec<(String, String)> = [("o1", "latte"), ("o2", "lavatrici"), ("o3", "pane")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let r = json!({
            "applied": ["o1"], "duplicates": ["o3"],
            "rejected": [{"opId": "o2", "error": "needs_confirmation", "text": "lavatrici",
                          "question": "Intendevi Detersivo per lavatrice?",
                          "suggestion": "Detersivo per lavatrice"}],
            "items": [{"status": "needed"}, {"status": "needed"}, {"status": "recent"}]
        });
        assert_eq!(
            add_result(&items, "default", &r),
            "Added on list default: latte, pane. (1 already applied before.) \
             The list now has 2 item(s) to buy.\n\
             Not added: lavatrici. Lucina asks: Intendevi Detersivo per lavatrice? \
             Ask the owner; if they agree, call shopping_add again with the suggested item \
             (Detersivo per lavatrice)."
        );

        // null suggestion, other codes, nothing added
        let r = json!({
            "applied": [],
            "rejected": [
                {"opId": "o1", "error": "duplicate_item"},
                {"opId": "o2", "error": "needs_confirmation", "text": "lavatrici",
                 "question": "Cosa intendevi?", "suggestion": null},
                {"opId": "o3", "error": "item_not_found"},
            ],
            "items": []
        });
        assert_eq!(
            add_result(&items, "l2", &r),
            "Nothing was added to list l2. The list now has 0 item(s) to buy.\n\
             Not added: latte (duplicate_item).\n\
             Not added: lavatrici. Lucina asks: Cosa intendevi? Ask the owner what they meant.\n\
             Not added: pane (item_not_found)."
        );

        // no rejected key (older Lucina): plain success
        let r = json!({"applied": ["o1", "o2", "o3"], "duplicates": []});
        assert_eq!(
            add_result(&items, "default", &r),
            "Added on list default: latte, lavatrici, pane."
        );
    }
}

#[cfg(test)]
mod decision_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn contract_answer_text() {
        let mut r = json!({"decision_id":"dc-012345abcdef","status":"decided","decision":"go","chosen_option":"Ship","confidence":0.86,"reason":"Small fix","decided_by":"lucina"});
        for (by, choice, expected) in [
            ("lucina", "go", "Lucina decided: GO with \"Ship\" (confidence 0.86). Reason: Small fix  Decision dc-012345abcdef"),
            ("lucina", "change", "Lucina decided: CHANGE to \"Ship\" (confidence 0.86). Reason: Small fix  Decision dc-012345abcdef"),
            ("owner", "go", "The owner decided: GO with \"Ship\" (confidence 0.86). Reason: Small fix  Decision dc-012345abcdef"),
            ("owner", "owner_reply", "The owner answered: \"Small fix\""),
        ] {
            r["decided_by"] = json!(by);
            r["decision"] = json!(choice);
            assert_eq!(decision(&r), format!("{expected}\n\n```json\n{}\n```", serde_json::to_string_pretty(&r).unwrap()));
        }
        r["status"] = json!("pending");
        assert_eq!(decision(&r).lines().next().unwrap(), "Pending: Lucina asked the owner on his phone (decision dc-012345abcdef). Do not act on this yet; check with lucina_decision_status {\"decision_id\": \"dc-012345abcdef\"}.");
        r["status"] = json!("expired");
        assert_eq!(decision(&r).lines().next().unwrap(), "Expired: the owner did not answer within 24 h (decision dc-012345abcdef). Ask again or leave it.");
        r["status"] = json!("withdrawn");
        r["reason"] = json!("Withdrawn by the asker: settled\nelsewhere");
        assert_eq!(decision(&r).lines().next().unwrap(), "Withdrawn: decision dc-012345abcdef needs no answer. Withdrawn by the asker: settled elsewhere");
    }

    #[test]
    fn consult_and_job_lines() {
        assert_eq!(chat_reply(&json!({"reply":"Hello","consult":true,"notRun":"research","hint":"Use agent_run."})), "Hello\n\n[Lucina only answered: this is a consult turn, so she did not run research. Use agent_run.]");
        assert_eq!(
            chat_reply(&json!({"reply":"Hello","consult":true,"notRun":"research"})),
            "Hello\n\n[Lucina only answered: this is a consult turn, so she did not run research.]"
        );
        assert_eq!(chat_reply(&json!({"reply":"Started","action":"research","job":{"id":"j1"}})), "Started\n\n[Lucina started a background job: research, id j1. Follow it with lucina_job_status {\"job_id\": \"j1\"}; the result also appears in lucina_timeline.]");
    }

    #[test]
    fn mcp_conversation_speaker() {
        let r = json!({"items":[{"role":"user","channel":"web","text":"Hi","meta":{"via":"mcp","caller":"claude-code","conversation":"coordinator"}}, {"role":"user","channel":"web","text":"Hi","meta":{"via":"mcp","caller":"claude-code","conversation":""}}]});
        let text = timeline(&r, None, 20, chrono_tz::UTC);
        assert!(text.contains("mcp:claude-code] claude-code (coordinator): Hi"));
        assert!(text.contains("mcp:claude-code] claude-code: Hi"));
    }

    #[test]
    fn notebook_texts() {
        assert_eq!(notebooks(&json!({"notebooks": []})), "No notebooks. Create one with notebook_create.");
        let n = json!({"id": "nb1", "name": "Old", "archived": true, "counts": {"sources": 1, "notes": 0, "chats": 0, "podcasts": 0}});
        assert_eq!(
            notebooks(&json!({"notebooks": [n]})),
            "1 notebook(s):\n- nb1 · Old [archived] · 1 source, 0 notes, 0 chats, 0 podcasts\nOpen one with notebook_get."
        );
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let empty = notebook(&json!({"id": "nb2", "name": "New", "updated_at": "2026-10-29T13:05:00Z"}), tz);
        assert!(empty.starts_with("New (nb2)\n0 sources, 0 notes, 0 chats, 0 podcasts · updated 2026-10-29 14:05"), "{empty}");
        assert!(empty.contains("No sources yet: add one with notebook_add_source."), "{empty}");
        let failed = source_line(&json!({"id": "s1", "kind": "audio", "status": "failed", "error": "no speech found", "duration_s": 75.0}));
        assert_eq!(failed, "- s1 · audio · (untitled) · failed (no speech found) · 1:15");
    }

    #[test]
    fn notebook_citations() {
        let reply = json!({"text": "Yes [source:s1-2].", "citations": [
            {"kind": "source", "source_id": "s1", "chunk": 2, "title": "Book", "excerpt": "a\n  b   c"},
            {"ref": "insight:i1", "kind": "insight", "title": "Summary"}]});
        assert_eq!(
            notebook_answer("c1", &reply),
            "Yes [source:s1-2].\n\nCitations:\n1. [source:s1-2] Book: \"a b c\"\n2. [insight:i1] Summary\n\nChat c1: pass it as chat_id to ask a follow-up."
        );
        let bare = notebook_answer("c1", &json!({"text": "I cannot tell."}));
        assert!(bare.starts_with("I cannot tell.\n\nNo citations."), "{bare}");
        let long = json!({"text": "t", "citations": [{"ref": "source:s-1", "title": "T", "excerpt": "x".repeat(500)}]});
        assert!(notebook_answer("c", &long).contains(&format!("\"{}…\"", "x".repeat(MAX_EXCERPT))));
    }

    #[test]
    fn notebook_insights_and_podcasts() {
        let queued = json!({"id": "i1", "source_id": "s1", "transformation_id": "t-a", "title": "A", "status": "queued", "text": null});
        assert!(notebook_insight(&queued).ends_with("queued. Call notebook_transform with this source_id again to read it when it is ready."));
        let ready = json!({"id": "i1", "source_id": "s1", "transformation_id": "t-a", "title": "A", "status": "ready", "text": " Body "});
        assert!(notebook_insight(&ready).ends_with("ready.\n\nBody"));
        assert_eq!(notebook_insights(&json!({"insights": []})), "No insights on this source yet.");
        assert_eq!(notebook_transformations(&json!({"transformations": []})), "No transformations are defined.");
        let failed = json!({"id": "p1", "title": "T", "status": "failed", "error": "kokoro is down"});
        assert_eq!(notebook_podcast(&failed), "Podcast p1 · T · failed\nFailed: kokoro is down");
        let running = json!({"id": "p1", "title": "T", "status": "voicing", "audio": false});
        assert!(notebook_podcast(&running).ends_with("Not ready yet: call notebook_podcast with this podcast_id again in a minute or two."));
        assert_eq!(notebook_hits(&json!({"hits": []}), "ring"), "Nothing in the notebooks matches \"ring\".");
    }
}
