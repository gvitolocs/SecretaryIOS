//! The presence engine and stores, driven by raw gateway frames.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use lucina_discord::engine::{Engine, Outbound};
use lucina_discord::store::{
    self, online_seconds, read_history, HistoryEvent, PresenceReport, PresenceStore,
};
use serde_json::{json, Value};

const OWNER: &str = "649020113026023475";
const OTHER: &str = "111111111111111111";
const G1: &str = "1343229782321659937";
const G2: &str = "1347233334060908555";
const G3: &str = "1508070671555624990";
const SECRET: &str = "OTHER-USER-SECRET-ACTIVITY";

fn t(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 22, h, m, 0).unwrap()
}

fn rome() -> Tz {
    "Europe/Rome".parse().unwrap()
}

fn engine(dir: &Path, now: DateTime<Utc>) -> Engine {
    Engine::start(
        OWNER,
        PresenceStore::new(dir.join("presence"), 90),
        now,
        Duration::from_secs(600),
        Duration::from_secs(60),
    )
    .unwrap()
}

fn frame(t: &str, d: Value) -> String {
    json!({"op": 0, "t": t, "s": 1, "d": d}).to_string()
}

fn presence(user: &str, guild: &str, status: &str, activities: Value) -> Value {
    json!({"user": {"id": user}, "guild_id": guild, "status": status,
           "client_status": {"desktop": status}, "activities": activities})
}

fn other_presence(guild: &str) -> Value {
    json!({"user": {"id": OTHER, "username": "someone"}, "guild_id": guild, "status": "online",
           "client_status": {"mobile": "online"},
           "activities": [{"type": 0, "name": SECRET, "details": SECRET}]})
}

fn spotify(song: &str) -> Value {
    json!({"type": 2, "name": "Spotify", "details": song, "state": "Artist A; Artist B",
           "timestamps": {"start": 1_792_696_200_000i64}})
}

fn game() -> Value {
    json!({"type": 0, "name": "Elden Ring", "timestamps": {"start": 1_792_696_000_000i64}})
}

fn all_files(dir: &Path) -> String {
    let mut s = String::new();
    for e in std::fs::read_dir(dir.join("presence")).unwrap() {
        s.push_str(&std::fs::read_to_string(e.unwrap().path()).unwrap());
    }
    s
}

fn current(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("presence/discord.json")).unwrap()).unwrap()
}

fn kinds(dir: &Path) -> Vec<String> {
    read_history(&dir.join("presence"))
        .iter()
        .map(|e| match e {
            HistoryEvent::Status { from, to, .. } => {
                format!("status:{}->{to}", from.as_deref().unwrap_or("-"))
            }
            HistoryEvent::ActivityStart { activity, .. } => format!("start:{}", activity.name),
            HistoryEvent::ActivityStop { activity, .. } => format!("stop:{}", activity.name),
            HistoryEvent::Watcher { state, .. } => format!("watcher:{state}"),
        })
        .collect()
}

#[test]
fn startup_from_guild_create_requests_owner_and_filters_others() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = engine(dir.path(), t(19, 0));
    let out = e.handle_text(
        &frame(
            "GUILD_CREATE",
            json!({"id": G1, "name": "Server", "members": [{"user": {"id": OTHER}}],
                   "presences": [other_presence(G1), presence(OWNER, G1, "online", json!([game()]))]}),
        ),
        t(19, 0),
    );
    assert_eq!(
        out,
        vec![Outbound::RequestOwner {
            guild_id: G1.parse().unwrap()
        }]
    );
    let c = current(dir.path());
    assert_eq!(c["status"], "online");
    assert_eq!(
        c["clientStatus"],
        json!({"desktop": "online", "mobile": null, "web": null})
    );
    assert_eq!(c["activities"][0]["type"], "playing");
    assert_eq!(c["activities"][0]["name"], "Elden Ring");
    // only owner-only member requests, and a PRESENCE_UPDATE for someone else changes nothing
    e.handle_text(&frame("PRESENCE_UPDATE", other_presence(G2)), t(19, 1));
    e.handle_text(
        &frame(
            "GUILD_MEMBERS_CHUNK",
            json!({"guild_id": G2, "chunk_index": 0, "chunk_count": 1,
                   "members": [{"user": {"id": OTHER}}], "presences": [other_presence(G2)], "not_found": []}),
        ),
        t(19, 1),
    );
    let files = all_files(dir.path());
    assert!(!files.contains(SECRET), "{files}");
    assert!(!files.contains(OTHER), "{files}");
    assert_eq!(current(dir.path())["status"], "online");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in ["discord.json", "discord.jsonl"] {
            let mode = std::fs::metadata(dir.path().join("presence").join(f))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{f}");
        }
    }
}

