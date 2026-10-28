//! The ops record end to end on temp vaults: confinement, page structure,
//! dedup, redaction, markers, backfill, incidents, conflicts and Honcho.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use lucina_memory::{HonchoClient, HonchoConfig};
use lucina_ops::{
    Kind, NewRecord, Ops, OpsConfig, OpsError, SearchQuery, Severity, Vault, VaultError,
};
use serde_json::json;
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct World {
    _dir: tempfile::TempDir,
    vault: PathBuf,
    state: PathBuf,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault/Lucina");
    std::fs::create_dir_all(&vault).unwrap();
    let state = dir.path().join("state/ops");
    World {
        _dir: dir,
        vault,
        state,
    }
}

fn cfg(w: &World) -> OpsConfig {
    OpsConfig::new(&w.vault, &w.state, chrono_tz::Europe::Rome)
}

fn ops(w: &World) -> Ops {
    Ops::open(cfg(w), None).unwrap()
}

fn honcho(uri: &str) -> HonchoClient {
    HonchoClient::new(HonchoConfig {
        base_url: uri.to_owned(),
        ..HonchoConfig::default()
    })
    .unwrap()
}

/// 2026-10-28 16:00 in Rome.
fn now() -> DateTime<Utc> {
    "2026-10-28T15:00:00Z".parse().unwrap()
}

fn rec(kind: Kind, project: &str, title: &str, at: &str) -> NewRecord {
    let mut r = NewRecord::new(kind, project, title);
    r.at = Some(at.parse().unwrap());
    r
}

fn read(w: &World, rel: &str) -> String {
    std::fs::read_to_string(w.vault.join(rel)).unwrap()
}

