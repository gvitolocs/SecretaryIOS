//! `PgStore` against a throwaway Postgres (never the live database).
//!
//! Needs `LUCINA_TEST_DATABASE_URL` (e.g. a `postgres:17` container on
//! 127.0.0.1:55481 with database `lucina_test`); without it every test is
//! skipped. The schema is dropped and recreated, so the URL must contain
//! `lucina_test`.

use chrono::{DateTime, Duration, Utc};
use lucina_proactive::model::{
    new_outreach_id, Candidate, Decision, Event, Feedback, FeedbackAction, Kind, Outreach,
    Priority, Scores,
};
use lucina_proactive::store::{PgStore, Store};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::Mutex;
use uuid::Uuid;

static LOCK: Mutex<()> = Mutex::const_new(());

async fn store() -> Option<PgStore> {
    let url = std::env::var("LUCINA_TEST_DATABASE_URL").ok()?;
    assert!(url.contains("lucina_test"), "refusing a non-test database");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS lucina_proactive CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    Some(PgStore::with_pool(pool))
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn candidate(e: &Event, decision: Decision, now: DateTime<Utc>) -> Candidate {
    Candidate {
        id: Uuid::new_v4(),
        event_id: e.id,
        kind: e.kind,
        scores: e.scores,
        score: 0.75,
        decision,
        reason: "r".into(),
        priority: Priority::Normal,
        created_at: now,
        decided_at: now,
        hold_until: None,
        expires_at: e.expires_at,
        pulse_id: None,
    }
}

fn outreach(kind: Kind, now: DateTime<Utc>, c: Option<Uuid>, unsolicited: bool) -> Outreach {
    Outreach {
        id: new_outreach_id(now),
        candidate_id: c,
        kind,
        channel: "app".into(),
        message: "ciao".into(),
        priority: Priority::Normal,
        actions: lucina_proactive::lang::feedback_actions("it"),
        why: "w".into(),
        language: "it".into(),
        dry_run: true,
        status: "dry_run".into(),
        result: json!({"x": 1}),
        sent_at: now,
        timeline_id: None,
        unsolicited,
    }
}

#[tokio::test]
async fn migrations_store_roundtrip() {
    let _g = LOCK.lock().await;
    let Some(s) = store().await else { return };
    assert_eq!(s.migrate().await.unwrap(), vec![1]);
    assert!(s.migrate().await.unwrap().is_empty(), "idempotent");
    // MVP 3/5 tables exist and are empty.
    for t in ["tasks", "approvals", "automation_suggestions"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM lucina_proactive.{t}"))
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    let now = at("2026-10-22T08:00:00Z");
    let e = Event::new(
        "calendar",
        Kind::MeetingPrep,
        "meeting_prep:g:1",
        now,
        "CRAFT",
    )
    .expires(now + Duration::hours(1))
    .scores(Scores::new(0.8, 0.8, 0.95, 0.9))
    .signal("starts in 60 min")
    .payload(json!({"event": {"title": "CRAFT"}}));
    assert!(s.insert_event(&e).await.unwrap());
    let mut again = e.clone();
    again.id = Uuid::new_v4();
    assert!(!s.insert_event(&again).await.unwrap(), "dedup key");

    let mut c = candidate(&e, Decision::Hold, now);
    c.hold_until = Some(now + Duration::minutes(5));
    s.save_candidate(&c).await.unwrap();
    let held = s.held().await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].0, c);
    assert_eq!(held[0].1.payload, e.payload);
    assert_eq!(held[0].1.signal, "starts in 60 min");
    assert_eq!(
        s.candidate_by_key("meeting_prep:g:1")
            .await
            .unwrap()
            .unwrap()
            .id,
        c.id
    );

    // Upsert: the held one is sent.
    c.decision = Decision::Sent;
    c.reason = "sent".into();
    s.save_candidate(&c).await.unwrap();
    assert!(s.held().await.unwrap().is_empty());

    let o = outreach(Kind::MeetingPrep, now, Some(c.id), true);
    s.record_outreach(&o).await.unwrap();
    assert_eq!(s.outreach(&o.id).await.unwrap().unwrap(), o);
    let pulse = outreach(Kind::Pulse, now + Duration::seconds(1), None, false);
    s.record_outreach(&pulse).await.unwrap();
    assert_eq!(
        s.unsolicited_since(now - Duration::hours(1)).await.unwrap(),
        1
    );
    assert_eq!(
        s.last_outreach_of_kind(Kind::MeetingPrep).await.unwrap(),
        Some(now)
    );
    assert_eq!(
        s.last_outreach_of_kind(Kind::ServiceDown).await.unwrap(),
        None
    );
    assert_eq!(
        s.outreach_between(now, now + Duration::minutes(1))
            .await
            .unwrap()
            .len(),
        2
    );

    // Digests for the Pulse.
    let e2 = Event::new("reminders", Kind::DueNoPlan, "due_no_plan:r:2d", now, "IVA");
    s.insert_event(&e2).await.unwrap();
    let mut d = candidate(&e2, Decision::Digest, now);
    s.save_candidate(&d).await.unwrap();
    assert_eq!(s.digests(now - Duration::hours(1)).await.unwrap().len(), 1);
    d.decision = Decision::Pulsed;
    d.pulse_id = Some(pulse.id.clone());
    s.save_candidate(&d).await.unwrap();
    assert!(s
        .digests(now - Duration::hours(1))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        s.decisions_between(now, now + Duration::minutes(1))
            .await
            .unwrap()
            .len(),
        2
    );

    // Feedback: idempotent by external id; unknown outreach refused.
    let f = Feedback {
        id: Uuid::new_v4(),
        outreach_id: o.id.clone(),
        action: FeedbackAction::NotNow,
        kind: Kind::MeetingPrep,
        source: "app".into(),
        external_id: Some("fb_1".into()),
        created_at: now,
    };
    assert!(s.record_feedback(&f).await.unwrap());
    assert!(!s
        .record_feedback(&Feedback {
            id: Uuid::new_v4(),
            ..f.clone()
        })
        .await
        .unwrap());
    assert!(!s
        .record_feedback(&Feedback {
            id: Uuid::new_v4(),
            outreach_id: "po_unknown".into(),
            external_id: Some("fb_2".into()),
            ..f.clone()
        })
        .await
        .unwrap());
    assert_eq!(
        s.last_feedback(Kind::MeetingPrep, FeedbackAction::NotNow)
            .await
            .unwrap(),
        Some(now)
    );
    assert_eq!(
        s.last_feedback(Kind::MeetingPrep, FeedbackAction::Stop)
            .await
            .unwrap(),
        None
    );

    // Source state.
    assert_eq!(s.get_state("health", "Secretary").await.unwrap(), None);
    s.put_state("health", "Secretary", &json!({"fails": 1}), now)
        .await
        .unwrap();
    s.put_state("health", "Secretary", &json!({"fails": 2}), now)
        .await
        .unwrap();
    assert_eq!(
        s.get_state("health", "Secretary").await.unwrap(),
        Some(json!({"fails": 2}))
    );
}

/// G3: the tick lock is one per database, across connections (processes):
/// the `run` daemon and a `once` run cannot tick at the same time.
#[tokio::test]
async fn the_tick_lock_is_exclusive_across_connections() {
    let _g = LOCK.lock().await;
    let Some(daemon) = store().await else { return };
    let url = std::env::var("LUCINA_TEST_DATABASE_URL").unwrap();
    let once = PgStore::with_pool(
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap(),
    );
    let held = daemon
        .try_tick_lock()
        .await
        .unwrap()
        .expect("free at first");
    assert!(
        once.try_tick_lock().await.unwrap().is_none(),
        "held by the daemon"
    );
    assert!(
        daemon.try_tick_lock().await.unwrap().is_none(),
        "not re-entrant"
    );
    drop(held);
    // The release reaches the server when the dropped transaction's
    // connection rolls back.
    let mut got = None;
    for _ in 0..50 {
        got = once.try_tick_lock().await.unwrap();
        if got.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(got.is_some(), "released after the tick");
}
