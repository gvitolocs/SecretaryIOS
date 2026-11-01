//! The ingest end to end against a fake gateway (wiremock) and a local vault
//! directory standing in for the Pi: queueing and dedup, idempotent notes,
//! amending, redaction, the availability gate and its backoff, drafts kept
//! while the vault is offline, compare-and-swap conflicts, and the ops and
//! git sources.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use lucina_ops::{Ops, OpsConfig};
use lucina_vault_ingest::queue::{Backlink, Item, SourceKind, State, MAX_ATTEMPTS};
use lucina_vault_ingest::store::{LocalStore, StoreError, VaultStore};
use lucina_vault_ingest::{Ingest, IngestConfig, RunOpts};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The test vault: a local directory that can be switched offline.
struct Switchable {
    inner: LocalStore,
    offline: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl VaultStore for Switchable {
    async fn read(&self, rel: &str) -> Result<Option<String>, StoreError> {
        self.up()?;
        self.inner.read(rel).await
    }
    async fn list(&self, dir: &str) -> Result<Vec<String>, StoreError> {
        self.up()?;
        self.inner.list(dir).await
    }
    async fn write(
        &self,
        rel: &str,
        text: &str,
        expected: Option<&str>,
    ) -> Result<bool, StoreError> {
        self.up()?;
        self.inner.write(rel, text, expected).await
    }
    fn describe(&self) -> String {
        "test".into()
    }
}

impl Switchable {
    fn up(&self) -> Result<(), StoreError> {
        if self.offline.load(Ordering::SeqCst) {
            Err(StoreError::Offline("pi-cursor: No route to host".into()))
        } else {
            Ok(())
        }
    }
}

struct Env {
    _dir: tempfile::TempDir,
    vault: PathBuf,
    ingest: Ingest,
    ops: Arc<Ops>,
    offline: Arc<AtomicBool>,
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn env_with(url: &str, git: Vec<lucina_vault_ingest::sources::GitRepo>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let ops_vault = dir.path().join("ops-vault");
    let vault = dir.path().join("Obsidian");
    std::fs::create_dir_all(&ops_vault).unwrap();
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("00 Home.md"),
        "# Obsidian vault hub\n\n## Projects\n\n| Vault | Hub |\n| --- | --- |\n| Pokoin | [[00 Pokoin hub]] |\n\n## Lectures\n\n- x\n",
    )
    .unwrap();
    let ops = Arc::new(
        Ops::open(
            OpsConfig::new(&ops_vault, dir.path().join("ops"), chrono_tz::Europe::Rome),
            None,
        )
        .unwrap(),
    );
    let mut cfg = IngestConfig::new(dir.path().join("ops/ingest"), chrono_tz::Europe::Rome);
    cfg.qwen_url = url.to_owned();
    cfg.key_file = None;
    cfg.qcode_lock = None;
    cfg.last_used = None;
    cfg.git = git;
    let offline = Arc::new(AtomicBool::new(false));
    let store = Switchable {
        inner: LocalStore::open(&vault).unwrap(),
        offline: offline.clone(),
    };
    let ingest = Ingest::open_with(cfg, ops.clone(), Box::new(store)).unwrap();
    Env {
        _dir: dir,
        vault,
        ingest,
        ops,
        offline,
    }
}

fn env(url: &str) -> Env {
    env_with(url, Vec::new())
}

fn item(key: &str, title: &str) -> Item {
    Item {
        key: key.into(),
        source: SourceKind::Session,
        project: "lucina".into(),
        title: title.into(),
        at: DateTime::parse_from_rfc3339("2026-11-01T11:00:00+01:00").unwrap(),
        text: "The session moved the ops store before the vault writes.".into(),
        files: vec!["/home/nez/Projects/Lucina/SecretaryIOS/crates/lucina-ops/src/lib.rs".into()],
        backlink: Backlink::Session { id: key.into() },
    }
}

fn ready() -> Value {
    json!({"state": "QWEN_READY", "switching": null, "lease_waiters": [],
           "gpu_owner": [{"name": "llama-server", "state": "held"}],
           "active_requests": {"qwen": 0}, "queued_requests": 0, "pending_strata": 0,
           "pending_dense": 0, "model_loaded": {"qwen": true}, "vram_holders": []})
}

fn idle() -> Value {
    json!({"state": "IDLE", "switching": null, "lease_waiters": [], "gpu_owner": [],
           "active_requests": {"qwen": 0}, "queued_requests": 0, "pending_strata": 0,
           "pending_dense": 0, "model_loaded": {"qwen": false}, "vram_holders": []})
}

fn busy() -> Value {
    let mut s = ready();
    s["state"] = json!("QWEN_BUSY");
    s["active_requests"] = json!({"qwen": 1});
    s
}

fn answer(plan: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"role": "assistant", "content": plan.to_string()}}]
    }))
}

