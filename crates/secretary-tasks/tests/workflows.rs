//! The command protocol and the sync merge, end to end on the engine.

use secretary_tasks::*;
use serde_json::{json, Value};

const TZ: &str = "Europe/Copenhagen";
// Thursday 22 October 2026, 10:00 in Copenhagen
const NOW: u64 = 1_792_656_000_000;

fn cmd(e: &mut Engine, op: &str, command: Value) -> Response {
    let r = e.handle_at(
        &json!({"op_id": op, "timezone": TZ, "command": command}).to_string(),
        NOW,
    );
    assert!(r.ok, "{op}: {:?} {:?}", r.code, r.error);
    r
}

fn view(e: &mut Engine, v: Value) -> View {
    let r = e.handle_at(
        &json!({"timezone": TZ, "command": {"type": "snapshot", "view": v}}).to_string(),
        NOW,
    );
    assert!(r.ok, "{:?}", r.error);
    r.view.unwrap()
}

fn titles(v: &View) -> Vec<(String, Vec<String>)> {
    v.list
        .groups
        .iter()
        .map(|g| {
            (
                g.key.clone(),
                g.tasks.iter().map(|t| t.content.clone()).collect(),
            )
        })
        .collect()
}

fn item(id: &str, content: &str, due: Value, updated: &str) -> Value {
    json!({"id": id, "content": content, "description": "", "project_id": "inbox1",
           "section_id": null, "parent_id": null, "priority": 1, "due": due, "labels": [],
           "child_order": 1, "day_order": -1, "checked": false, "is_deleted": false,
           "added_at": "2026-10-01T08:00:00Z", "updated_at": updated, "completed_at": null})
}

fn full_sync() -> Value {
    json!({
        "sync_token": "t1", "full_sync": true,
        "projects": [
            {"id": "inbox1", "name": "Inbox", "inbox_project": true, "child_order": 0, "color": "charcoal"},
            {"id": "lang", "name": "Lingue", "child_order": 1, "color": "blue"}
        ],
        "sections": [{"id": "sec1", "project_id": "lang", "name": "Speaking", "section_order": 1}],
        "labels": [{"id": "l1", "name": "routine", "item_order": 1}],
        "filters": [{"id": "f1", "name": "Urgent today", "query": "today & p1", "item_order": 1}],
        "items": [
            item("a1", "15 min shadowing", json!({"date": "2026-10-22", "string": "every day", "lang": "en", "is_recurring": true}), "2026-10-21T08:00:00Z"),
            item("a2", "10 min anki/memrise", json!({"date": "2026-10-20", "string": "ogni giorno", "lang": "it", "is_recurring": true}), "2026-10-20T08:00:00Z"),
            item("a3", "20min speaking", json!({"date": "2026-10-23", "string": "tomorrow", "lang": "en", "is_recurring": false}), "2026-10-20T08:00:00Z"),
            item("a4", "Brain Training", Value::Null, "2026-10-20T08:00:00Z"),
        ],
        "user": {"email": "owner@example.com", "daily_goal": 6, "lang": "it", "next_week": 1, "start_day": 1,
                  "tz_info": {"timezone": "Europe/Copenhagen"}},
        "stats": {"days_items": [{"date": "2026-10-22", "total_completed": 2}]}
    })
}

fn synced() -> Engine {
    let mut e = Engine::in_memory();
    e.connect(Some("owner@example.com".into())).unwrap();
    let rep = e.apply_sync(&full_sync(), NOW).unwrap();
    assert!(rep.full && rep.changed);
    e
}