#[test]
fn merge_across_guilds_takes_the_freshest_and_logs_transitions_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = engine(dir.path(), t(18, 0));
    // the same update from three guilds: one status line, one activity start
    for g in [G1, G2, G3] {
        e.handle_text(
            &frame(
                "PRESENCE_UPDATE",
                presence(OWNER, g, "online", json!([spotify("Song 1")])),
            ),
            t(18, 0),
        );
    }
    assert_eq!(
        kinds(dir.path()),
        ["watcher:start", "status:-->online", "start:Spotify"]
    );
    // song change: state updated, no history line
    e.handle_text(
        &frame(
            "PRESENCE_UPDATE",
            presence(OWNER, G2, "online", json!([spotify("Song 2")])),
        ),
        t(18, 3),
    );
    let c = current(dir.path());
    assert_eq!(c["activities"][0]["details"], "Song 2");
    assert_eq!(c["activities"][0]["state"], "Artist A, Artist B");
    assert_eq!(c["activities"][0]["type"], "listening");
    assert_eq!(kinds(dir.path()).len(), 3);
    // a stale guild still saying "online + Song 1" does not win over a fresher one
    e.handle_text(
        &frame(
            "PRESENCE_UPDATE",
            presence(OWNER, G3, "idle", json!([game()])),
        ),
        t(18, 10),
    );
    let c = current(dir.path());
    assert_eq!(c["status"], "idle");
    assert_eq!(c["since"], "2026-10-22T18:10:00.000Z");
    assert_eq!(c["activities"].as_array().unwrap().len(), 1);
    assert_eq!(
        kinds(dir.path())[3..],
        ["status:online->idle", "start:Elden Ring", "stop:Spotify"]
    );
    // the other guilds echo the same: nothing new
    for g in [G1, G2] {
        e.handle_text(
            &frame(
                "PRESENCE_UPDATE",
                presence(OWNER, g, "idle", json!([game()])),
            ),
            t(18, 10),
        );
    }
    assert_eq!(kinds(dir.path()).len(), 6);
    // custom status: a new text is a new activity
    let custom = |s: &str| json!({"type": 4, "name": "Custom Status", "state": s});
    e.handle_text(
        &frame(
            "PRESENCE_UPDATE",
            presence(OWNER, G1, "idle", json!([game(), custom("studying")])),
        ),
        t(18, 20),
    );
    e.handle_text(
        &frame(
            "PRESENCE_UPDATE",
            presence(OWNER, G1, "idle", json!([game(), custom("eating")])),
        ),
        t(18, 25),
    );
    assert_eq!(
        kinds(dir.path())[6..],
        [
            "start:Custom Status",
            "start:Custom Status",
            "stop:Custom Status"
        ]
    );
    // offline in the freshest guild: everything stops
    e.handle_text(
        &frame("PRESENCE_UPDATE", presence(OWNER, G2, "offline", json!([]))),
        t(19, 0),
    );
    let c = current(dir.path());
    assert_eq!(c["status"], "offline");
    assert_eq!(
        c["clientStatus"],
        json!({"desktop": null, "mobile": null, "web": null})
    );
    assert!(kinds(dir.path()).ends_with(&[
        "status:idle->offline".to_string(),
        "stop:Elden Ring".to_string(),
        "stop:Custom Status".to_string()
    ]));
}

#[test]
fn members_chunk_and_timeout_mean_offline() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = engine(dir.path(), t(8, 0));
    e.handle_text(
        &frame(
            "GUILD_CREATE",
            json!({"id": G1, "presences": [other_presence(G1)]}),
        ),
        t(8, 0),
    );
    assert!(e.state().is_none());
    e.handle_text(
        &frame(
            "GUILD_MEMBERS_CHUNK",
            json!({"guild_id": G1, "members": [{"user": {"id": OWNER}, "roles": []}], "presences": []}),
        ),
        t(8, 0),
    );
    assert_eq!(current(dir.path())["status"], "offline");

    let dir2 = tempfile::tempdir().unwrap();
    let mut e = engine(dir2.path(), t(8, 0));
    e.tick(t(8, 0));
    assert!(e.state().is_none());
    e.tick(t(8, 2));
    assert_eq!(current(dir2.path())["status"], "offline");
    // a later real observation replaces the timeout guess
    e.handle_text(
        &frame("PRESENCE_UPDATE", presence(OWNER, G1, "dnd", json!([]))),
        t(8, 3),
    );
    assert_eq!(current(dir2.path())["status"], "dnd");
    assert_eq!(e.merged().unwrap().status, "dnd");
}

#[test]
fn restart_keeps_since_and_closes_the_previous_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = engine(dir.path(), t(10, 0));
    e.handle_text(
        &frame(
            "PRESENCE_UPDATE",
            presence(OWNER, G1, "online", json!([game()])),
        ),
        t(10, 0),
    );
    e.tick(t(10, 5)); // updatedAt 10:05, then a crash (no stop line)
    drop(e);
    let mut e = engine(dir.path(), t(10, 7));
    e.handle_text(
        &frame(
            "PRESENCE_UPDATE",
            presence(OWNER, G1, "online", json!([game()])),
        ),
        t(10, 7),
    );
    let c = current(dir.path());
    assert_eq!(c["since"], "2026-10-22T10:00:00.000Z");
    // the game is not re-logged; the status is (the history needs it after a start)
    assert_eq!(
        kinds(dir.path()),
        [
            "watcher:start",
            "status:-->online",
            "start:Elden Ring",
            "watcher:stop",
            "watcher:start",
            "status:online->online"
        ]
    );
    let hist = read_history(&dir.path().join("presence"));
    assert!(
        matches!(&hist[3], HistoryEvent::Watcher { at, inferred: true, .. } if at == "2026-10-22T10:05:00.000Z")
    );
    // clean stop: no inferred line on the next start
    e.stop(t(10, 30));
    drop(e);
    let _e = engine(dir.path(), t(12, 0));
    let k = kinds(dir.path());
    assert_eq!(k[k.len() - 2..], ["watcher:stop", "watcher:start"]);
}