fn store_first_plan() -> Value {
    json!({
        "skip": false,
        "event": {"summary": "The ops record now writes its store before the vault pages."},
        "claims": [{
            "title": "lucina-ops writes the store before the vault pages",
            "body": "A record is appended to records.jsonl first. The vault pages follow, so a crash never leaves a page without a record.",
            "files": ["/home/nez/Projects/Lucina/SecretaryIOS/crates/lucina-ops/src/lib.rs", "/home/nez/invented/file.rs"],
            "connects_to": ["A link to nothing"]
        }]
    })
}

async fn gateway(status: Value) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gpu/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status))
        .mount(&s)
        .await;
    s
}

const CLAIM: &str = "LucinaRS/Permanent/lucina-ops writes the store before the vault pages.md";

fn read(v: &Path, rel: &str) -> String {
    std::fs::read_to_string(v.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn list(v: &Path, dir: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(v.join(dir))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn snapshot(v: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for d in [
        "LucinaRS/Permanent",
        "LucinaRS/Events",
        "LucinaRS/MOC",
        "LucinaRS",
    ] {
        for f in list(v, d) {
            if f.ends_with(".md") {
                out.push((format!("{d}/{f}"), read(v, &format!("{d}/{f}"))));
            }
        }
    }
    out.push(("home".into(), read(v, "00 Home.md")));
    out
}

#[tokio::test]
async fn queueing_is_durable_and_deduplicated() {
    let e = env("http://127.0.0.1:9");
    let q = e.ingest.queue();
    assert_eq!(
        q.enqueue(vec![item("session:a", "A"), item("session:a", "A")], now())
            .unwrap(),
        1
    );
    assert_eq!(
        q.enqueue(vec![item("session:a", "A"), item("session:b", "B")], now())
            .unwrap(),
        1
    );
    // A restart reads the same journal.
    let again = lucina_vault_ingest::queue::Queue::open(q.dir()).unwrap();
    assert_eq!(
        again.enqueue(vec![item("session:b", "B")], now()).unwrap(),
        0
    );
    let order: Vec<String> = again
        .ordered()
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(order, ["session:a", "session:b"]);
    // A torn last line (crash mid-append) is skipped.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(q.dir().join("queue.jsonl"))
        .unwrap();
    f.write_all(b"{\"op\":\"enqu").unwrap();
    assert_eq!(again.slots().unwrap().len(), 2);
    assert_eq!(e.ingest.status().unwrap().queue.pending, 2);
}

#[tokio::test]
async fn an_item_becomes_notes_once() {
    let gw = gateway(ready()).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(store_first_plan()))
        .expect(1)
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    e.ingest
        .queue()
        .enqueue(vec![item("session:a", "Ops store first")], now())
        .unwrap();
    let r = e.ingest.run(RunOpts::default(), now()).await.unwrap();
    assert_eq!(
        (r.outcome.as_str(), r.planned, r.ingested, r.failed),
        ("ran", 1, 1, 0),
        "{r:?}"
    );

    let claim = read(&e.vault, CLAIM);
    assert!(claim.starts_with("---\ntype: permanent\n"), "{claim}");
    assert!(claim.contains("# lucina-ops writes the store before the vault pages"));
    assert!(
        claim.contains("- `/home/nez/Projects/Lucina/SecretaryIOS/crates/lucina-ops/src/lib.rs`")
    );
    assert!(!claim.contains("invented"), "invented path dropped");
    assert!(
        !claim.contains("A link to nothing"),
        "dangling link dropped"
    );
    assert!(claim.contains("- [[00 LucinaRS hub]] · [[MOC LucinaRS change log]]"));
    assert!(claim.contains("[[2026-11-01 Ops store first]]"));
    assert!(claim.contains("<!-- ingest:session:a -->"));
    for s in [
        "## Project files",
        "## Connects to",
        "## Enables",
        "## Index",
        "## Part of",
        "## Sources",
    ] {
        assert!(claim.contains(s), "{s}");
    }
    let ev = read(&e.vault, "LucinaRS/Events/2026-11-01 Ops store first.md");
    assert!(ev.contains("ingest_key: session:a") && ev.contains("status: complete"));
    assert!(ev.contains("- [[lucina-ops writes the store before the vault pages]]"));
    assert!(ev.contains("Claude session report `session:a`"));
    let moc = read(&e.vault, "LucinaRS/MOC/MOC LucinaRS change log.md");
    assert!(moc.contains("- [[lucina-ops writes the store before the vault pages]]"));
    let hub = read(&e.vault, "LucinaRS/00 LucinaRS hub.md");
    assert!(
        hub.contains("- [[MOC LucinaRS change log]]")
            && hub.contains("[[2026-11-01 Ops store first]]")
    );
    let home = read(&e.vault, "00 Home.md");
    assert!(
        home.contains(
            "| Pokoin | [[00 Pokoin hub]] |\n| LucinaRS | [[00 LucinaRS hub]] |\n\n## Lectures"
        ),
        "the new hub is a row of the Projects table: {home}"
    );
    assert!(
        list(&e.ingest.staging_dir(), "").is_empty(),
        "delivered drafts leave staging"
    );

    // Replay: nothing is due; writing the same stored plan again (a crash
    // after the notes, before `done`) changes no page and makes no second claim.
    let before = snapshot(&e.vault);
    let r = e
        .ingest
        .run(
            RunOpts {
                force: true,
                ..Default::default()
            },
            now(),
        )
        .await
        .unwrap();
    assert_eq!(r.outcome, "idle");
    let slot = e
        .ingest
        .queue()
        .slots()
        .unwrap()
        .remove("session:a")
        .unwrap();
    let store = LocalStore::open(&e.vault).unwrap();
    let w = lucina_vault_ingest::notes::Writer {
        store: &store,
        templates: &lucina_vault_ingest::templates::Templates::default(),
        today: chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
    };
    w.write(
        "LucinaRS",
        slot.item.as_ref().unwrap(),
        slot.plan.as_ref().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(snapshot(&e.vault), before);
    assert_eq!(list(&e.vault, "LucinaRS/Permanent").len(), 1);
    assert_eq!(list(&e.vault, "LucinaRS/Events").len(), 1);
}

#[tokio::test]
async fn a_known_claim_is_amended_not_duplicated() {
    let gw = gateway(ready()).await;
    let mut plan = store_first_plan();
    plan["claims"][0]["title"] = json!("LUCINA-OPS writes the store before the vault pages!");
    plan["claims"][0]["update"] = json!("It now fsyncs the store too.");
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(plan))
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    let owner = "---\ntype: permanent\nupdated: 2026-10-01\n---\n\n# lucina-ops writes the store before the vault pages\n\nThe owner's own words.\n\n## Sources\n\n- hand-written\n";
    std::fs::create_dir_all(e.vault.join("LucinaRS/Permanent")).unwrap();
    std::fs::write(e.vault.join(CLAIM), owner).unwrap();
    std::fs::write(
        e.vault.join("LucinaRS/00 LucinaRS hub.md"),
        "# 00 LucinaRS hub\n\nThe owner's hub.\n\n## Recent events\n\n- older\n",
    )
    .unwrap();
    e.ingest
        .queue()
        .enqueue(vec![item("session:b", "fsync the store")], now())
        .unwrap();
    let r = e.ingest.run(RunOpts::default(), now()).await.unwrap();
    assert_eq!(r.ingested, 1, "{r:?}");
    assert_eq!(
        list(&e.vault, "LucinaRS/Permanent").len(),
        1,
        "amended, not duplicated"
    );
    let t = read(&e.vault, CLAIM);
    assert!(t.contains("The owner's own words."));
    assert!(t.contains("updated: 2026-11-01"));
    assert!(
        t.contains("- hand-written\n- 2026-11-01 [[2026-11-01 fsync the store]]"),
        "{t}"
    );
    assert!(t.contains("## Updates\n\n- 2026-11-01: It now fsyncs the store too. ([[2026-11-01 fsync the store]]) <!-- ingest:session:b -->"), "{t}");
    let hub = read(&e.vault, "LucinaRS/00 LucinaRS hub.md");
    assert!(
        hub.contains("## Recent events\n\n- [[2026-11-01 fsync the store]] · lucina <!-- ingest:session:b -->\n- older"),
        "an existing hub keeps its own events section: {hub}"
    );
    assert!(
        !read(&e.vault, "00 Home.md").contains("LucinaRS"),
        "an existing hub is not re-linked"
    );
}

#[tokio::test]
async fn secrets_never_reach_a_note() {
    let gw = gateway(ready()).await;
    let plan = json!({
        "event": {"summary": "Flashed with psk=hunter2hunter2 and token ghp_abcdefghijklmnopqrstuvwxyz0123456789 on serial G000AB1234567890."},
        "claims": [{"title": "The Kindle OTA keeps two boot slots", "body": "The release key -----BEGIN PRIVATE KEY-----\nMIIEv\n-----END PRIVATE KEY----- signs it. imei 356938035643809."}]
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(plan))
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    e.ingest
        .queue()
        .enqueue(vec![item("session:s", "OTA")], now())
        .unwrap();
    assert_eq!(
        e.ingest
            .run(RunOpts::default(), now())
            .await
            .unwrap()
            .ingested,
        1
    );
    for (p, text) in snapshot(&e.vault) {
        for s in [
            "hunter2",
            "ghp_",
            "G000AB",
            "BEGIN PRIVATE",
            "356938035643809",
        ] {
            assert!(!text.contains(s), "{s} in {p}:\n{text}");
        }
    }
    let q = std::fs::read_to_string(e.ingest.queue().dir().join("queue.jsonl")).unwrap();
    assert!(
        !q.contains("hunter2") && !q.contains("ghp_"),
        "the stored draft is redacted too"
    );
}

#[tokio::test]
async fn an_offline_vault_keeps_the_drafts_until_it_is_back() {
    let gw = gateway(ready()).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(store_first_plan()))
        .expect(1)
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    e.offline.store(true, Ordering::SeqCst);
    e.ingest
        .queue()
        .enqueue(
            vec![item("session:o", "Drafted while the Pi is off")],
            now(),
        )
        .unwrap();
    let r = e.ingest.run(RunOpts::default(), now()).await.unwrap();
    assert_eq!((r.planned, r.ingested, r.failed), (1, 0, 0), "{r:?}");
    assert!(r.vault_waiting_for.as_deref().unwrap().contains("No route"));
    let st = e.ingest.status().unwrap();
    assert_eq!((st.undelivered, st.queue.failed), (1, 0));
    assert!(st.vault.offline_since.is_some());
    assert_eq!(
        list(&e.ingest.staging_dir(), "").len(),
        1,
        "the draft is staged"
    );
    // Backoff: the next tick does not even try; still nothing lost.
    let r = e
        .ingest
        .run(RunOpts::default(), now() + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!((r.outcome.as_str(), r.ingested), ("waiting", 0), "{r:?}");
    // Back online: delivered from the stored draft, without asking Qwen again.
    e.offline.store(false, Ordering::SeqCst);
    let r = e
        .ingest
        .run(RunOpts::default(), now() + Duration::minutes(11))
        .await
        .unwrap();
    assert_eq!((r.planned, r.ingested), (0, 1), "{r:?}");
    assert!(e
        .vault
        .join("LucinaRS/Events/2026-11-01 Drafted while the Pi is off.md")
        .exists());
    let st = e.ingest.status().unwrap();
    assert_eq!((st.undelivered, st.queue.done), (0, 1));
    assert!(st.vault.offline_since.is_none() && st.vault.last_ok.is_some());
}

#[tokio::test]
async fn an_edit_made_meanwhile_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let s = LocalStore::open(dir.path()).unwrap();
    s.write("P/Permanent/x.md", "owner v1", None).await.unwrap();
    let read_at = lucina_vault_ingest::store::version(Some("owner v1"));
    // The owner edits between our read and our write.
    std::fs::write(dir.path().join("P/Permanent/x.md"), "owner v2").unwrap();
    assert!(matches!(
        s.write("P/Permanent/x.md", "ours", read_at.as_deref())
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("P/Permanent/x.md")).unwrap(),
        "owner v2"
    );
}

#[tokio::test]
async fn a_project_without_a_vault_folder_is_skipped_without_qwen() {
    let gw = gateway(ready()).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(store_first_plan()))
        .expect(0)
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    let mut it = item("session:u", "University lecture");
    it.project = "university".into();
    e.ingest.queue().enqueue(vec![it], now()).unwrap();
    let r = e.ingest.run(RunOpts::default(), now()).await.unwrap();
    assert_eq!(r.skipped, 1, "{r:?}");
}

#[tokio::test]
async fn a_busy_gpu_means_wait_and_back_off() {
    let gw = gateway(busy()).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(store_first_plan()))
        .expect(0)
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    e.ingest
        .queue()
        .enqueue(vec![item("session:a", "A")], now())
        .unwrap();
    let r = e
        .ingest
        .run(
            RunOpts {
                cold: true,
                ..Default::default()
            },
            now(),
        )
        .await
        .unwrap();
    assert_eq!(r.outcome, "waiting");
    assert!(r.waiting_for.as_deref().unwrap().contains("busy"), "{r:?}");
    let next = r.next_try.unwrap();
    assert!(next > now());
    // The backoff holds the next tick, and the item is untouched.
    let r = e
        .ingest
        .run(RunOpts::default(), now() + Duration::seconds(30))
        .await
        .unwrap();
    assert_eq!((r.outcome.as_str(), r.next_try), ("waiting", Some(next)));
    let st = e.ingest.status().unwrap();
    assert_eq!(
        (st.queue.pending, st.queue.failed, st.next_try),
        (1, 0, Some(next))
    );
    match &e.ingest.queue().slots().unwrap()["session:a"].state {
        State::Pending { attempts, .. } => assert_eq!(*attempts, 0, "waiting is not failing"),
        s => panic!("{s:?}"),
    }
}

