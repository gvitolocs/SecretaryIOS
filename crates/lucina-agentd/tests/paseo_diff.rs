//! Differential tests: the same scenarios through Paseo 0.13.2 and through
//! lucina-agentd, compared after normalizing ids, times and the cwd. They run only
//! with `PASEO_DIFF=1`, against a separate development Paseo on 127.0.0.1:6777 with
//! a fresh home under `~/scratch/paseo-diff/` (never the production daemon on 6767),
//! which is stopped at the end, also on failure.
//!
//! Intentional differences (not compared):
//!
//! | What | Paseo | Lucina |
//! |---|---|---|
//! | `run` without `--workspace` | creates a workspace and prints "Created workspace ..." on stderr | no workspace yet (phase 4) |
//! | JSON key order | insertion order | sorted (values are compared, not text) |
//! | Streamed text length | depends on timer ticks | same, so only prefixes of the scripted text are compared |
//! | `OutputTokens` | counts emitted tokens, timing-dependent | same; only the field set is compared |
//! | timeline after restart | rebuilt from provider history, new epoch | stored, same epoch (`daemon.rs`) |
//! | error details | `Use "paseo ls" ...` | `Use "lagent ls" ...` |

mod common;

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{run, Daemon, Guard, Run};
use serde_json::{json, Value};

const PASEO: &str = "/home/nez/.local/bin/paseo";
const PORT: u16 = 6777;

struct Paseo {
    home: PathBuf,
    guard: Option<Guard>,
}

