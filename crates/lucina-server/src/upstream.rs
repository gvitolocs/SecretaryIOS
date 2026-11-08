//! `lucina-rs upstream check | reconcile | status | validate` (docs/upstream-watch.md): the CLI around
//! crate `lucina-upstream`. JEV and the owner's go-aheads go through the running server
//! (`jev/upstream`, `/decisions`) with the token from the unit's env files, never printed.
//!
//! - **check** (weekly timer): reconcile, then per registry entry: changes → summary → verdict (JEV,
//!   else the heuristic) → at most `max_decisions_per_run` questions to the owner, one contribution
//!   offer, the run report and one ops record.
//! - **reconcile** (hourly timer): acts on the answers: "Port into ours" makes a Lucina Agents
//!   workspace and a porting brief; a contribution "Draft" writes a draft plan. Nothing is pushed.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use lucina_upstream::decision::{self, Chosen};
use lucina_upstream::fetch::{self, ChangeSet, OursRecent, SystemFetcher};
use lucina_upstream::judge::Verdict;
use lucina_upstream::registry::{ContribStatus, Level, Registry, Upstream};
use lucina_upstream::report::{self, BacklogItem, ContribLine, EntryResult, Run};
use lucina_upstream::state::{self, Asked, State};
use lucina_upstream::summary::{self, Summary};
use serde_json::{json, Value};

use crate::decide::cli::{setting, Client};

const DEFAULT_REPO: &str = "/home/nez/Projects/Lucina/SecretaryIOS";
/// The registry as built, when origin/main cannot be read.
const BUILT_IN: &str = include_str!("../../../docs/upstreams.toml");

pub struct CheckArgs {
    pub registry: Option<PathBuf>,
    pub only: Vec<String>,
    pub dry_run: bool,
    pub no_notify: bool,
    pub max_decisions: Option<usize>,
}