#[tokio::test]
async fn an_unloaded_model_is_loaded_only_when_allowed() {
    let gw = gateway(idle()).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(store_first_plan()))
        .expect(1)
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    e.ingest
        .queue()
        .enqueue(vec![item("session:a", "A")], now())
        .unwrap();
    let r = e.ingest.run(RunOpts::default(), now()).await.unwrap();
    assert_eq!(r.outcome, "waiting");
    assert!(r.waiting_for.unwrap().contains("unloaded"));
    // Old enough for a cold start (6 h by default).
    let later = now() + Duration::hours(7);
    let r = e.ingest.run(RunOpts::default(), later).await.unwrap();
    assert_eq!(r.ingested, 1, "{r:?}");
}

#[tokio::test]
async fn a_manual_run_may_load_the_model() {
    let gw = gateway(idle()).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(store_first_plan()))
        .expect(1)
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    e.ingest
        .queue()
        .enqueue(vec![item("session:a", "A")], now())
        .unwrap();
    let r = e
        .ingest
        .run(
            RunOpts {
                force: true,
                cold: true,
                limit: None,
            },
            now(),
        )
        .await
        .unwrap();
    assert_eq!(r.ingested, 1, "{r:?}");
}

#[tokio::test]
async fn a_gateway_refusal_is_a_wait_and_bad_answers_are_failures() {
    let gw = gateway(ready()).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("busy:lease_queue"))
        .up_to_n_times(1)
        .mount(&gw)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(json!("not an object")))
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    e.ingest
        .queue()
        .enqueue(vec![item("session:a", "A")], now())
        .unwrap();
    let r = e.ingest.run(RunOpts::default(), now()).await.unwrap();
    assert_eq!(r.outcome, "waiting");
    assert!(r.waiting_for.unwrap().contains("503"));
    let mut t = now();
    for n in 1..=MAX_ATTEMPTS {
        t += Duration::hours(7);
        let r = e.ingest.run(RunOpts::default(), t).await.unwrap();
        assert_eq!(r.failed, 1, "attempt {n}: {r:?}");
    }
    let st = e.ingest.status().unwrap();
    assert_eq!((st.queue.pending, st.queue.failed), (0, 1));
    assert_eq!(st.failed[0].key, "session:a");
    assert_eq!(e.ingest.retry(&[], false, t).unwrap(), 1);
    assert_eq!(e.ingest.status().unwrap().queue.pending, 1);
}