impl Paseo {
    fn start() -> Paseo {
        let base = PathBuf::from(std::env::var("HOME").expect("HOME")).join("scratch/paseo-diff");
        let home = base.join(format!("run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("paseo home");
        let config = json!({
            "version": 1,
            "daemon": {"listen": format!("127.0.0.1:{PORT}"), "mcp": {"enabled": false}, "relay": {"enabled": false}},
            "features": {"webUi": {"enabled": false}}
        });
        std::fs::write(home.join("config.json"), config.to_string()).unwrap();
        let log = std::fs::File::create(base.join("last-run.log")).unwrap();
        let child = Command::new(PASEO)
            .args(["daemon", "run", "--home", &home.display().to_string()])
            .env("PASEO_NODE_ENV", "development")
            .current_dir("/tmp")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .process_group(0)
            .spawn()
            .expect("spawn paseo");
        let paseo = Paseo { home, guard: Some(Guard { child: Some(child) }) };
        // /api/health answers before the client WebSocket is ready: wait for `ls`.
        let deadline = Instant::now() + Duration::from_secs(60);
        while paseo.cmd(&["ls", "--json"]).code != 0 {
            assert!(Instant::now() < deadline, "dev Paseo did not come up on {PORT}");
            std::thread::sleep(Duration::from_millis(250));
        }
        paseo
    }

    fn cmd(&self, args: &[&str]) -> Run {
        let mut all = vec!["--host", "127.0.0.1:6777"];
        all.extend_from_slice(args);
        run(Command::new(PASEO).args(&all).current_dir("/tmp"))
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut a = args.to_vec();
        a.push("--json");
        let r = self.cmd(&a);
        assert_eq!(r.code, 0, "paseo {args:?} failed: {}", r.stderr);
        serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("paseo {args:?}: {e}: {}", r.stdout))
    }
}

impl Drop for Paseo {
    fn drop(&mut self) {
        // Stop the dev daemon's process group first, then remove its home.
        drop(self.guard.take());
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Both sides of one comparison, printed on a mismatch.
fn same(what: &str, paseo: &Value, lucina: &Value) {
    assert_eq!(paseo, lucina, "{what} differs\n--- paseo\n{paseo:#}\n--- lucina\n{lucina:#}");
}

fn pick(v: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().map(|k| (k.to_string(), v[*k].clone())).collect())
}

fn keys(v: &Value) -> Value {
    let mut k: Vec<String> = v.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    k.sort();
    json!(k)
}

fn id(v: &Value) -> String {
    v["agentId"].as_str().expect("agentId").to_string()
}

/// `["[User] ...", <first text line>, ..., <last line>]` and the body between.
fn log_shape(text: &str) -> (Vec<String>, String) {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let first = lines.first().map(|s| s.to_string()).unwrap_or_default();
    let second = lines.get(1).map(|s| s.to_string()).unwrap_or_default();
    let last = lines.last().map(|s| s.to_string()).unwrap_or_default();
    let body = lines.get(1..lines.len().saturating_sub(1)).map(|b| b.join("\n").trim_end().to_string()).unwrap_or_default();
    (vec![first, second, last], body)
}

fn user_lines(text: &str) -> Vec<String> {
    text.lines().filter(|l| l.starts_with("[User]")).map(str::to_string).collect()
}

fn scenario_run_logs_inspect(p: &Paseo, l: &Daemon) {
    let args = ["run", "--provider", "mock/e2e-fast-stream", "--title", "t1", "--cwd", "/tmp", "hello"];
    let (pr, lr) = (p.json(&args), l.json(&args));
    same("run result", &pick(&pr, &["status", "provider", "cwd", "title"]), &pick(&lr, &["status", "provider", "cwd", "title"]));
    same("run result keys", &keys(&pr), &keys(&lr));

    let (plog, llog) = (p.cmd(&["logs", &id(&pr)]).stdout, l.lagent(&["logs", &id(&lr)]).stdout);
    let ((pshape, pbody), (lshape, lbody)) = (log_shape(&plog), log_shape(&llog));
    same("logs first/second/last line", &json!(pshape), &json!(lshape));
    let (short, long) = if pbody.len() <= lbody.len() { (&pbody, &lbody) } else { (&lbody, &pbody) };
    assert!(long.starts_with(short.as_str()), "streamed text diverges\n--- paseo\n{pbody}\n--- lucina\n{lbody}");

    let (pi, li) = (p.json(&["inspect", &id(&pr)]), l.json(&["inspect", &id(&lr)]));
    let compared = ["Name", "Provider", "Model", "Thinking", "Status", "Archived", "ArchivedAt", "Mode", "Capabilities", "AvailableModes", "PendingPermissions", "Worktree", "ParentAgentId"];
    same("inspect", &pick(&pi, &compared), &pick(&li, &compared));
    same("inspect keys", &keys(&pi), &keys(&li));
    same("inspect LastUsage keys", &keys(&pi["LastUsage"]), &keys(&li["LastUsage"]));
    same("inspect LastUsage fixed fields", &pick(&pi["LastUsage"], &["InputTokens", "CachedTokens", "CostUsd"]), &pick(&li["LastUsage"], &["InputTokens", "CachedTokens", "CostUsd"]));

    let pls: Vec<Value> = p.json(&["ls"]).as_array().cloned().unwrap_or_default();
    let lls: Vec<Value> = l.json(&["ls"]).as_array().cloned().unwrap_or_default();
    let find = |list: &[Value], want: &str| list.iter().find(|a| a["id"] == want).cloned().expect("agent in ls");
    let ls_keys = ["name", "provider", "thinking", "status", "cwd", "created"];
    same("ls item", &pick(&find(&pls, &id(&pr)), &ls_keys), &pick(&find(&lls, &id(&lr)), &ls_keys));
    same("ls item keys", &keys(&find(&pls, &id(&pr))), &keys(&find(&lls, &id(&lr))));
}

fn scenario_turn_failure(p: &Paseo, l: &Daemon) {
    let args = ["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic turn failure"];
    let (pr, lr) = (p.json(&args), l.json(&args));
    same("failed run", &pick(&pr, &["status", "provider", "cwd", "title"]), &pick(&lr, &["status", "provider", "cwd", "title"]));
    let (plog, llog) = (p.cmd(&["logs", &id(&pr)]), l.lagent(&["logs", &id(&lr)]));
    same("failed run logs", &json!(plog.stdout), &json!(llog.stdout));
    same("failed run status", &p.json(&["inspect", &id(&pr)])["Status"], &l.json(&["inspect", &id(&lr)])["Status"]);
}

fn scenario_plan_approval_stop(p: &Paseo, l: &Daemon) {
    let args = ["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic plan approval"];
    let (pr, lr) = (p.json(&args), l.json(&args));
    same("plan run", &pick(&pr, &["status", "title"]), &pick(&lr, &["status", "title"]));
    let norm_pending = |v: &Value| {
        let list = v["PendingPermissions"].as_array().cloned().unwrap_or_default();
        json!(list.iter().map(|x| json!({"idPrefix": x["id"].as_str().unwrap_or("").starts_with("mock-plan-"), "tool": x["tool"]})).collect::<Vec<_>>())
    };
    same("pending permission", &norm_pending(&p.json(&["inspect", &id(&pr)])), &norm_pending(&l.json(&["inspect", &id(&lr)])));
    let (ps, ls) = (p.json(&["stop", &id(&pr)]), l.json(&["stop", &id(&lr)]));
    same("stop", &json!({"stoppedCount": ps["stoppedCount"], "n": ps["agentIds"].as_array().map(Vec::len)}), &json!({"stoppedCount": ls["stoppedCount"], "n": ls["agentIds"].as_array().map(Vec::len)}));
    let (pi, li) = (p.json(&["inspect", &id(&pr)]), l.json(&["inspect", &id(&lr)]));
    same("after stop", &pick(&pi, &["Status", "PendingPermissions"]), &pick(&li, &["Status", "PendingPermissions"]));
    same("plan logs", &json!(p.cmd(&["logs", &id(&pr)]).stdout), &json!(l.lagent(&["logs", &id(&lr)]).stdout));
}

fn scenario_send_wait(p: &Paseo, l: &Daemon) {
    let args = ["run", "--provider", "mock/ten-second-stream", "--cwd", "/tmp", "--no-wait", "first task"];
    let (pr, lr) = (p.json(&args), l.json(&args));
    same("no-wait run", &pick(&pr, &["status", "title"]), &pick(&lr, &["status", "title"]));
    std::thread::sleep(Duration::from_millis(500));
    let (ps, ls) = (p.json(&["send", &id(&pr), "second task"]), l.json(&["send", &id(&lr), "second task"]));
    same("send", &pick(&ps, &["status", "message"]), &pick(&ls, &["status", "message"]));
    let (pw, lw) = (p.json(&["wait", &id(&pr)]), l.json(&["wait", &id(&lr)]));
    same("wait status", &pw["status"], &lw["status"]);
    let head = |v: &Value| v["message"].as_str().unwrap_or("").lines().take(2).collect::<Vec<_>>().join("\n");
    same("wait message head", &json!(head(&pw)), &json!(head(&lw)));
    let (plog, llog) = (p.cmd(&["logs", &id(&pr)]).stdout, l.lagent(&["logs", &id(&lr)]).stdout);
    same("user prompts", &json!(user_lines(&plog)), &json!(user_lines(&llog)));
}

fn scenario_archive_delete(p: &Paseo, l: &Daemon) {
    let args = ["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "to archive"];
    let (pr, lr) = (p.json(&args), l.json(&args));
    let (pa, la) = (p.json(&["archive", &id(&pr)]), l.json(&["archive", &id(&lr)]));
    same("archive", &json!({"status": pa["status"], "keys": keys(&pa)}), &json!({"status": la["status"], "keys": keys(&la)}));
    let listed = |list: Value, want: &str| list.as_array().is_some_and(|a| a.iter().any(|x| x["id"] == want));
    same("ls hides archived", &json!(listed(p.json(&["ls"]), &id(&pr))), &json!(listed(l.json(&["ls"]), &id(&lr))));
    same("ls -a shows archived", &json!(listed(p.json(&["ls", "-a"]), &id(&pr))), &json!(listed(l.json(&["ls", "-a"]), &id(&lr))));
    let (pd, ld) = (p.json(&["delete", &id(&pr)]), l.json(&["delete", &id(&lr)]));
    same("delete", &json!({"deletedCount": pd["deletedCount"], "keys": keys(&pd)}), &json!({"deletedCount": ld["deletedCount"], "keys": keys(&ld)}));
    for (what, pargs, largs) in [
        ("inspect deleted", vec!["inspect".to_string(), id(&pr)], vec!["inspect".to_string(), id(&lr)]),
        ("archive unknown", vec!["archive".into(), "nope".into()], vec!["archive".into(), "nope".into()]),
    ] {
        let mut pa: Vec<&str> = pargs.iter().map(String::as_str).collect();
        pa.push("--json");
        let mut la: Vec<&str> = largs.iter().map(String::as_str).collect();
        la.push("--json");
        let (pe, le) = (p.cmd(&pa), l.lagent(&la));
        same(&format!("{what} exit"), &json!(pe.code), &json!(le.code));
        let perr: Value = serde_json::from_str(&pe.stderr).unwrap_or(Value::Null);
        let lerr: Value = serde_json::from_str(&le.stderr).unwrap_or(Value::Null);
        let norm = |e: &Value, id: &str| json!({"code": e["error"]["code"], "message": e["error"]["message"].as_str().unwrap_or("").replace(id, "<id>")});
        same(&format!("{what} error"), &norm(&perr, &pargs[1]), &norm(&lerr, &largs[1]));
    }
}

#[test]
fn paseo_parity() {
    if std::env::var("PASEO_DIFF").as_deref() != Ok("1") {
        eprintln!("paseo_parity: skipped (set PASEO_DIFF=1 to compare against a dev Paseo on 127.0.0.1:{PORT})");
        return;
    }
    let paseo = Paseo::start();
    let lucina = Daemon::start();
    type Scenario = fn(&Paseo, &Daemon);
    let scenarios: [(&str, Scenario); 5] = [
        ("a run/logs/inspect/ls", scenario_run_logs_inspect),
        ("b turn failure", scenario_turn_failure),
        ("c plan approval + stop", scenario_plan_approval_stop),
        ("d send --interrupt + wait", scenario_send_wait),
        ("e archive/ls/delete/not found", scenario_archive_delete),
    ];
    for (name, scenario) in scenarios {
        let start = Instant::now();
        scenario(&paseo, &lucina);
        eprintln!("paseo_parity: {name}: same ({:.1}s)", start.elapsed().as_secs_f32());
    }
}