/// The registry: `--registry`, else `origin/main:docs/upstreams.toml` of `LUCINA_UPSTREAM_REPO`
/// (edits on main apply without a redeploy), else the built-in copy. Errors fail; warnings are returned.
pub fn load_registry(path: Option<&Path>) -> anyhow::Result<(Registry, String, Vec<String>)> {
    let mut warnings = Vec::new();
    let (text, source) = match path {
        Some(p) => (
            std::fs::read_to_string(p).with_context(|| format!("{}", p.display()))?,
            p.display().to_string(),
        ),
        None => {
            let repo = setting("LUCINA_UPSTREAM_REPO").unwrap_or_else(|| DEFAULT_REPO.into());
            let git = |args: &[&str]| {
                std::process::Command::new("timeout")
                    .arg("60")
                    .arg("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .env("GIT_TERMINAL_PROMPT", "0")
                    .output()
            };
            match git(&["fetch", "-q", "origin", "main"]) {
                Ok(o) if o.status.success() => {}
                _ => warnings.push(format!(
                    "git fetch origin main in {repo} failed; using its last origin/main"
                )),
            }
            match git(&["show", "origin/main:docs/upstreams.toml"]) {
                Ok(o) if o.status.success() => (
                    String::from_utf8_lossy(&o.stdout).into_owned(),
                    format!("{repo} origin/main:docs/upstreams.toml"),
                ),
                _ => {
                    warnings.push(format!(
                        "could not read origin/main:docs/upstreams.toml in {repo}; using the built-in copy"
                    ));
                    (BUILT_IN.to_owned(), "built-in copy".into())
                }
            }
        }
    };
    let reg = Registry::parse(&text).with_context(|| format!("registry {source}"))?;
    let problems = reg.validate();
    let errors: Vec<String> = problems
        .iter()
        .filter(|p| p.level == Level::Error)
        .map(|p| p.to_string())
        .collect();
    if !errors.is_empty() {
        anyhow::bail!("registry {source} is invalid:\n{}", errors.join("\n"));
    }
    warnings.extend(
        problems
            .iter()
            .filter(|p| p.level == Level::Warning)
            .map(|p| p.to_string()),
    );
    Ok((reg, source, warnings))
}

fn data_dir(reg: &Registry) -> PathBuf {
    setting("LUCINA_UPSTREAM_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&reg.settings.data_dir))
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn today() -> String {
    chrono::Local::now().date_naive().to_string()
}

/// JEV's verdict through the running server, or why not.
async fn ask_jev(
    c: Option<&Client>,
    up: &Upstream,
    cs: &ChangeSet,
    s: &Summary,
) -> Result<Verdict, String> {
    let c = c.ok_or("no Lucina API client")?;
    let body = json!({"upstream": up, "changes": cs.for_judge(), "summary": s});
    let v = c
        .post("jev/upstream", &body)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if v["verdict"].is_null() {
        return Err(format!(
            "jev: {}",
            v["jev"].as_str().unwrap_or("no verdict")
        ));
    }
    serde_json::from_value(v["verdict"].clone()).map_err(|e| format!("verdict: {e}"))
}

/// `asked dc-…`, or why the question did not go out.
async fn ask_owner(c: Option<&Client>, body: &Value) -> Result<Value, String> {
    let c = c.ok_or("no Lucina API client")?;
    c.create(body).await.map_err(|e| format!("{e:#}"))
}

struct Checked {
    up: Upstream,
    cs: ChangeSet,
    summary: Summary,
    verdict: Option<Verdict>,
    fallback: Option<String>,
    decision: Option<String>,
    ours: Vec<OursRecent>,
}

pub async fn check(a: CheckArgs) -> anyhow::Result<()> {
    let started = now();
    let date = today();
    let (reg, source, mut warnings) = load_registry(a.registry.as_deref())?;
    for id in &a.only {
        if reg.get(id).is_none() {
            anyhow::bail!("--only {id}: no such entry in the registry");
        }
    }
    let settings = reg.settings.clone();
    let data = data_dir(&reg);
    let state_path = data.join("state.json");
    let mut st = State::load(&state_path)?;
    let client = match Client::from_env() {
        Ok(c) => Some(c),
        Err(e) => {
            warnings.push(format!(
                "no Lucina API client ({e}): heuristic verdicts, nothing asked"
            ));
            None
        }
    };
    if let Some(c) = &client {
        for line in reconcile_state(c, &reg, &mut st, &data, a.dry_run).await {
            eprintln!("reconcile: {line}");
        }
    }

    let stem = if a.dry_run {
        format!("{date}-dry-run")
    } else {
        date.clone()
    };
    let runs = data.join("runs");
    let report_path = runs.join(format!("{stem}.md"));
    let since_ours = st.last_run.clone().unwrap_or_else(|| {
        (chrono::Local::now() - chrono::Duration::days(7))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });

    let mut checked = Vec::new();
    for up in reg
        .upstream
        .iter()
        .filter(|u| a.only.is_empty() || a.only.contains(&u.id))
    {
        let seen = st.upstream.get(&up.id).and_then(|e| e.last_latest.clone());
        let (up2, since) = (up.clone(), since_ours.clone());
        let (cs, ours) = tokio::task::spawn_blocking(move || {
            let cs = fetch::changes_or_error(&up2, &SystemFetcher, seen.as_deref());
            (cs, fetch::ours_recent(&up2, &SystemFetcher, &since))
        })
        .await?;
        let summary = summary::summarize(&cs);
        let (verdict, fallback) = if cs.is_empty() {
            (None, None)
        } else {
            match ask_jev(client.as_ref(), up, &cs, &summary).await {
                Ok(v) => (Some(v), None),
                Err(why) => (Some(summary::heuristic(up, &cs, &summary)), Some(why)),
            }
        };
        checked.push(Checked {
            up: up.clone(),
            cs,
            summary,
            verdict,
            fallback,
            decision: None,
            ours,
        });
    }

    // Who to ask: security first, then importance, then reliance; at most the cap.
    let mut ask: Vec<usize> = Vec::new();
    for (i, c) in checked.iter_mut().enumerate() {
        let es = st.upstream.get(&c.up.id).cloned().unwrap_or_default();
        let Some(v) = &c.verdict else {
            continue;
        };
        if state::needs_asking(&c.up, &es, &c.cs, v, &settings) {
            ask.push(i);
        } else if state::worth_asking(&es, &c.cs, v, &settings) {
            c.decision = Some("report only (notify = report)".into());
        } else if es.pending() {
            c.decision = Some(format!(
                "pending {}",
                es.decision_id.as_deref().unwrap_or("?")
            ));
        } else if es.notified_latest.is_some() && es.notified_latest == c.cs.latest {
            c.decision = Some(format!(
                "already asked about {}: {}",
                c.cs.latest.as_deref().unwrap_or("?"),
                es.chosen
                    .as_deref()
                    .or(es.decision_status.as_deref())
                    .unwrap_or("?")
            ));
        }
    }
    let sec = settings.security_min;
    ask.sort_by(|&x, &y| {
        let (a, b) = (
            checked[x].verdict.as_ref().unwrap(),
            checked[y].verdict.as_ref().unwrap(),
        );
        (b.security_p >= sec)
            .cmp(&(a.security_p >= sec))
            .then(b.importance.cmp(&a.importance))
            .then(b.relies_p.total_cmp(&a.relies_p))
    });
    let cap = a.max_decisions.unwrap_or(settings.max_decisions_per_run);
    for (n, &i) in ask.iter().enumerate() {
        let c = &mut checked[i];
        let v = c.verdict.clone().unwrap();
        let body = decision::body(
            &c.up,
            &c.cs,
            &c.summary,
            &v,
            &settings,
            &report_path.display().to_string(),
        );
        let rec = body["recommendation"].as_str().unwrap_or("").to_owned();
        if n >= cap {
            c.decision = Some(format!("not asked: over the cap of {cap} per run"));
            continue;
        }
        if a.dry_run {
            c.decision = Some(format!("would ask (dry run), recommending \"{rec}\""));
            continue;
        }
        if a.no_notify {
            c.decision = Some(format!(
                "not asked (--no-notify), would recommend \"{rec}\""
            ));
            continue;
        }
        match ask_owner(client.as_ref(), &body).await {
            Ok(d) => {
                let id = d["decision_id"].as_str().unwrap_or("").to_owned();
                let status = d["status"].as_str().unwrap_or("pending").to_owned();
                c.decision = Some(format!("asked {id} ({status})"));
                let es = st.upstream.entry(c.up.id.clone()).or_default();
                es.decision_id = Some(id);
                es.decision_status = Some(status);
                es.notified_latest = c.cs.latest.clone();
                es.chosen = None;
                es.owner_reply = None;
                es.decided_at = None;
                es.workspace_id = None;
                es.workspace_cwd = None;
                es.brief_path = None;
                es.asked = Some(Asked {
                    changes: c.cs.clone(),
                    summary: c.summary.clone(),
                    verdict: v,
                });
            }
            Err(e) => c.decision = Some(format!("asking failed: {e}")),
        }
    }

    // The reverse direction: one contribution offer per run.
    let mut offered = 0;
    let mut contrib_lines = Vec::new();
    for up in reg
        .upstream
        .iter()
        .filter(|u| a.only.is_empty() || a.only.contains(&u.id))
    {
        for co in &up.contrib {
            let cst = st.contrib.get(&co.id).cloned().unwrap_or_default();
            let mut status = format!("{:?}", co.status).to_lowercase();
            if let Some(id) = &cst.decision_id {
                status.push_str(&format!(
                    "; {} {id}",
                    cst.chosen
                        .as_deref()
                        .or(cst.decision_status.as_deref())
                        .unwrap_or("asked")
                ));
            }
            if let Some(s) = &cst.status {
                status.push_str(&format!("; {s}"));
            }
            if co.status == ContribStatus::Candidate
                && cst.decision_id.is_none()
                && offered < settings.max_contrib_per_run
            {
                offered += 1;
                if a.dry_run || a.no_notify {
                    status.push_str("; would offer");
                } else {
                    match ask_owner(client.as_ref(), &decision::contrib_body(up, co)).await {
                        Ok(d) => {
                            let id = d["decision_id"].as_str().unwrap_or("").to_owned();
                            status.push_str(&format!("; asked {id}"));
                            let cs = st.contrib.entry(co.id.clone()).or_default();
                            cs.decision_id = Some(id);
                            cs.decision_status =
                                Some(d["status"].as_str().unwrap_or("pending").to_owned());
                        }
                        Err(e) => status.push_str(&format!("; asking failed: {e}")),
                    }
                }
            }
            contrib_lines.push(ContribLine {
                upstream: up.name.clone(),
                id: co.id.clone(),
                title: co.title.clone(),
                status,
            });
        }
    }

    let finished = now();
    for c in &checked {
        let es = st.upstream.entry(c.up.id.clone()).or_default();
        es.last_checked = Some(finished.clone());
        if c.cs.error.is_none() && c.cs.skipped.is_none() {
            es.last_latest = c.cs.latest.clone();
            es.last_commits_total = Some(c.cs.commits_total);
        }
    }
    let backlog = st
        .upstream
        .iter()
        .filter(|(_, e)| e.backlog)
        .map(|(id, e)| BacklogItem {
            id: id.clone(),
            latest: e.notified_latest.clone(),
            decided_at: e.decided_at.clone(),
        })
        .collect();
    let run = Run {
        started_at: started,
        finished_at: finished.clone(),
        dry_run: a.dry_run,
        registry: source,
        entries: checked
            .into_iter()
            .map(|c| EntryResult {
                id: c.up.id.clone(),
                name: c.up.name.clone(),
                relation: c.up.relation,
                status: EntryResult::status_of(&c.cs).into(),
                changes: c.cs,
                summary: c.summary,
                verdict: c.verdict,
                fallback: c.fallback,
                decision: c.decision,
                ours_recent: c.ours,
            })
            .collect(),
        backlog,
        contrib: contrib_lines,
        warnings,
    };
    let (_, md_path) = report::save_run(&runs, &stem, &run)?;
    if !a.dry_run {
        st.last_run = Some(finished);
        st.save(&state_path)?;
    }

    for w in &run.warnings {
        eprintln!("warning: {w}");
    }
    for e in &run.entries {
        let verdict = e.verdict.as_ref().map_or(String::new(), |v| {
            format!(
                " | {} {}/4 sec {:.2}",
                v.judged_by, v.importance, v.security_p
            )
        });
        let fb = e.fallback.as_ref().map_or(String::new(), |f| {
            format!(" (JEV unavailable: {})", lucina_upstream::clip(f, 80))
        });
        let dec = e
            .decision
            .as_ref()
            .map_or(String::new(), |d| format!(" | {d}"));
        println!(
            "{:<24} {:<7} {}{verdict}{fb}{dec}",
            e.id, e.status, e.summary.headline
        );
    }
    println!("Report: {}", md_path.display());

    if !a.dry_run {
        let md = report::report_md(&run);
        let record = json!({
            "kind": "status",
            "project": "lucina",
            "title": format!("Upstream watch {date}: {} new, {} asked", run.new_count(), run.asked_count()),
            "body": lucina_upstream::clip(&md, 3000),
            "dedup_key": format!("upstream-watch-{date}"),
        });
        let recorded = async {
            let ops = crate::ops::open_cli(None, None, None)?;
            ops.record(serde_json::from_value(record)?, "upstream-watch")
                .await?;
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = recorded {
            eprintln!("warning: ops record not written: {e:#}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- reconcile

/// `lagent workspace create …` → (workspaceId, cwd).
async fn create_workspace(up: &Upstream, latest: &str) -> anyhow::Result<(String, String)> {
    let ours = up.ours.first().context("no ours")?;
    let branch = format!("port-{}-{}", up.id, chrono::Local::now().format("%Y%m%d"));
    let out = tokio::process::Command::new("lagent")
        .args(["workspace", "create", "--isolation", "worktree", "--path"])
        .arg(&ours.repo)
        .args(["--mode", "branch-off", "--base"])
        .arg(ours.base.as_deref().unwrap_or("origin/main"))
        .arg("--new-branch")
        .arg(&branch)
        .arg("--title")
        .arg(format!("port {} {latest}", up.name))
        .arg("--json")
        .output()
        .await
        .context("lagent workspace create: could not start")?;
    if !out.status.success() {
        anyhow::bail!(
            "lagent workspace create: {}",
            lucina_upstream::clip(String::from_utf8_lossy(&out.stderr).trim(), 300)
        );
    }
    let v: Value = serde_json::from_slice(&out.stdout).context("lagent: not JSON")?;
    let w = if v["workspaceId"].is_string() {
        &v
    } else {
        &v["workspace"]
    };
    let id = w["workspaceId"]
        .as_str()
        .context("lagent: no workspaceId")?;
    let cwd = w["cwd"].as_str().unwrap_or("");
    Ok((id.to_owned(), cwd.to_owned()))
}

/// Answers to pending questions: record them and act ("Port into ours": workspace + brief; a
/// contribution "Draft": the draft plan). `dry_run` records nothing outside `st` and runs nothing.
pub async fn reconcile_state(
    c: &Client,
    reg: &Registry,
    st: &mut State,
    data: &Path,
    dry_run: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    let stamp = chrono::Local::now().format("%Y%m%d").to_string();
    for (id, es) in st.upstream.iter_mut().filter(|(_, e)| e.pending()) {
        let Some(did) = es.decision_id.clone() else {
            continue;
        };
        let d = match c.get(&did).await {
            Ok(d) => d,
            Err(e) => {
                lines.push(format!("{id}: {did}: {e:#}"));
                continue;
            }
        };
        match d["status"].as_str().unwrap_or("") {
            "expired" => {
                es.decision_status = Some("expired".into());
                es.notified_latest = None;
                lines.push(format!(
                    "{id}: {did} expired; the next weekly run may ask again"
                ));
                continue;
            }
            "withdrawn" => {
                es.decision_status = Some("withdrawn".into());
                lines.push(format!("{id}: {did} withdrawn"));
                continue;
            }
            "decided" => {}
            _ => continue,
        }
        let Some(ch) = decision::chosen(&d) else {
            lines.push(format!("{id}: {did} decided, but no option recognised"));
            continue;
        };
        es.decision_status = Some("decided".into());
        es.decided_at = d["decided_at"].as_str().map(str::to_owned);
        es.chosen = Some(ch.word().into());
        match ch {
            Chosen::Later => {
                es.backlog = true;
                lines.push(format!("{id}: Later (backlog)"));
            }
            Chosen::Ignore => {
                es.backlog = false;
                lines.push(format!("{id}: Ignore"));
            }
            Chosen::OwnerReply(text) => {
                lines.push(format!(
                    "{id}: the owner answered in his own words (recorded)"
                ));
                es.owner_reply = Some(text);
            }
            Chosen::Port => {
                es.backlog = false;
                if es.workspace_id.is_some() {
                    continue;
                }
                let Some(up) = reg.get(id) else {
                    lines.push(format!(
                        "{id}: Port, but the entry is no longer in the registry"
                    ));
                    continue;
                };
                if dry_run {
                    lines.push(format!(
                        "{id}: Port: would create a workspace and a brief (dry run)"
                    ));
                    continue;
                }
                let asked = es.asked.clone().unwrap_or_else(|| {
                    let cs = ChangeSet {
                        id: id.clone(),
                        latest: es.notified_latest.clone(),
                        ..ChangeSet::default()
                    };
                    let s = summary::summarize(&cs);
                    Asked {
                        verdict: summary::heuristic(up, &cs, &s),
                        changes: cs,
                        summary: s,
                    }
                });
                let latest = asked
                    .changes
                    .latest
                    .clone()
                    .unwrap_or_else(|| "latest".into());
                let (wid, cwd) = match create_workspace(up, &latest).await {
                    Ok(w) => w,
                    Err(e) => {
                        // Back to pending, so the next pass asks the API again and retries.
                        es.decision_status = Some("pending".into());
                        lines.push(format!("{id}: Port: {e:#} (will retry)"));
                        continue;
                    }
                };
                es.workspace_id = Some(wid.clone());
                es.workspace_cwd = Some(cwd.clone());
                let brief = lucina_upstream::brief::port_brief(
                    up,
                    &asked.changes,
                    &asked.summary,
                    &asked.verdict,
                    &cwd,
                );
                let path = data.join("briefs").join(format!("{id}-{stamp}.md"));
                match state::write_private(&path, brief.as_bytes()) {
                    Ok(()) => es.brief_path = Some(path.display().to_string()),
                    Err(e) => lines.push(format!("{id}: brief not written: {e:#}")),
                }
                lines.push(format!(
                    "{id}: Port: workspace {wid} at {cwd}, brief {}",
                    path.display()
                ));
                if reg.settings.auto_run {
                    if let Some(bp) = &es.brief_path {
                        let r = tokio::process::Command::new("lagent")
                            .args(["run", "--workspace", &wid, "--provider", "claude", "--no-wait"])
                            .arg("--title")
                            .arg(format!("port {}", up.name))
                            .arg(format!("Read {bp} and do what it says, following docs/porting-guidelines.md."))
                            .output()
                            .await;
                        match r {
                            Ok(o) if o.status.success() => {
                                lines.push(format!("{id}: agent started"))
                            }
                            Ok(o) => lines.push(format!(
                                "{id}: lagent run failed: {}",
                                lucina_upstream::clip(
                                    String::from_utf8_lossy(&o.stderr).trim(),
                                    200
                                )
                            )),
                            Err(e) => lines.push(format!("{id}: lagent run: {e}")),
                        }
                    }
                }
            }
        }
    }

    for (cid, cs) in st.contrib.iter_mut().filter(|(_, c)| c.pending()) {
        let Some(did) = cs.decision_id.clone() else {
            continue;
        };
        let d = match c.get(&did).await {
            Ok(d) => d,
            Err(e) => {
                lines.push(format!("{cid}: {did}: {e:#}"));
                continue;
            }
        };
        match d["status"].as_str().unwrap_or("") {
            "decided" => {}
            s @ ("expired" | "withdrawn") => {
                cs.decision_status = Some(s.into());
                continue;
            }
            _ => continue,
        }
        let Some(ch) = decision::chosen(&d) else {
            continue;
        };
        cs.decision_status = Some("decided".into());
        cs.decided_at = d["decided_at"].as_str().map(str::to_owned);
        match ch {
            Chosen::Port => {
                cs.chosen = Some("draft".into());
                let found = reg
                    .upstream
                    .iter()
                    .find_map(|u| u.contrib.iter().find(|x| &x.id == cid).map(|x| (u, x)));
                let Some((up, co)) = found else {
                    lines.push(format!("{cid}: Draft, but it is no longer in the registry"));
                    continue;
                };
                if dry_run {
                    lines.push(format!("{cid}: Draft: would write the plan (dry run)"));
                    continue;
                }
                let path = data.join("contrib").join(format!("{cid}.md"));
                match state::write_private(
                    &path,
                    lucina_upstream::brief::contrib_brief(up, co).as_bytes(),
                ) {
                    Ok(()) => {
                        cs.status = Some("drafting".into());
                        cs.brief_path = Some(path.display().to_string());
                        lines.push(format!("{cid}: Draft plan at {}", path.display()));
                    }
                    Err(e) => lines.push(format!("{cid}: draft not written: {e:#}")),
                }
            }
            Chosen::Later => cs.chosen = Some("later".into()),
            Chosen::Ignore => cs.chosen = Some("never".into()),
            Chosen::OwnerReply(t) => {
                cs.chosen = Some("reply".into());
                cs.owner_reply = Some(t);
            }
        }
    }
    lines
}

pub async fn reconcile(registry: Option<PathBuf>, dry_run: bool) -> anyhow::Result<()> {
    let (reg, _, warnings) = load_registry(registry.as_deref())?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let data = data_dir(&reg);
    let path = data.join("state.json");
    let mut st = State::load(&path)?;
    let open =
        st.upstream.values().any(|e| e.pending()) || st.contrib.values().any(|c| c.pending());
    if !open {
        println!("nothing pending");
        return Ok(());
    }
    let c = Client::from_env()?;
    let lines = reconcile_state(&c, &reg, &mut st, &data, dry_run).await;
    if !dry_run {
        st.save(&path)?;
    }
    for l in &lines {
        println!("{l}");
    }
    if lines.is_empty() {
        println!("no answers yet");
    }
    Ok(())
}

// ------------------------------------------------------------ status, validate

pub fn status(registry: Option<PathBuf>, as_json: bool) -> anyhow::Result<()> {
    let (reg, _, _) = load_registry(registry.as_deref())?;
    let st = State::load(&data_dir(&reg).join("state.json"))?;
    let rows: Vec<Value> = reg
        .upstream
        .iter()
        .map(|u| {
            let e = st.upstream.get(&u.id).cloned().unwrap_or_default();
            json!({
                "id": u.id,
                "source": format!("{:?}", u.source).to_lowercase(),
                "pin": u.pin.clone().or_else(|| u.pin_date.clone()),
                "last_latest": e.last_latest,
                "last_checked": e.last_checked,
                "decision_id": e.decision_id,
                "decision_status": e.decision_status,
                "chosen": e.chosen,
                "backlog": e.backlog,
                "workspace_id": e.workspace_id,
                "brief_path": e.brief_path,
            })
        })
        .collect();
    if as_json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let s = |v: &Value| match v {
        Value::Null => "-".to_owned(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    println!(
        "{:<26} {:<7} {:<16} {:<16} {:<11} {:<20} {:<7} WORKSPACE",
        "ID", "SOURCE", "PIN", "LATEST", "CHECKED", "DECISION", "BACKLOG"
    );
    for r in &rows {
        let short = |v: &Value, n: usize| lucina_upstream::clip(&s(v), n);
        let decision = match (s(&r["decision_status"]).as_str(), s(&r["chosen"])) {
            ("-", _) => "-".to_owned(),
            (st, ch) if ch != "-" => format!("{st}/{ch}"),
            (st, _) => st.to_owned(),
        };
        println!(
            "{:<26} {:<7} {:<16} {:<16} {:<11} {:<20} {:<7} {}",
            short(&r["id"], 26),
            s(&r["source"]),
            short(&r["pin"], 16),
            short(&r["last_latest"], 16),
            s(&r["last_checked"]).chars().take(10).collect::<String>(),
            decision,
            if r["backlog"] == true { "yes" } else { "-" },
            s(&r["workspace_id"])
        );
    }
    Ok(())
}

pub fn validate(registry: Option<PathBuf>) -> anyhow::Result<()> {
    let (text, source) = match &registry {
        Some(p) => (
            std::fs::read_to_string(p).with_context(|| format!("{}", p.display()))?,
            p.display().to_string(),
        ),
        None => {
            let (reg, source, warnings) = load_registry(None)?;
            for w in warnings {
                println!("warning: {w}");
            }
            println!("{source}: {} entries, valid", reg.upstream.len());
            return Ok(());
        }
    };
    let reg = Registry::parse(&text).with_context(|| format!("registry {source}"))?;
    let problems = reg.validate();
    for p in &problems {
        println!("{p}");
    }
    let errors = problems.iter().filter(|p| p.level == Level::Error).count();
    if errors > 0 {
        anyhow::bail!("{source}: {errors} errors");
    }
    println!(
        "{source}: {} entries, {} contributions, valid ({} warnings)",
        reg.upstream.len(),
        reg.upstream.iter().map(|u| u.contrib.len()).sum::<usize>(),
        problems.len()
    );
    Ok(())
}