#[tokio::test]
async fn a_stored_draft_is_delivered_without_the_model() {
    // The gateway is down: only the stored draft can make the notes.
    let e = env("http://127.0.0.1:9");
    let it = item("session:p", "Planned before a crash");
    e.ingest.queue().enqueue(vec![it.clone()], now()).unwrap();
    let plan =
        lucina_vault_ingest::plan::Plan::from_answer(&store_first_plan().to_string(), &it, &[])
            .unwrap();
    e.ingest.queue().planned("session:p", &plan, now()).unwrap();
    let r = e.ingest.run(RunOpts::default(), now()).await.unwrap();
    assert_eq!(r.ingested, 1, "{r:?}");
    assert!(e
        .vault
        .join("LucinaRS/Events/2026-11-01 Planned before a crash.md")
        .exists());
}

#[tokio::test]
async fn skipped_items_write_nothing() {
    let gw = gateway(ready()).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answer(json!({"skip": true, "skip_reason": "typo fix"})))
        .mount(&gw)
        .await;
    let e = env(&gw.uri());
    e.ingest
        .queue()
        .enqueue(vec![item("session:t", "fix typo")], now())
        .unwrap();
    let r = e.ingest.run(RunOpts::default(), now()).await.unwrap();
    assert_eq!((r.ingested, r.skipped), (0, 1));
    assert!(list(&e.vault, "LucinaRS").is_empty());
    assert_eq!(e.ingest.status().unwrap().queue.skipped, 1);
}