#[test]
fn full_import_and_views() {
    let mut e = synced();
    let v = view(&mut e, json!({"kind": "today"}));
    assert_eq!(
        titles(&v),
        vec![
            ("overdue".into(), vec!["10 min anki/memrise".into()]),
            ("2026-10-22".into(), vec!["15 min shadowing".into()]),
        ]
    );
    assert_eq!(v.counts.today, 2);
    assert_eq!(v.counts.overdue, 1);
    assert_eq!(v.counts.inbox, 4);
    assert_eq!(v.goal.daily_goal, 6);
    assert_eq!(v.goal.completed_today, 2);
    assert_eq!(v.sync.status, SyncStatus::Ok);
    assert_eq!(v.sync.account.as_deref(), Some("owner@example.com"));
    let t = &v.list.groups[1].tasks[0];
    let due = t.due.as_ref().unwrap();
    assert!(due.is_recurring && due.string == "every day" && due.rel_days == 0);
    assert_eq!(t.priority, 4);
    assert_eq!(v.projects[0].name, "Inbox");
    assert_eq!(v.projects[1].sections[0].name, "Speaking");
    assert_eq!(v.filters[0].query.as_deref(), Some("today & p1"));
    let up = view(&mut e, json!({"kind": "upcoming"}));
    assert_eq!(up.list.groups.len(), 8); // overdue + 7 days
    assert_eq!(up.list.groups[2].tasks[0].content, "20min speaking");
    let inbox = view(&mut e, json!({"kind": "inbox"}));
    assert_eq!(inbox.list.count, 4);
    let search = view(&mut e, json!({"kind": "search", "query": "anki"}));
    assert_eq!(search.list.count, 1);
    let f = view(
        &mut e,
        json!({"kind": "filter", "query": "recurring & overdue"}),
    );
    assert_eq!(titles(&f)[0].1, vec!["10 min anki/memrise".to_string()]);
    // a full sync that brings nothing new does not move the revision
    let rev = e.state().revision;
    let rep = e
        .apply_sync(&json!({"sync_token": "t2", "full_sync": false}), NOW + 1000)
        .unwrap();
    assert!(!rep.changed);
    assert_eq!(e.state().revision, rev);
    assert_eq!(e.sync_token().as_deref(), Some("t2"));
}

#[test]
fn quick_add_queues_and_temp_id_maps() {
    let mut e = synced();
    let r = cmd(
        &mut e,
        "q1",
        json!({"type": "quick_add", "text": "Ripasso verbi domani alle 9 #Lingue /Speaking @routine p2"}),
    );
    let tmp = r.task_id.unwrap();
    assert!(tmp.starts_with("tmp-"));
    let p = r.parsed.unwrap();
    assert_eq!(p.content, "Ripasso verbi");
    assert_eq!(p.lang, "it");
    let t = &e.state().tasks[&tmp];
    assert_eq!(t.project_id, "lang");
    assert_eq!(t.section_id.as_deref(), Some("sec1"));
    assert_eq!(t.priority, 3);
    assert_eq!(t.due.as_ref().unwrap().date, "2026-10-23T09:00:00.000000");
    let batch = e.outbox_batch(NOW);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0]["type"], "item_add");
    assert_eq!(batch[0]["temp_id"], json!(tmp));
    assert_eq!(
        batch[0]["args"]["due"],
        json!({"string": "domani alle 9", "lang": "it"})
    );
    assert_eq!(batch[0]["args"]["project_id"], "lang");
    // complete it before Todoist answered: the close rides in the same batch
    cmd(
        &mut e,
        "q2",
        json!({"type": "update", "task_id": tmp, "content": "Ripasso verbi irregolari"}),
    );
    let batch = e.outbox_batch(NOW);
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[1]["args"]["id"], json!(tmp));
    let uuids: Vec<String> = batch
        .iter()
        .map(|c| c["uuid"].as_str().unwrap().to_owned())
        .collect();
    let mut item = item(
        "n1",
        "Ripasso verbi irregolari",
        json!({"date": "2026-10-23T09:00:00", "string": "domani alle 9", "lang": "it", "is_recurring": false}),
        "2026-10-22T08:00:01Z",
    );
    item["project_id"] = json!("lang");
    item["section_id"] = json!("sec1");
    let rep = e
        .apply_sync(
            &json!({"sync_token": "t2", "full_sync": false,
                    "sync_status": {uuids[0].clone(): "ok", uuids[1].clone(): "ok"},
                    "temp_id_mapping": {tmp.clone(): "n1"},
                    "items": [item]}),
            NOW,
        )
        .unwrap();
    assert_eq!(rep.pushed_ok, 2);
    assert!(e.state().outbox.is_empty());
    assert!(!e.state().tasks.contains_key(&tmp));
    assert_eq!(e.state().tasks["n1"].content, "Ripasso verbi irregolari");
}