/// Every file under `root` (relative paths), hidden ones included.
fn files(root: &Path) -> Vec<String> {
    fn go(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let ft = std::fs::symlink_metadata(&p).unwrap().file_type();
            if ft.is_dir() {
                go(root, &p, out);
            } else {
                out.push(p.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    if root.exists() {
        go(root, root, &mut out);
    }
    out.sort();
    out
}

fn all_text(root: &Path) -> String {
    files(root)
        .iter()
        .map(|f| std::fs::read_to_string(root.join(f)).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

fn store_lines(w: &World) -> Vec<serde_json::Value> {
    std::fs::read_to_string(w.state.join("records.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// ---------------------------------------------------------------- confinement

#[test]
fn page_paths_are_confined() {
    let w = world();
    let v = Vault::open(&w.vault).unwrap();
    assert!(w.vault.join("README.md").is_file());
    for bad in [
        "../x.md",
        "Map/../../x.md",
        "/tmp/x.md",
        "Map/.hidden.md",
        "Map/.obsidian/x.md",
        "Map\\x.md",
        "Map/x\0.md",
        "Map/x.txt",
        "Map/x",
        "Map/./x.md",
        "Map//x.md",
        "",
    ] {
        let e = v.write_page(bad, "x").unwrap_err();
        assert!(matches!(e, VaultError::InvalidPath(..)), "{bad:?}: {e:?}");
    }
    // Outside Map/: refused even when the path itself is fine.
    for other in ["Projects/x.md", "Log/2026-10-28.md", "README.md", "Map.md"] {
        assert!(
            matches!(v.write_page(other, "x"), Err(VaultError::NotAllowed(_))),
            "{other}"
        );
    }
    assert!(v.write_page("Map/lucina/overview.md", "# map\n").unwrap());
    assert_eq!(read(&w, "Map/lucina/overview.md"), "# map\n");
    // The same content again is a no-op; new content replaces the page.
    let mtime = |w: &World| {
        std::fs::metadata(w.vault.join("Map/lucina/overview.md"))
            .unwrap()
            .modified()
            .unwrap()
    };
    let before = mtime(&w);
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(!v.write_page("Map/lucina/overview.md", "# map\n").unwrap());
    assert_eq!(mtime(&w), before);
    assert!(v
        .write_page("Map/lucina/overview.md", "# map v2\n")
        .unwrap());
    assert_eq!(read(&w, "Map/lucina/overview.md"), "# map v2\n");
    // Map pages are redacted too.
    lucina_secrets::redactor().register("OPS_MAP_TEST_KEY", "map-Secret-Value-7781zq");
    v.write_page(
        "Map/lucina/leak.md",
        "key map-Secret-Value-7781zq and ghp_ABCDEFGHIJKLMNOPQRSTuvwx1234\n",
    )
    .unwrap();
    let leak = read(&w, "Map/lucina/leak.md");
    assert!(!leak.contains("map-Secret-Value-7781zq"), "{leak}");
    assert!(!leak.contains("ghp_ABCDEFGHIJKLMNOPQRST"), "{leak}");
    // Nothing outside the root.
    let parent = w.vault.parent().unwrap();
    assert_eq!(
        files(parent)
            .iter()
            .filter(|f| !f.starts_with("Lucina/"))
            .count(),
        0
    );
}

#[tokio::test]
async fn symlinks_are_refused() {
    let w = world();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), w.vault.join("Projects")).unwrap();
    let o = ops(&w);
    let e = o
        .record_at(
            rec(
                Kind::Decision,
                "lucina",
                "Use SQLite",
                "2026-10-28T14:05:00+01:00",
            ),
            "cli",
            now(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, OpsError::Vault(VaultError::SymlinkRefused(ref p)) if p == "Projects"),
        "{e:?}"
    );
    assert!(files(elsewhere.path()).is_empty());

    // A symlinked target file (Map/ via write_page, and an ops page).
    let outside = elsewhere.path().join("target.md");
    std::fs::write(&outside, "keep").unwrap();
    std::fs::create_dir_all(w.vault.join("Map")).unwrap();
    std::os::unix::fs::symlink(&outside, w.vault.join("Map/x.md")).unwrap();
    assert!(matches!(
        o.vault().write_page("Map/x.md", "overwritten"),
        Err(VaultError::SymlinkRefused(_))
    ));
    assert!(matches!(
        o.vault().read("Map/x.md"),
        Err(VaultError::SymlinkRefused(_))
    ));
    std::fs::create_dir_all(w.vault.join("Log")).unwrap();
    std::os::unix::fs::symlink(&outside, w.vault.join("Log/2026-10-27.md")).unwrap();
    std::fs::remove_file(w.vault.join("Projects")).unwrap();
    let e = o
        .record_at(
            rec(Kind::Status, "lucina", "Green", "2026-10-27T10:00:00+01:00"),
            "cli",
            now(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, OpsError::Vault(VaultError::SymlinkRefused(_))),
        "{e:?}"
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    assert_eq!(files(elsewhere.path()), vec!["target.md"]);
}

// ---------------------------------------------------------------- pages

#[tokio::test]
async fn pages_keep_their_structure_and_the_owners_text() {
    let w = world();
    let o = ops(&w);
    let mut d = rec(
        Kind::Deploy,
        "lucina",
        "Lucina Agents phase 5 live",
        "2026-10-28T14:05:00+01:00",
    );
    d.commit = Some("90e9426abc".into());
    d.host = Some("nezopt".into());
    d.body = "Agents run in worktrees.\nSecond line.".into();
    d.links = vec!["https://github.com/x/y/pull/1".into()];
    let r = o.record_at(d, "mcp:claude", now()).await.unwrap();
    assert_eq!(r.paths.log, "Log/2026-10-28.md");
    assert_eq!(r.paths.project, "Projects/lucina.md");
    assert!(!r.duplicate && r.honcho == "off");
    let log = read(&w, "Log/2026-10-28.md");
    assert!(
        log.contains(&format!(
            "- 14:05 **deploy** · [[Projects/lucina|lucina]] · Lucina Agents phase 5 live (`90e9426` on nezopt) · mcp:claude <!-- ops:{} -->\n  Agents run in worktrees. Second line.",
            r.id
        )),
        "{log}"
    );
    let page = read(&w, "Projects/lucina.md");
    assert!(
        page.starts_with("---\ntags: [lucina-ops]\nproject: lucina\n---\n# lucina\n"),
        "{page}"
    );
    assert!(page.contains(&format!(
        "## Deploys\n\n- 2026-10-28 14:05 · **Lucina Agents phase 5 live** (`90e9426` on nezopt) <!-- ops:{} -->\n  Agents run in worktrees.\n  Second line.\n  - https://github.com/x/y/pull/1\n",
        r.id
    )), "{page}");

    // The owner writes free text and adds a section of their own.
    let owner_top = "# lucina\n\nMy notes about Lucina.\nDo not touch.\n\n";
    let page = page.replacen("# lucina\n\n", owner_top, 1);
    let page = page.replacen(
        "## Deploys",
        "## My section\n\nOwner text  with  spaces.\n\n## Deploys",
        1,
    );
    std::fs::write(w.vault.join("Projects/lucina.md"), &page).unwrap();
    let before_sections = page[..page.find("## ").unwrap()].to_owned();

    for (i, t) in ["Second deploy", "Third deploy"].iter().enumerate() {
        o.record_at(
            rec(
                Kind::Deploy,
                "lucina",
                t,
                &format!("2026-10-28T15:0{i}:00+01:00"),
            ),
            "cli",
            now(),
        )
        .await
        .unwrap();
    }
    o.record_at(
        rec(
            Kind::Decision,
            "lucina",
            "Keep SQLite",
            "2026-10-28T15:30:00+01:00",
        ),
        "cli",
        now(),
    )
    .await
    .unwrap();
    let page = read(&w, "Projects/lucina.md");
    assert!(page.starts_with(&before_sections), "{page}");
    assert!(
        page.contains("## My section\n\nOwner text  with  spaces.\n\n## Deploys\n"),
        "{page}"
    );
    for s in [
        "Status",
        "Decisions",
        "Incidents",
        "Deploys",
        "Findings",
        "Open questions",
    ] {
        assert_eq!(
            page.matches(&format!("\n## {s}\n")).count(),
            1,
            "{s}: {page}"
        );
    }
    // Newest first.
    let (third, second, first) = (
        page.find("Third deploy").unwrap(),
        page.find("Second deploy").unwrap(),
        page.find("Lucina Agents phase 5").unwrap(),
    );
    assert!(third < second && second < first, "{page}");
    // No temp files left.
    assert!(
        files(&w.vault)
            .iter()
            .all(|f| !f.split('/').any(|c| c.starts_with('.'))),
        "{:?}",
        files(&w.vault)
    );

    // The owner deleted a section: it comes back at the end.
    let page = page.replacen("## Findings\n\n", "", 1);
    std::fs::write(w.vault.join("Projects/lucina.md"), &page).unwrap();
    o.record_at(
        rec(
            Kind::Finding,
            "lucina",
            "Slow cold start",
            "2026-10-28T15:40:00+01:00",
        ),
        "cli",
        now(),
    )
    .await
    .unwrap();
    let page = read(&w, "Projects/lucina.md");
    assert!(
        page.trim_end().ends_with(&format!(
            "## Findings\n\n- 2026-10-28 15:40 · **Slow cold start** <!-- ops:{} -->",
            lucina_ops::record::record_id("finding|lucina|slow cold start|")
        )),
        "{page}"
    );
}

#[tokio::test]
async fn status_keeps_the_ten_newest() {
    let w = world();
    let o = ops(&w);
    for i in 0..12 {
        o.record_at(
            rec(
                Kind::Status,
                "pi",
                &format!("status {i:02}"),
                &format!("2026-10-28T1{}:{:02}:00+01:00", i / 6, i % 6 * 10),
            ),
            "cli",
            now(),
        )
        .await
        .unwrap();
    }
    let page = read(&w, "Projects/pi.md");
    assert!(
        !page.contains("status 00") && !page.contains("status 01"),
        "{page}"
    );
    assert!(page.contains("status 02") && page.contains("status 11"));
    assert_eq!(page.matches("<!-- ops:").count(), 10);
    // They stay in the log and the store.
    assert_eq!(
        read(&w, "Log/2026-10-28.md").matches("<!-- ops:").count(),
        12
    );
    assert_eq!(store_lines(&w).len(), 12);
}

#[tokio::test]
async fn dedup() {
    let w = world();
    let o = ops(&w);
    let mut d = rec(
        Kind::Deploy,
        "lucina",
        "Phase 5 live",
        "2026-10-28T14:05:00+01:00",
    );
    d.commit = Some("90e9426".into());
    let a = o.record_at(d.clone(), "cli", now()).await.unwrap();
    let mut again = d.clone();
    again.title = "  phase 5   LIVE! ".into();
    let b = o.record_at(again, "cli", now()).await.unwrap();
    assert!(!a.duplicate && b.duplicate);
    assert_eq!(a.id, b.id);
    assert_eq!(a.paths, b.paths);
    assert_eq!(store_lines(&w).len(), 1);
    assert_eq!(
        read(&w, "Log/2026-10-28.md").matches("<!-- ops:").count(),
        1
    );
    // A different commit is a different deploy.
    d.commit = Some("1234567".into());
    let c = o.record_at(d, "cli", now()).await.unwrap();
    assert!(!c.duplicate && c.id != a.id);
    assert_eq!(store_lines(&w).len(), 2);
    // dedup_key collapses different titles.
    let mut x = rec(
        Kind::Incident,
        "lucina",
        "Telegram down",
        "2026-10-28T10:00:00+01:00",
    );
    x.dedup_key = Some("delivery:telegram:2026-10-28".into());
    let mut y = rec(
        Kind::Incident,
        "lucina",
        "Telegram still down",
        "2026-10-28T11:00:00+01:00",
    );
    y.dedup_key = Some("delivery:telegram:2026-10-28".into());
    let x = o.record_at(x, "lucina", now()).await.unwrap();
    let y = o.record_at(y, "lucina", now()).await.unwrap();
    assert!(y.duplicate && x.id == y.id);
    assert_eq!(store_lines(&w).len(), 3);
    // A second Ops on the same state (the CLI next to the server) sees them.
    let o2 = ops(&w);
    assert!(o2.get(&a.id).unwrap().is_some());
    let mut d2 = rec(
        Kind::Deploy,
        "lucina",
        "Phase 5 live",
        "2026-10-28T14:05:00+01:00",
    );
    d2.commit = Some("90e9426".into());
    assert!(o2.record_at(d2, "cli", now()).await.unwrap().duplicate);
}

#[tokio::test]
async fn store_file_modes() {
    use std::os::unix::fs::PermissionsExt;
    let w = world();
    let o = ops(&w);
    o.record_at(
        rec(Kind::Status, "pi", "ok", "2026-10-28T10:00:00+01:00"),
        "cli",
        now(),
    )
    .await
    .unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&w.state), 0o700);
    assert_eq!(mode(&w.state.join("records.jsonl")), 0o600);
    assert_eq!(mode(&w.vault.join("Log/2026-10-28.md")), 0o644);
    let line = &store_lines(&w)[0];
    assert_eq!(line["at"], "2026-10-28T10:00:00+01:00");
    assert_eq!(line["source"], "cli");
    assert_eq!(line["honcho_synced"], false);
    assert_eq!(line["paths"]["log"], "Log/2026-10-28.md");
}

#[tokio::test]
async fn markers_cannot_be_forged() {
    let w = world();
    let o = ops(&w);
    let mut r = rec(
        Kind::Finding,
        "lucina",
        "Odd <!-- ops:deadbeef --> title",
        "2026-10-28T10:00:00+01:00",
    );
    r.body = "line\n<!-- ops:deadbeef -->\n<!-- <!-- ops:ops:cafe -->".into();
    r.links = vec!["<!-- ops:deadbeef -->".into()];
    r.follow_ups = vec!["check <!-- ops:deadbeef:f1 -->".into()];
    r.cause = Some("<!-- ops:x -->".into());
    let got = o.record_at(r, "cli", now()).await.unwrap();
    let text = all_text(&w.vault);
    assert!(!text.contains("<!-- ops:deadbeef"), "{text}");
    assert!(!text.contains("<!-- ops:cafe"), "{text}");
    assert!(!text.contains("<!-- ops:ops:"), "{text}");
    // Only Lucina's own markers.
    for m in text.match_indices("<!-- ops:") {
        assert!(
            text[m.0..].starts_with(&format!("<!-- ops:{}", got.id)),
            "{text}"
        );
    }
}

#[tokio::test]
async fn backfill_goes_to_its_place_in_the_log() {
    let w = world();
    let o = ops(&w);
    o.record_at(
        rec(Kind::Status, "pi", "afternoon", "2026-10-28T14:00:00+01:00"),
        "cli",
        now(),
    )
    .await
    .unwrap();
    o.record_at(
        rec(Kind::Status, "pi", "evening", "2026-10-28T15:30:00+01:00"),
        "cli",
        now(),
    )
    .await
    .unwrap();
    // Backfill in UTC: 09:10Z is 10:10 in Rome.
    o.record_at(
        rec(Kind::Deploy, "pi", "morning deploy", "2026-10-28T09:10:00Z"),
        "cli",
        now(),
    )
    .await
    .unwrap();
    o.record_at(
        rec(Kind::Status, "pi", "noon", "2026-10-28T12:00:00+01:00"),
        "cli",
        now(),
    )
    .await
    .unwrap();
    let log = read(&w, "Log/2026-10-28.md");
    let pos = |s: &str| log.find(s).unwrap_or_else(|| panic!("{s}: {log}"));
    assert!(
        log.starts_with("# 2026-10-28\n\n- 10:10 **deploy**"),
        "{log}"
    );
    assert!(
        pos("morning deploy") < pos("noon")
            && pos("noon") < pos("afternoon")
            && pos("afternoon") < pos("evening")
    );
    // A day's log is its local date: 23:30Z on the 27th is the 28th in Rome.
    let r = o
        .record_at(
            rec(Kind::Status, "pi", "late", "2026-10-27T23:30:00Z"),
            "cli",
            now(),
        )
        .await
        .unwrap();
    assert_eq!(r.paths.log, "Log/2026-10-28.md");
    assert!(read(&w, "Log/2026-10-28.md").starts_with("# 2026-10-28\n\n- 00:30 **status**"));
}

#[tokio::test]
async fn incidents_get_a_page_and_open_questions() {
    let w = world();
    let o = ops(&w);
    let mut r = rec(
        Kind::Incident,
        "pi",
        "Pokoin sync outage",
        "2026-10-28T14:05:00+01:00",
    );
    r.body = "Sync stopped for 2 hours.".into();
    r.timeline = vec!["14:05 sync stopped".into(), "16:00 restarted".into()];
    r.cause = Some("Disk full".into());
    r.follow_ups = vec!["Add a disk alert".into(), "Rotate logs".into()];
    r.host = Some("pi".into());
    r.severity = Some(Severity::High);
    let got = o.record_at(r, "mcp:claude", now()).await.unwrap();
    let inc = got.paths.incident.clone().unwrap();
    assert_eq!(inc, "Incidents/2026-10-28-pi-pokoin-sync-outage.md");
    let page = read(&w, &inc);
    assert!(
        page.starts_with("---\ntags: [lucina-ops, incident]\n"),
        "{page}"
    );
    for s in [
        &format!("ops_id: {}", got.id),
        "severity: high",
        "status: open",
        "# Pokoin sync outage\n",
        "## Summary\n\nSync stopped for 2 hours.",
        "## Timeline\n\n- 14:05 sync stopped\n- 16:00 restarted",
        "## Cause\n\nDisk full",
        "## Fix\n\n_unknown_",
        "## Follow-ups\n\n- [ ] Add a disk alert\n- [ ] Rotate logs",
    ] {
        assert!(page.contains(s), "{s}: {page}");
    }
    assert!(!page.contains("## Links"));
    let proj = read(&w, "Projects/pi.md");
    assert!(proj.contains(&format!(
        "## Incidents\n\n- 2026-10-28 14:05 · **Pokoin sync outage** (on pi, severity high) <!-- ops:{} -->\n  Sync stopped for 2 hours.\n  - [[Incidents/2026-10-28-pi-pokoin-sync-outage|incident page]]",
        got.id
    )), "{proj}");
    assert!(proj.contains(&format!(
        "## Open questions\n\n- [ ] Add a disk alert ([[Log/2026-10-28|2026-10-28]]) <!-- ops:{0}:f1 -->\n- [ ] Rotate logs ([[Log/2026-10-28|2026-10-28]]) <!-- ops:{0}:f2 -->",
        got.id
    )), "{proj}");
    assert!(read(&w, "Log/2026-10-28.md")
        .contains("[[Incidents/2026-10-28-pi-pokoin-sync-outage|incident]]"));
    // Same title, another incident: -2. Fixed: resolved.
    let mut again = rec(
        Kind::Incident,
        "pi",
        "Pokoin sync outage",
        "2026-10-28T15:05:00+01:00",
    );
    again.dedup_key = Some("second".into());
    again.fix = Some("Freed space".into());
    let second = o.record_at(again, "cli", now()).await.unwrap();
    assert_eq!(
        second.paths.incident.as_deref(),
        Some("Incidents/2026-10-28-pi-pokoin-sync-outage-2.md")
    );
    assert!(
        read(&w, "Incidents/2026-10-28-pi-pokoin-sync-outage-2.md").contains("status: resolved")
    );
    // Follow-ups of other kinds go to Open questions too.
    let mut dec = rec(
        Kind::Decision,
        "pi",
        "Move to SSD",
        "2026-10-28T15:10:00+01:00",
    );
    dec.follow_ups = vec!["Order the SSD".into()];
    o.record_at(dec, "cli", now()).await.unwrap();
    assert!(read(&w, "Projects/pi.md").contains("- [ ] Order the SSD ("));
}

// ---------------------------------------------------------------- redaction

const OPS_SECRET: &str = "ops-Registered-Secret-91xQ";
const GHP: &str = "ghp_ABCDEFGHIJKLMNOPQRSTuvwxyz0123456789";

#[tokio::test]
async fn secrets_never_reach_any_sink() {
    lucina_secrets::redactor().register("OPS_TEST_KEY", OPS_SECRET);
    let s = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&s)
        .await;
    let w = world();
    let o = Ops::open(cfg(&w), Some(honcho(&s.uri()))).unwrap();
    let mut r = rec(
        Kind::Incident,
        "lucina",
        &format!("Key {OPS_SECRET} leaked"),
        "2026-10-28T14:05:00+01:00",
    );
    r.body = format!("token was {GHP} in the log");
    r.cause = Some(format!("env had {OPS_SECRET}"));
    r.links = vec![format!("https://x.test/?t={GHP}")];
    r.follow_ups = vec![format!("rotate {GHP}")];
    let got = o.record_at(r, "mcp:claude", now()).await.unwrap();
    assert!(got.redacted);
    assert_eq!(got.honcho, "ok");
    let vault = all_text(&w.vault);
    let store = std::fs::read_to_string(w.state.join("records.jsonl")).unwrap();
    for (name, text) in [("vault", &vault), ("store", &store)] {
        assert!(!text.contains(OPS_SECRET), "{name}: {text}");
        assert!(!text.contains("ghp_ABCDEFGHIJKLMNOPQRST"), "{name}: {text}");
    }
    assert!(vault.contains("[secret:OPS_TEST_KEY]"));
    let reqs = s.received_requests().await.unwrap();
    assert!(reqs.iter().any(|r| r.url.path().ends_with("/messages")));
    for r in reqs {
        let body = String::from_utf8_lossy(&r.body);
        assert!(
            !body.contains(OPS_SECRET) && !body.contains("ghp_ABCDEFGHIJKLMNOPQRST"),
            "{body}"
        );
    }
    // The incident page name is made from the redacted title.
    assert!(!got.paths.incident.unwrap().contains("91xq"));
}

// ---------------------------------------------------------------- conflicts

#[tokio::test]
async fn sync_conflicts_are_moved_and_entries_restored() {
    let w = world();
    let o = ops(&w);
    let a = o
        .record_at(
            rec(Kind::Decision, "x", "Use Rust", "2026-10-28T10:00:00+01:00"),
            "cli",
            now(),
        )
        .await
        .unwrap();
    let mut b = rec(
        Kind::Decision,
        "x",
        "Drop Node",
        "2026-10-28T11:00:00+01:00",
    );
    b.follow_ups = vec!["Port the last script".into()];
    let b = o.record_at(b, "cli", now()).await.unwrap();
    // Syncthing: the Mac's older copy won and lost record b; ours is the conflict copy.
    let page = read(&w, "Projects/x.md");
    let ours = page.clone();
    let lost: String = page
        .lines()
        .filter(|l| !l.contains(&b.id))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(!lost.contains(&b.id) && lost.contains(&a.id));
    std::fs::write(w.vault.join("Projects/x.md"), &lost).unwrap();
    let conflict = "Projects/x.sync-conflict-20261028-140000-ABCDEFG.md";
    std::fs::write(w.vault.join(conflict), &ours).unwrap();
    // A symlink in the vault is skipped, never followed.
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(
        elsewhere
            .path()
            .join("y.sync-conflict-20261028-140000-ABCDEFG.md"),
        "x",
    )
    .unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), w.vault.join("Linked")).unwrap();

    let report = o.sweep_conflicts_at(now()).unwrap();
    assert_eq!(report.moved.len(), 1, "{report:?}");
    let moved = "Conflicts/2026-10-28/Projects__x.sync-conflict-20261028-140000-ABCDEFG.md";
    assert_eq!(report.moved[0].to, moved);
    assert_eq!(report.reinserted, 1);
    assert!(!w.vault.join(conflict).exists());
    assert_eq!(read(&w, moved), ours);
    let page = read(&w, "Projects/x.md");
    assert!(
        page.contains(&format!("**Drop Node** <!-- ops:{} -->", b.id)),
        "{page}"
    );
    assert!(
        page.contains(&format!("<!-- ops:{}:f1 -->", b.id)),
        "{page}"
    );
    assert_eq!(page.matches(&format!("<!-- ops:{} -->", a.id)).count(), 1);
    let log = read(&w, "Log/2026-10-28.md");
    assert!(log.contains(&format!(
        "- 16:00 **status** · sync conflict on Projects/x.md: the other copy is in [[{}]] · lucina <!-- ops:conflict-",
        moved.strip_suffix(".md").unwrap()
    )), "{log}");
    assert!(elsewhere
        .path()
        .join("y.sync-conflict-20261028-140000-ABCDEFG.md")
        .exists());
    // A second sweep finds nothing.
    assert!(o.sweep_conflicts_at(now()).unwrap().moved.is_empty());

    // A conflict on a page a record touches is handled by the record itself.
    let log_conflict = "Log/2026-10-28.sync-conflict-20261028-150000-ZZZZ999.md";
    std::fs::write(w.vault.join(log_conflict), "old").unwrap();
    o.record_at(
        rec(Kind::Status, "x", "fine", "2026-10-28T15:45:00+01:00"),
        "cli",
        now(),
    )
    .await
    .unwrap();
    assert!(!w.vault.join(log_conflict).exists());
    assert!(w
        .vault
        .join("Conflicts/2026-10-28/Log__2026-10-28.sync-conflict-20261028-150000-ZZZZ999.md")
        .exists());
}

// ---------------------------------------------------------------- Honcho

async fn mount_ok(s: &MockServer) {
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(s)
        .await;
}

#[tokio::test]
async fn honcho_messages_carry_the_ops_id() {
    let s = MockServer::start().await;
    mount_ok(&s).await;
    let w = world();
    let o = Ops::open(cfg(&w), Some(honcho(&s.uri()))).unwrap();
    let mut r = rec(
        Kind::Deploy,
        "lucina",
        "Phase 5",
        "2026-10-28T14:05:00+01:00",
    );
    r.commit = Some("90e9426".into());
    r.host = Some("nezopt".into());
    let got = o.record_at(r, "mcp:claude", now()).await.unwrap();
    assert_eq!(got.honcho, "ok");
    let reqs = s.received_requests().await.unwrap();
    let msg = reqs
        .iter()
        .find(|r| r.url.path().ends_with("/sessions/lucina_ops/messages"))
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&msg.body).unwrap();
    let m = &body["messages"][0];
    assert_eq!(m["peer_id"], "lucina-ops");
    assert_eq!(m["metadata"]["ops_id"], got.id.as_str());
    assert_eq!(m["metadata"]["kind"], "deploy");
    assert_eq!(m["metadata"]["source"], "mcp:claude");
    assert_eq!(
        m["content"],
        "[deploy] lucina: Phase 5\n(commit 90e9426 on host nezopt)"
    );
    assert_eq!(store_lines(&w)[0]["honcho_synced"], true);
    // Off: no client.
    let w2 = world();
    let off = ops(&w2);
    let r = off
        .record_at(
            rec(Kind::Status, "pi", "ok", "2026-10-28T10:00:00+01:00"),
            "cli",
            now(),
        )
        .await
        .unwrap();
    assert_eq!(r.honcho, "off");
    assert_eq!(off.sync_pending().await.unwrap().attempted, 0);
}

#[tokio::test]
async fn honcho_failure_is_pending_until_synced() {
    let s = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .mount(&s)
        .await;
    let w = world();
    let o = Ops::open(cfg(&w), Some(honcho(&s.uri()))).unwrap();
    let a = o
        .record_at(
            rec(
                Kind::Decision,
                "lucina",
                "Use Honcho",
                "2026-10-28T10:00:00+01:00",
            ),
            "cli",
            now(),
        )
        .await
        .unwrap();
    let b = o
        .record_at(
            rec(
                Kind::Decision,
                "lucina",
                "Keep JSONL",
                "2026-10-28T11:00:00+01:00",
            ),
            "cli",
            now(),
        )
        .await
        .unwrap();
    assert_eq!(
        (a.honcho.as_str(), b.honcho.as_str()),
        ("pending", "pending")
    );
    assert!(store_lines(&w).iter().all(|l| l["honcho_synced"] == false));
    let rep = o.sync_pending().await.unwrap();
    assert_eq!((rep.synced, rep.failed), (0, 2));
    s.reset().await;
    mount_ok(&s).await;
    let rep = o.sync_pending().await.unwrap();
    assert_eq!((rep.attempted, rep.synced, rep.failed), (2, 2, 0));
    assert!(store_lines(&w).iter().all(|l| l["honcho_synced"] == true));
    assert!(o.get(&a.id).unwrap().unwrap().honcho_synced);
    // Oldest first.
    let reqs = s.received_requests().await.unwrap();
    let ids: Vec<String> = reqs
        .iter()
        .filter(|r| r.url.path().ends_with("/messages"))
        .map(|r| {
            let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            v["messages"][0]["metadata"]["ops_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(ids, vec![a.id.clone(), b.id.clone()]);
    assert_eq!(o.sync_pending().await.unwrap().attempted, 0);
    // The CLI writes without Honcho; the server's Ops syncs those records.
    let cli = ops(&w);
    cli.record_at(
        rec(
            Kind::Status,
            "lucina",
            "from the cli",
            "2026-10-28T12:00:00+01:00",
        ),
        "cli",
        now(),
    )
    .await
    .unwrap();
    assert_eq!(o.sync_pending().await.unwrap().synced, 1);
}

#[tokio::test]
async fn search_scores_filters_and_maps_honcho_hits() {
    let s = MockServer::start().await;
    let w = world();
    let o = ops(&w);
    let mut a = rec(
        Kind::Decision,
        "lucina",
        "Use SQLite for the store",
        "2026-10-20T10:00:00+02:00",
    );
    a.body = "Postgres is too much for one user.".into();
    let a = o.record_at(a, "cli", now()).await.unwrap();
    let b = o
        .record_at(
            rec(
                Kind::Deploy,
                "lucina",
                "Deploy the store",
                "2026-10-27T10:00:00+01:00",
            ),
            "cli",
            now(),
        )
        .await
        .unwrap();
    let c = o
        .record_at(
            rec(
                Kind::Deploy,
                "pi",
                "Deploy pokoin",
                "2026-10-28T10:00:00+01:00",
            ),
            "cli",
            now(),
        )
        .await
        .unwrap();
    // Title x3 beats body x1; ties: newer first.
    let r = o
        .search(SearchQuery {
            q: "store".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.honcho, "off");
    let ids: Vec<&str> = r.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec![b.id.as_str(), a.id.as_str()]);
    assert_eq!(r.items[0].matched, vec!["vault"]);
    // Accents fold, words split.
    let r = o
        .search(SearchQuery {
            q: "pérché postgres?".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.items[0].id, a.id);
    // Filters, empty q = newest first.
    let r = o
        .search(SearchQuery {
            kind: Some(Kind::Deploy),
            ..Default::default()
        })
        .await
        .unwrap();
    let ids: Vec<&str> = r.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec![c.id.as_str(), b.id.as_str()]);
    let r = o
        .search(SearchQuery {
            project: Some("lucina".into()),
            since: Some("2026-10-21".parse().unwrap()),
            until: Some("2026-10-27".parse().unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.items.len(), 1);
    assert_eq!(r.items[0].id, b.id);
    assert!(o
        .search(SearchQuery {
            limit: Some(51),
            ..Default::default()
        })
        .await
        .is_err());
    assert_eq!(
        o.get(&a.id).unwrap().unwrap().title,
        "Use SQLite for the store"
    );
    assert!(o.get("0000000000000000").unwrap().is_none());
    assert!(o.get("../x").unwrap().is_none());

    // Honcho on: a hit with an ops_id joins (even without a local match), hits without one are ignored.
    Mock::given(method("POST"))
        .and(path(
            "/v3/workspaces/hermes-peer1/sessions/lucina_ops/search",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"content": "[decision] lucina: Use SQLite", "metadata": {"ops_id": a.id}},
            {"content": "unrelated"},
            {"content": "gone", "metadata": {"ops_id": "ffffffffffffffff"}}
        ])))
        .mount(&s)
        .await;
    let mut hc = cfg(&w);
    hc.honcho_write = false;
    let cfg_honcho = HonchoConfig {
        base_url: s.uri(),
        workspace_id: "hermes-peer1".into(),
        ..HonchoConfig::default()
    };
    let oh = Ops::open(hc, Some(HonchoClient::new(cfg_honcho).unwrap())).unwrap();
    let r = oh
        .search(SearchQuery {
            q: "database choice".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.honcho, "ok");
    assert_eq!(r.items.len(), 1);
    assert_eq!(r.items[0].id, a.id);
    assert_eq!(r.items[0].matched, vec!["honcho"]);
    let r = oh
        .search(SearchQuery {
            q: "sqlite".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.items[0].matched, vec!["vault", "honcho"]);
    // Read-only Honcho: records are not written there.
    let rr = oh
        .record_at(
            rec(Kind::Status, "pi", "quiet", "2026-10-28T12:00:00+01:00"),
            "cli",
            now(),
        )
        .await
        .unwrap();
    assert_eq!(rr.honcho, "off");
    // Honcho errors: the local result still returns.
    s.reset().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .mount(&s)
        .await;
    let r = oh
        .search(SearchQuery {
            q: "sqlite".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.honcho, "error");
    assert_eq!(r.items[0].id, a.id);
}

#[tokio::test]
async fn validation_errors_name_the_field() {
    let w = world();
    let o = ops(&w);
    let e = o
        .record_at(
            NewRecord::new(Kind::Decision, "Lucina Agents", "x"),
            "cli",
            now(),
        )
        .await
        .unwrap_err();
    match e {
        OpsError::Invalid { field, message } => {
            assert_eq!(field, "project");
            assert!(message.contains("lucina-agents"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    let bad: Result<NewRecord, _> =
        serde_json::from_value(json!({"kind": "deploy", "project": "x", "title": "t", "extra": 1}));
    assert!(bad.is_err());
    let bad: Result<NewRecord, _> =
        serde_json::from_value(json!({"kind": "party", "project": "x", "title": "t"}));
    assert!(bad.is_err());
    assert!(store_lines(&w).is_empty());
}