#[tokio::test]
async fn ops_records_are_queued_once() {
    let e = env("http://127.0.0.1:9");
    let rec: lucina_ops::NewRecord = serde_json::from_value(json!({
        "kind": "incident", "project": "lucina", "title": "p1 starved by a journalctl glob",
        "body": "An orphaned journalctl kept p1 busy for 12 h.", "severity": "high",
        "cause": "ssh stopped locally", "fix": "remote timeout"
    }))
    .unwrap();
    let r = e.ops.record(rec, "mcp:claude").await.unwrap();
    let own: lucina_ops::NewRecord = serde_json::from_value(json!({
        "kind": "status", "project": "lucina", "title": "ingest ran"
    }))
    .unwrap();
    e.ops.record(own, "vault-ingest").await.unwrap();
    let s = e.ingest.scan(Utc::now()).await.unwrap();
    assert_eq!(s.ops, 1, "{s:?}");
    assert_eq!(e.ingest.scan(Utc::now()).await.unwrap().ops, 0);
    let slots = e.ingest.queue().slots().unwrap();
    let item = slots[&format!("ops:{}", r.id)].item.clone().unwrap();
    assert_eq!(item.project, "lucina");
    assert!(item.text.contains("Cause: ssh stopped locally"));
    match item.backlink {
        Backlink::Ops { log, .. } => assert!(log.starts_with("Log/")),
        b => panic!("{b:?}"),
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[tokio::test]
async fn git_commits_and_merges_are_queued_once() {
    let repo = tempfile::tempdir().unwrap();
    let r = repo.path();
    git(r, &["init", "-q", "-b", "main"]);
    for i in 0..3 {
        std::fs::write(r.join(format!("f{i}.rs")), format!("// {i}\n")).unwrap();
        git(r, &["add", "."]);
        git(
            r,
            &[
                "commit",
                "-q",
                "-m",
                &format!("commit {i}"),
                "-m",
                "token ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            ],
        );
    }
    let spec = format!("demo={}@main#lucina", r.display());
    let e = env_with(
        "http://127.0.0.1:9",
        vec![lucina_vault_ingest::sources::GitRepo::parse(&spec).unwrap()],
    );
    let s = e.ingest.scan(Utc::now()).await.unwrap();
    assert_eq!(s.git["demo"], 3, "{s:?}");
    assert_eq!(e.ingest.scan(Utc::now()).await.unwrap().git["demo"], 0);

    git(r, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(r.join("t.rs"), "// t\n").unwrap();
    git(r, &["add", "."]);
    git(r, &["commit", "-q", "-m", "topic work"]);
    git(r, &["checkout", "-q", "main"]);
    git(
        r,
        &[
            "merge",
            "-q",
            "--no-ff",
            "topic",
            "-m",
            "Merge branch topic",
        ],
    );
    let head = git(r, &["rev-parse", "HEAD"]);
    let s = e.ingest.scan(Utc::now()).await.unwrap();
    assert_eq!(s.git["demo"], 1, "only the merge (first parent)");
    let slots = e.ingest.queue().slots().unwrap();
    let m = slots[&format!("git:demo:{head}")].item.clone().unwrap();
    assert!(
        m.text.contains("Commits it merged:") && m.text.contains("topic work"),
        "{}",
        m.text
    );
    assert_eq!(m.files, [format!("{}/t.rs", r.display())]);
    assert!(matches!(m.backlink, Backlink::Git { merge: true, .. }));
    for s in slots.values() {
        let t = &s.item.as_ref().unwrap().text;
        assert!(!t.contains("ghp_"), "commit messages are redacted: {t}");
    }
    let st = e.ingest.status().unwrap();
    assert_eq!(st.repos[0].cursor.as_deref(), Some(head.as_str()));
}

#[test]
fn a_remote_model_url_or_a_repo_vault_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("v");
    std::fs::create_dir_all(&vault).unwrap();
    let ops = Arc::new(
        Ops::open(
            OpsConfig::new(&vault, dir.path().join("s"), chrono_tz::Europe::Rome),
            None,
        )
        .unwrap(),
    );
    let mut cfg = IngestConfig::new(dir.path().join("i"), chrono_tz::Europe::Rome);
    cfg.target = format!("dir:{}", vault.display());
    cfg.qwen_url = "https://api.deepseek.com".into();
    assert!(Ingest::open(cfg.clone(), ops.clone()).is_err());
    cfg.qwen_url = lucina_vault_ingest::DEFAULT_QWEN_URL.into();
    assert!(Ingest::open(cfg.clone(), ops.clone()).is_ok());
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    assert!(
        Ingest::open(cfg, ops).is_err(),
        "never a vault inside a git repo"
    );
}