#[test]
fn recurring_completion_advances_then_todoist_confirms() {
    let mut e = synced();
    cmd(&mut e, "c1", json!({"type": "complete", "task_id": "a1"}));
    let t = &e.state().tasks["a1"];
    assert!(t.provisional);
    assert_eq!(t.due.as_ref().unwrap().date, "2026-10-23");
    let v = view(&mut e, json!({"kind": "today"}));
    assert_eq!(v.goal.completed_today, 2); // local 1, Todoist said 2 before
    assert!(!titles(&v)
        .iter()
        .any(|(_, ts)| ts.contains(&"15 min shadowing".to_string())));
    let batch = e.outbox_batch(NOW);
    assert_eq!(batch[0]["type"], "item_close");
    let uuid = batch[0]["uuid"].as_str().unwrap().to_owned();
    // Todoist moves it to tomorrow and counts the completion
    e.apply_sync(
        &json!({"sync_token": "t2", "sync_status": {uuid: "ok"},
                "items": [item("a1", "15 min shadowing", json!({"date": "2026-10-23", "string": "every day", "lang": "en", "is_recurring": true}), "2026-10-22T08:01:00Z")],
                "stats": {"days_items": [{"date": "2026-10-22", "total_completed": 3}]}}),
        NOW,
    )
    .unwrap();
    let t = &e.state().tasks["a1"];
    assert!(!t.provisional);
    assert_eq!(t.due.as_ref().unwrap().string, "every day");
    assert!(e.state().conflicts.is_empty());
    let v = view(&mut e, json!({"kind": "today"}));
    assert_eq!(v.goal.completed_today, 3);
    let sig = signals::signals(e.state(), TZ.parse().unwrap(), NOW);
    let r = sig.routines.iter().find(|r| r.id == "a1").unwrap();
    assert_eq!(r.streak_days, 1);
    assert_eq!(sig.overdue[0].content, "10 min anki/memrise");
    assert_eq!(sig.overdue[0].days_overdue, 2);
    assert_eq!(sig.load_minutes, 10); // only the overdue anki is still open today
}