#[test]
fn retention_prunes_old_lines() {
    let dir = tempfile::tempdir().unwrap();
    let pdir = dir.path().join("presence");
    std::fs::create_dir_all(&pdir).unwrap();
    let old = json!({"at": "2026-06-01T10:00:00.000Z", "event": "watcher", "state": "start"});
    let new = json!({"at": "2026-10-20T10:00:00.000Z", "event": "watcher", "state": "start"});
    std::fs::write(
        pdir.join("discord.jsonl"),
        format!("{old}\n{new}\nnot json\n"),
    )
    .unwrap();
    let store = PresenceStore::new(&pdir, 90);
    assert_eq!(store.prune(t(12, 0)).unwrap(), 2);
    let text = std::fs::read_to_string(pdir.join("discord.jsonl")).unwrap();
    assert_eq!(text, format!("{new}\n"));
    // retention from the env value: 1 day drops the 2-day-old line too
    assert_eq!(PresenceStore::new(&pdir, 1).prune(t(12, 0)).unwrap(), 1);
    // the size cap keeps the newest half
    let mut s = PresenceStore::new(&pdir, 90);
    s.max_history_bytes = 200;
    let lines: Vec<String> = (0..10)
        .map(|i| json!({"at": format!("2026-10-22T10:0{i}:00.000Z"), "event": "watcher", "state": "start"}).to_string())
        .collect();
    std::fs::write(pdir.join("discord.jsonl"), lines.join("\n") + "\n").unwrap();
    assert_eq!(s.prune(t(12, 0)).unwrap(), 5);
    let kept = std::fs::read_to_string(pdir.join("discord.jsonl")).unwrap();
    assert!(kept.starts_with(&lines[5]));
}

#[test]
fn online_minutes_today_and_summary() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = engine(dir.path(), t(5, 0)); // 07:00 Rome
    let p = |s: &str, a: Value| frame("PRESENCE_UPDATE", presence(OWNER, G1, s, a));
    e.handle_text(&p("online", json!([])), t(6, 0));
    e.handle_text(&p("idle", json!([])), t(6, 30));
    e.handle_text(&p("offline", json!([])), t(7, 0));
    e.handle_text(&p("dnd", json!([game(), spotify("Song 1")])), t(17, 0));
    e.tick(t(17, 45));
    let pdir = dir.path().join("presence");
    let events = read_history(&pdir);
    let from = store::day_start(t(17, 45), rome());
    assert_eq!(from, Utc.with_ymd_and_hms(2026, 10, 21, 22, 0, 0).unwrap());
    let secs = online_seconds(&events, from, t(17, 45), t(17, 45));
    assert_eq!(
        secs,
        vec![
            ("online".to_string(), 1800),
            ("idle".to_string(), 1800),
            ("dnd".to_string(), 2700)
        ]
    );
    let r = PresenceReport::build(&pdir, rome(), t(17, 45), Duration::from_secs(600));
    assert!(r.available && r.fresh);
    assert_eq!(r.today_online_minutes, 105);
    assert_eq!(r.today_by_status["dnd"], 45);
    assert!(
        r.summary.starts_with(
            "Discord: do not disturb on desktop since 19:00; playing Elden Ring since"
        ),
        "{}",
        r.summary
    );
    assert!(r
        .summary
        .contains("listening to \"Song 1\" by Artist A, Artist B on Spotify"));
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["status"], "dnd");
    assert_eq!(v["todayOnlineMinutes"], 105);
    // stale: the open interval ends at updatedAt, the summary says unknown
    let r = PresenceReport::build(&pdir, rome(), t(19, 0), Duration::from_secs(600));
    assert!(!r.fresh);
    assert_eq!(r.today_online_minutes, 105);
    assert!(r.summary.contains("presence unknown"));
    assert!(store::context_line(&pdir, rome(), t(19, 0), Duration::from_secs(600)).is_none());
    assert!(store::context_line(&pdir, rome(), t(17, 46), Duration::from_secs(600)).is_some());
    // no watcher at all
    let none = PresenceReport::build(
        &dir.path().join("nothing"),
        rome(),
        t(9, 0),
        Duration::from_secs(600),
    );
    assert!(!none.available);
    assert_eq!(
        none.summary,
        "Discord: presence unknown (watcher not running)"
    );
}