#[test]
fn non_recurring_complete_and_undo() {
    let mut e = synced();
    cmd(&mut e, "c1", json!({"type": "complete", "task_id": "a4"}));
    assert!(!e.state().tasks.contains_key("a4"));
    let done = view(&mut e, json!({"kind": "completed"}));
    assert_eq!(done.list.groups[0].tasks[0].content, "Brain Training");
    cmd(&mut e, "c2", json!({"type": "uncomplete", "task_id": "a4"}));
    assert!(e.state().tasks.contains_key("a4"));
    let kinds: Vec<_> = e
        .outbox_batch(NOW)
        .iter()
        .map(|c| c["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds, vec!["item_close", "item_uncomplete"]);
}

#[test]
fn conflict_todoist_wins_with_a_note() {
    let mut e = synced();
    cmd(
        &mut e,
        "u1",
        json!({"type": "update", "task_id": "a3", "content": "20 min speaking (local)"}),
    );
    assert_eq!(
        e.state().outbox[0].base_updated_at.as_deref(),
        Some("2026-10-20T08:00:00Z")
    );
    // the pull before the push shows a change made in the Todoist app
    let rep = e
        .apply_sync(
            &json!({"sync_token": "t2", "items": [item("a3", "20 min speaking with Marco", json!({"date": "2026-10-24", "string": "sat", "lang": "en", "is_recurring": false}), "2026-10-22T07:59:00Z")]}),
            NOW,
        )
        .unwrap();
    assert_eq!(rep.conflicts, 1);
    assert!(e.state().outbox.is_empty());
    assert_eq!(e.state().tasks["a3"].content, "20 min speaking with Marco");
    let v = view(&mut e, json!({"kind": "today"}));
    assert_eq!(v.conflicts[0].kind, "changed_remotely");
    let id = v.conflicts[0].id.clone();
    cmd(
        &mut e,
        "d1",
        json!({"type": "dismiss_conflict", "conflict_id": id}),
    );
    assert!(e.state().conflicts.is_empty());
    // an unchanged task keeps the local edit until it is pushed
    cmd(
        &mut e,
        "u2",
        json!({"type": "update", "task_id": "a4", "priority": 1}),
    );
    e.apply_sync(&json!({"sync_token": "t3", "items": [item("a4", "Brain Training", Value::Null, "2026-10-20T08:00:00Z")]}), NOW)
        .unwrap();
    assert_eq!(e.state().tasks["a4"].priority, 4);
    assert_eq!(e.state().outbox.len(), 1);
}

#[test]
fn rejected_and_retried_commands() {
    let mut e = synced();
    cmd(&mut e, "a", json!({"type": "add", "content": "Bad one"}));
    cmd(
        &mut e,
        "b",
        json!({"type": "update", "task_id": "a4", "content": "Brain Training 2"}),
    );
    let batch = e.outbox_batch(NOW);
    let (u1, u2) = (
        batch[0]["uuid"].as_str().unwrap().to_owned(),
        batch[1]["uuid"].as_str().unwrap().to_owned(),
    );
    let rep = e
        .apply_sync(
            &json!({"sync_token": "t2", "sync_status": {
                u1: {"error_tag": "INVALID_ARGUMENT_VALUE", "error_code": 20, "error": "Invalid argument value", "http_code": 400},
                u2.clone(): {"error_tag": "TOO_MANY_REQUESTS", "error": "Too many requests", "http_code": 429, "error_extra": {"retry_after": 30}}
            }}),
            NOW,
        )
        .unwrap();
    assert_eq!((rep.rejected, rep.retried), (1, 1));
    assert!(!e.state().tasks.values().any(|t| t.content == "Bad one"));
    assert_eq!(e.state().conflicts[0].kind, "rejected");
    assert_eq!(e.sync_token(), None, "a rejection forces a full sync");
    assert!(e.outbox_batch(NOW).is_empty(), "the 429 waits");
    assert_eq!(e.outbox_batch(NOW + 31_000)[0]["uuid"], json!(u2));
}

#[test]
fn soft_delete_undo_window_and_restore() {
    let mut e = synced();
    cmd(&mut e, "d1", json!({"type": "delete", "task_id": "a4"}));
    let v = view(&mut e, json!({"kind": "inbox"}));
    assert_eq!(v.list.count, 3);
    assert_eq!(v.undo, vec!["a4".to_string()]);
    assert!(e.outbox_batch(NOW).is_empty(), "inside the undo window");
    cmd(
        &mut e,
        "d2",
        json!({"type": "undo_delete", "task_id": "a4"}),
    );
    assert!(e.state().outbox.is_empty());
    assert_eq!(view(&mut e, json!({"kind": "inbox"})).list.count, 4);
    // delete for real: pushed after the window
    cmd(&mut e, "d3", json!({"type": "delete", "task_id": "a4"}));
    let batch = e.outbox_batch(NOW + UNDO_MS + 1);
    assert_eq!(batch[0]["type"], "item_delete");
    let uuid = batch[0]["uuid"].as_str().unwrap().to_owned();
    e.apply_sync(
        &json!({"sync_token": "t2", "sync_status": {uuid: "ok"}}),
        NOW + UNDO_MS + 2,
    )
    .unwrap();
    assert!(!e.state().tasks.contains_key("a4"));
    assert_eq!(e.state().trash.len(), 1);
    // undo after it reached Todoist: restored as a new task
    let r = cmd(
        &mut e,
        "d4",
        json!({"type": "undo_delete", "task_id": "a4"}),
    );
    assert!(r.task_id.unwrap().starts_with("tmp-"));
    assert_eq!(e.outbox_batch(NOW)[0]["args"]["content"], "Brain Training");
}

#[test]
fn reschedule_keeps_time_and_recurrence() {
    let mut e = synced();
    cmd(&mut e, "r1", json!({"type": "reschedule_overdue"}));
    let batch = e.outbox_batch(NOW);
    assert_eq!(batch.len(), 1);
    assert_eq!(
        batch[0]["args"]["due"],
        json!({"date": "2026-10-22", "string": "ogni giorno", "lang": "it", "is_recurring": true})
    );
    assert_eq!(view(&mut e, json!({"kind": "today"})).counts.overdue, 0);
    cmd(
        &mut e,
        "r2",
        json!({"type": "reschedule", "task_id": "a3", "date": "next week"}),
    );
    assert_eq!(
        e.state().tasks["a3"].due.as_ref().unwrap().date,
        "2026-10-26"
    );
    assert_eq!(
        e.outbox_batch(NOW)[1]["args"]["due"],
        json!({"date": "2026-10-26"})
    );
    // Todoist dropped the recurrence: a note says so
    let uuid = batch[0]["uuid"].as_str().unwrap().to_owned();
    e.apply_sync(
        &json!({"sync_token": "t2", "sync_status": {uuid: "ok"},
                "items": [item("a2", "10 min anki/memrise", json!({"date": "2026-10-22", "string": "22 Oct", "lang": "it", "is_recurring": false}), "2026-10-22T08:05:00Z")]}),
        NOW,
    )
    .unwrap();
    assert_eq!(e.state().conflicts.last().unwrap().kind, "recurrence_lost");
}

#[test]
fn remote_completion_deletion_and_full_resync() {
    let mut e = synced();
    let mut done = item("a3", "20min speaking", Value::Null, "2026-10-22T08:00:00Z");
    done["checked"] = json!(true);
    done["completed_at"] = json!("2026-10-22T07:30:00Z");
    let mut gone = item("a4", "Brain Training", Value::Null, "2026-10-22T08:00:00Z");
    gone["is_deleted"] = json!(true);
    e.apply_sync(&json!({"sync_token": "t2", "items": [done, gone]}), NOW)
        .unwrap();
    assert!(!e.state().tasks.contains_key("a3") && !e.state().tasks.contains_key("a4"));
    assert_eq!(e.state().completed.last().unwrap().source, "todoist");
    // a later full sync without a2: it is gone; the local-only task stays
    cmd(
        &mut e,
        "x",
        json!({"type": "add", "content": "local only", "due_string": "today"}),
    );
    let mut fs = full_sync();
    fs["items"] = json!([fs["items"][0].clone()]);
    e.apply_sync(&fs, NOW).unwrap();
    let names: Vec<String> = e
        .state()
        .tasks
        .values()
        .map(|t| t.content.clone())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"local only".to_string()));
    // completed history from the REST endpoint merges without duplicates
    let n = e.state().completed.len();
    e.apply_completed(&[json!({"id": "a3", "content": "20min speaking", "project_id": "inbox1", "completed_at": "2026-10-22T07:30:00Z"})], NOW).unwrap();
    assert_eq!(e.state().completed.len(), n);
}

#[test]
fn protocol_receipts_conflicts_and_validation() {
    let mut e = synced();
    let rev = e.state().revision;
    let add = json!({"op_id": "same", "expected_revision": rev, "timezone": TZ, "command": {"type": "add", "content": "x"}});
    let r = e.handle_at(&add.to_string(), NOW);
    assert!(r.ok && !r.replayed);
    let r = e.handle_at(&add.to_string(), NOW);
    assert!(r.ok && r.replayed);
    assert_eq!(e.state().outbox.len(), 1);
    let other =
        json!({"op_id": "same", "timezone": TZ, "command": {"type": "add", "content": "y"}});
    assert_eq!(
        e.handle_at(&other.to_string(), NOW).code.as_deref(),
        Some("op_id_reused")
    );
    let stale = json!({"op_id": "n2", "expected_revision": rev, "timezone": TZ, "command": {"type": "add", "content": "y"}});
    assert_eq!(
        e.handle_at(&stale.to_string(), NOW).code.as_deref(),
        Some("conflict")
    );
    for bad in [
        json!({"type": "add", "content": "  "}),
        json!({"type": "add", "content": "x", "priority": 7}),
        json!({"type": "add", "content": "x", "project_id": "nope"}),
        json!({"type": "complete", "task_id": "nope"}),
        json!({"type": "move", "task_id": "a1", "project_id": "lang", "section_id": "sec1"}),
        json!({"type": "update", "task_id": "a1"}),
        json!({"type": "reschedule", "task_id": "a1", "date": "banana"}),
    ] {
        let r = e.handle_at(
            &json!({"op_id": format!("bad-{bad}"), "timezone": TZ, "command": bad}).to_string(),
            NOW,
        );
        assert_eq!(r.code.as_deref(), Some("invalid_command"), "{bad}");
    }
    assert_eq!(
        e.handle_at(
            &json!({"timezone": "Mars/Base", "command": {"type": "snapshot"}}).to_string(),
            NOW
        )
        .code
        .as_deref(),
        Some("invalid_command")
    );
    let r = e.handle_at(
        &json!({"timezone": TZ, "command": {"type": "parse_quick_add", "text": "x ogni lunedì"}})
            .to_string(),
        NOW,
    );
    assert!(r.parsed.unwrap().is_recurring);
}

#[test]
fn move_and_subtasks() {
    let mut e = synced();
    let r = cmd(
        &mut e,
        "s1",
        json!({"type": "add", "content": "sub", "parent_id": "a4"}),
    );
    let sub = r.task_id.unwrap();
    let inbox = view(&mut e, json!({"kind": "inbox"}));
    let parent = inbox.list.groups[0]
        .tasks
        .iter()
        .find(|t| t.id == "a4")
        .unwrap();
    assert_eq!(parent.children[0].id, sub);
    assert_eq!(parent.subtasks, 1);
    cmd(
        &mut e,
        "m1",
        json!({"type": "move", "task_id": "a4", "section_id": "sec1"}),
    );
    assert_eq!(e.state().tasks[&sub].project_id, "lang");
    let p = view(&mut e, json!({"kind": "project", "id": "lang"}));
    assert_eq!(p.list.groups[1].key, "sec1");
    assert_eq!(p.list.groups[1].tasks[0].children.len(), 1);
    let r = e.handle_at(
        &json!({"op_id": "m2", "timezone": TZ, "command": {"type": "move", "task_id": "a4", "parent_id": sub}}).to_string(),
        NOW,
    );
    assert_eq!(r.code.as_deref(), Some("invalid_command"));
}

#[test]
fn persistence_is_atomic_and_private() {
    let dir = std::env::temp_dir().join(format!("tasks-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("tasks.json");
    {
        let mut e = Engine::open(&path).unwrap();
        assert!(Engine::open(&path).is_err(), "locked");
        cmd(&mut e, "p1", json!({"type": "add", "content": "persisted"}));
    }
    let e = Engine::open(&path).unwrap();
    assert_eq!(e.state().tasks.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn disconnect_keeps_local_tasks_only() {
    let mut e = synced();
    cmd(
        &mut e,
        "l",
        json!({"type": "add", "content": "mine", "project_id": "lang"}),
    );
    e.disconnect().unwrap();
    let st = e.state();
    assert_eq!(st.tasks.len(), 1);
    assert!(st.projects.is_empty());
    assert_eq!(st.sync.status, SyncStatus::Disconnected);
    assert!(st.outbox[0].args.get("project_id").is_none());
}
