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
//! | worktree path | `<paseo home>/worktrees/<hash>/<slug>` | `~/Projects/.worktrees/<Repo>/<branch>` (`wt add`); only the last part is compared |
//! | archived worktree's branch | kept | deleted when merged (`git branch -d`, the owner's rule) |
//! | workspace ids, project ids, file mtimes | random, times | not compared; `ls` entries are compared sorted (Paseo lists in directory order) |
//! | terminal env | `PASEO_HOME`; `PASEO_TERMINAL_ACTIVITY_URL` on its HTTP port | no home; the URL only with `--listen` (`lagent hooks` uses the socket) |
//! | plain script env | the terminal's only | also `PASEO_SCRIPTNAME` and setup's worktree variables |
//! | a plain script's terminal | stays open as a shell | ends with the script (its output stays capturable) |
//! | `send-keys Up` (CLI) | types the word `Up` | the arrow key (the MCP encoder, as Paseo's MCP tool) |
//! | rewind (mock) | keeps only the first user message, whatever the target; an unknown target fails inside the provider | rewinds to the target: the rows before it stay; an unknown target is `REWIND_TARGET_NOT_FOUND` before anything runs |

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

/// The error of a failing `--json` command: exit code, error code and message (ids
/// replaced), not the details (they name the CLI).
fn error_of(r: &Run, id: &str) -> Value {
    let e: Value = serde_json::from_str(&r.stderr).unwrap_or(Value::Null);
    json!({"exit": r.code, "code": e["error"]["code"], "message": e["error"]["message"].as_str().unwrap_or("").replace(id, "<id>")})
}

fn scenario_permit(p: &Paseo, l: &Daemon) {
    let args = ["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic plan approval"];
    let (pr, lr) = (p.json(&args), l.json(&args));
    let norm_ls = |v: &Value, id: &str| {
        json!(v.as_array().cloned().unwrap_or_default().iter().filter(|x| x["agentId"] == id).map(|x| json!({
            "keys": keys(x), "name": x["name"], "description": x["description"],
            "short": x["agentShortId"] == id[..7], "plan": x["id"].as_str().unwrap_or("").starts_with("mock-plan-"),
        })).collect::<Vec<_>>())
    };
    same("permit ls", &norm_ls(&p.json(&["permit", "ls"]), &id(&pr)), &norm_ls(&l.json(&["permit", "ls"]), &id(&lr)));
    let (pe, le) = (p.cmd(&["permit", "deny", &id(&pr), "--json"]), l.lagent(&["permit", "deny", &id(&lr), "--json"]));
    same("deny without a request id", &error_of(&pe, &id(&pr)), &error_of(&le, &id(&lr)));
    let (pe, le) = (p.cmd(&["permit", "allow", &id(&pr), "nope", "--json"]), l.lagent(&["permit", "allow", &id(&lr), "nope", "--json"]));
    same("allow an unknown request", &error_of(&pe, &id(&pr)), &error_of(&le, &id(&lr)));
    let norm_answer = |v: &Value, id: &str| {
        json!(v.as_array().cloned().unwrap_or_default().iter().map(|x| json!({
            "keys": keys(x), "name": x["name"], "result": x["result"], "agent": x["agentId"] == id, "short": x["agentShortId"] == id[..7],
        })).collect::<Vec<_>>())
    };
    let (pa, la) = (p.json(&["permit", "allow", &id(&pr)]), l.json(&["permit", "allow", &id(&lr)]));
    same("permit allow", &norm_answer(&pa, &id(&pr)), &norm_answer(&la, &id(&lr)));
    let (pw, lw) = (p.json(&["wait", &id(&pr)]), l.json(&["wait", &id(&lr)]));
    same("wait after allow", &pw["status"], &lw["status"]);
    let (pi, li) = (p.json(&["inspect", &id(&pr)]), l.json(&["inspect", &id(&lr)]));
    same("inspect after allow", &pick(&pi, &["Status", "PendingPermissions"]), &pick(&li, &["Status", "PendingPermissions"]));
    let short = |s: &str| s[..7].to_string();
    let (pe, le) = (p.cmd(&["permit", "allow", &id(&pr), "--json"]), l.lagent(&["permit", "allow", &id(&lr), "--json"]));
    same("allow with nothing pending", &error_of(&pe, &short(&id(&pr))), &error_of(&le, &short(&id(&lr))));

    let args = ["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic questions"];
    let (pq, lq) = (p.json(&args), l.json(&args));
    let req = |v: &Value| v["PendingPermissions"][0]["id"].as_str().unwrap_or("").to_string();
    let (preq, lreq) = (req(&p.json(&["inspect", &id(&pq)])), req(&l.json(&["inspect", &id(&lq)])));
    let (pd, ld) = (
        p.json(&["permit", "deny", &id(&pq), &preq[..14], "--message", "not now"]),
        l.json(&["permit", "deny", &id(&lq), &lreq[..14], "--message", "not now"]),
    );
    same("permit deny", &norm_answer(&pd, &id(&pq)), &norm_answer(&ld, &id(&lq)));
    let (pw, lw) = (p.json(&["wait", &id(&pq)]), l.json(&["wait", &id(&lq)]));
    same("wait after deny", &pw["status"], &lw["status"]);
    same("question logs", &json!(p.cmd(&["logs", &id(&pq)]).stdout), &json!(l.lagent(&["logs", &id(&lq)]).stdout));
    same("permit ls empty", &norm_ls(&p.json(&["permit", "ls"]), &id(&pq)), &norm_ls(&l.json(&["permit", "ls"]), &id(&lq)));
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git").arg("-C").arg(dir).args(args).output().map(|o| o.status.success()).unwrap_or(false);
    assert!(ok, "git {args:?}");
}

/// Paseo's answer through its own client (`tests/paseo_oracle.mjs`).
fn oracle(op: &str, cwd: &str, arg: Option<&str>) -> Value {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/paseo_oracle.mjs");
    let mut cmd = Command::new("node");
    cmd.args([script, "127.0.0.1:6777", op, cwd]);
    if let Some(a) = arg {
        cmd.arg(a);
    }
    let r = run(&mut cmd);
    serde_json::from_str(r.stdout.trim()).unwrap_or_else(|e| panic!("oracle {op}: {e}: {} {}", r.stdout, r.stderr))
}

/// Paseo's answer about an agent through its own client (`paseo_oracle.mjs`: timeline,
/// fork-context, rewind).
fn agent_oracle(op: &str, agent: &str, arg: Option<&str>, arg2: Option<&str>) -> Value {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/paseo_oracle.mjs");
    let mut cmd = Command::new("node");
    cmd.args([script, "127.0.0.1:6777", op, agent]);
    if arg.is_some() || arg2.is_some() {
        cmd.arg(arg.unwrap_or(""));
    }
    if let Some(a) = arg2 {
        cmd.arg(a);
    }
    let r = run(&mut cmd);
    serde_json::from_str(r.stdout.trim()).unwrap_or_else(|e| panic!("oracle {op}: {e}: {} {}", r.stdout, r.stderr))
}

/// A local HTTP request on the lucina socket: status and JSON body.
fn lucina_call(l: &Daemon, method: &str, path: &str, body: &Value) -> (u16, Value) {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(l.socket()).unwrap();
    let body = body.to_string();
    write!(s, "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, rest) = raw.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let text = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        rest.split_once("\r\n").map(|(_, r)| r.rsplit_once("\r\n0\r\n").map(|(b, _)| b).unwrap_or(r)).unwrap_or("")
    } else {
        rest
    };
    (status, serde_json::from_str(text.trim()).unwrap_or(Value::Null))
}

/// A local HTTP GET on the lucina socket.
fn lucina_get(l: &Daemon, path: &str) -> Value {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(l.socket()).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, rest) = raw.split_once("\r\n\r\n").unwrap();
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        rest.split_once("\r\n").map(|(_, r)| r.rsplit_once("\r\n0\r\n").map(|(b, _)| b).unwrap_or(r)).unwrap_or("")
    } else {
        rest
    };
    serde_json::from_str(body.trim()).unwrap_or(Value::Null)
}

/// One repo per side (same content, `<root>/<side>/repo`), so neither daemon sees the
/// other's branches; paths are compared with the side's root replaced by `<root>`.
fn scenario_workspaces(p: &Paseo, l: &Daemon) {
    let root = tempfile::Builder::new().prefix("ldiff-ws.").tempdir_in("/tmp").unwrap();
    let mk = |side: &str| {
        let r = root.path().join(side).join("repo");
        std::fs::create_dir_all(r.join("d")).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("f.txt"), "a\nb\nc\n").unwrap();
        std::fs::write(r.join("old.txt"), "keep\n").unwrap();
        std::fs::write(r.join("d/y.txt"), "x\n").unwrap();
        git(&r, &["add", "."]);
        git(&r, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        git(&r, &["branch", "existing"]);
        r
    };
    let (prr, lrr) = (mk("paseo"), mk("lucina"));
    let (rp, rl) = (prr.display().to_string(), lrr.display().to_string());
    let norm = |v: &Value, side: &str| -> Value {
        let text = v.to_string().replace(&root.path().join(side).display().to_string(), "<root>");
        serde_json::from_str(&text).unwrap()
    };
    let both = |pa: Vec<&str>, la: Vec<&str>| (p.json(&pa), l.json(&la));

    let (pp, lp) = both(vec!["project", "create", &rp], vec!["project", "create", &rl]);
    same("project create", &norm(&pick(&pp, &["name", "kind", "path"]), "paseo"), &norm(&pick(&lp, &["name", "kind", "path"]), "lucina"));
    let (pl, ll) = both(vec!["workspace", "create", "--isolation", "local", "--path", &rp], vec!["workspace", "create", "--isolation", "local", "--path", &rl]);
    let row = |v: &Value, side: &str| norm(&pick(v, &["project", "name", "isolation", "cwd", "background"]), side);
    same("workspace create local", &row(&pl, "paseo"), &row(&ll, "lucina"));
    let wt = |r: &str| vec!["workspace".to_string(), "create".into(), "--isolation".into(), "worktree".into(), "--path".into(), r.to_string(), "--new-branch".into(), "feat/y".into(), "--base".into(), "main".into()];
    let (pwa, lwa) = (wt(&rp), wt(&rl));
    let (pw, lw) = both(pwa.iter().map(String::as_str).collect(), lwa.iter().map(String::as_str).collect());
    same("workspace create worktree", &pick(&pw, &["project", "name", "isolation", "background"]), &pick(&lw, &["project", "name", "isolation", "background"]));
    for (what, v) in [("paseo", &pw), ("lucina", &lw)] {
        assert!(v["cwd"].as_str().unwrap_or("").ends_with("/feat-y"), "{what} worktree path: {v}");
    }
    let cb = |r: &str| vec!["workspace".to_string(), "create".into(), "--isolation".into(), "worktree".into(), "--path".into(), r.to_string(), "--mode".into(), "checkout-branch".into(), "--branch".into(), "existing".into()];
    let (pca, lca) = (cb(&rp), cb(&rl));
    let (pc, lc) = both(pca.iter().map(String::as_str).collect(), lca.iter().map(String::as_str).collect());
    same("checkout-branch", &pick(&pc, &["name", "isolation"]), &pick(&lc, &["name", "isolation"]));
    let listing = |v: Value| {
        let mut rows: Vec<Value> = v.as_array().cloned().unwrap_or_default().iter().map(|w| pick(w, &["project", "name", "isolation", "background"])).collect();
        rows.sort_by_key(|x| x.to_string());
        json!(rows)
    };
    same("workspace ls", &listing(p.json(&["workspace", "ls"])), &listing(l.json(&["workspace", "ls"])));

    let (pid, lid) = (pw["workspaceId"].as_str().unwrap().to_string(), lw["workspaceId"].as_str().unwrap().to_string());
    same("rename", &p.json(&["workspace", "rename", &pid, "My title"])["title"], &l.json(&["workspace", "rename", &lid, "My title"])["title"]);
    same("rename --reset", &p.json(&["workspace", "rename", &pid, "--reset"])["title"], &l.json(&["workspace", "rename", &lid, "--reset"])["title"]);
    let errors = |r: &str| -> Vec<(&'static str, Vec<String>)> {
        let v = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        vec![
            ("rename unknown", v(&["workspace", "rename", "nope", "x"])),
            ("archive unknown", v(&["workspace", "archive", "nope"])),
            ("unknown branch", v(&["workspace", "create", "--isolation", "worktree", "--path", r, "--mode", "checkout-branch", "--branch", "nope"])),
            ("checkout-branch without --branch", v(&["workspace", "create", "--isolation", "worktree", "--path", r, "--mode", "checkout-branch"])),
            ("local with worktree options", v(&["workspace", "create", "--isolation", "local", "--path", r, "--base", "main"])),
            ("worktree outside git", v(&["workspace", "create", "--isolation", "worktree", "--path", "/tmp"])),
        ]
    };
    for ((what, pa), (_, la)) in errors(&rp).into_iter().zip(errors(&rl)) {
        let mut pa: Vec<&str> = pa.iter().map(String::as_str).collect();
        let mut la: Vec<&str> = la.iter().map(String::as_str).collect();
        pa.push("--json");
        la.push("--json");
        same(what, &error_of(&p.cmd(&pa), "nope"), &error_of(&l.lagent(&la), "nope"));
    }

    // A bare run makes a local workspace for its cwd (stderr), then runs there.
    let (pr, lr) = (
        p.cmd(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", &rp, "hello", "--json"]),
        l.lagent(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", &rl, "hello", "--json"]),
    );
    let head = |s: &str| s.lines().next().map(|x| x.split_once(" - ").map(|(_, label)| label.to_string()).unwrap_or_default()).unwrap_or_default();
    same("bare run stderr label", &json!(head(&pr.stderr)), &json!(head(&lr.stderr)));
    let (pv, lv): (Value, Value) = (serde_json::from_str(&pr.stdout).unwrap(), serde_json::from_str(&lr.stdout).unwrap());
    same("bare run", &norm(&pick(&pv, &["status", "provider", "cwd", "title"]), "paseo"), &norm(&pick(&lv, &["status", "provider", "cwd", "title"]), "lucina"));

    // Diffs, status and the file explorer, against Paseo's own client.
    for r in [&prr, &lrr] {
        git(r, &["checkout", "-q", "-b", "work"]);
        std::fs::write(r.join("f.txt"), "a\nB\nc\nd\n").unwrap();
        git(r, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "two"]);
        std::fs::write(r.join("f.txt"), "a\nB\nc\nd\ne\n").unwrap();
        std::fs::write(r.join("n.txt"), "new\n").unwrap();
        git(r, &["rm", "-q", "old.txt"]);
        std::fs::write(r.join("bin.dat"), [0u8, 1]).unwrap();
    }
    let local_id = ll["workspaceId"].as_str().unwrap();
    for mode in ["uncommitted", "base", "all"] {
        let pd = oracle(&format!("diff-{mode}"), &rp, None);
        let ld = lucina_get(l, &format!("/workspaces/{local_id}/diff?mode={mode}"));
        same(&format!("diff {mode}"), &pd["files"], &ld["files"]);
    }
    let keys = ["isGit", "currentBranch", "isDirty", "baseRef", "aheadBehind", "hasRemote", "remoteUrl", "upstreamRef", "aheadOfOrigin", "mainRepoRoot", "repoRoot"];
    same("checkout status", &norm(&pick(&oracle("status", &rp, None), &keys), "paseo"), &norm(&pick(&lucina_get(l, &format!("/workspaces/{local_id}/status")), &keys), "lucina"));
    let entries = |v: &Value| {
        let mut e: Vec<Value> = v["entries"].as_array().cloned().unwrap_or_default().iter().map(|x| pick(x, &["name", "path", "kind", "size"])).collect();
        e.sort_by_key(|x| x["name"].to_string());
        json!(e)
    };
    same("files .", &entries(&oracle("ls", &rp, Some("."))), &entries(&lucina_get(l, &format!("/workspaces/{local_id}/files?path=."))));
    same("files d", &entries(&oracle("ls", &rp, Some("d"))), &entries(&lucina_get(l, &format!("/workspaces/{local_id}/files?path=d"))));
    let pf = oracle("read", &rp, Some("f.txt"));
    let lf = lucina_get(l, &format!("/workspaces/{local_id}/file?path=f.txt"));
    same("read f.txt", &json!({"path": pf["path"], "kind": pf["kind"], "size": pf["size"], "text": pf["text"]}), &json!({"path": lf["path"], "kind": lf["kind"], "size": lf["size"], "text": lf["content"]}));
    same("read bin.dat kind", &oracle("read", &rp, Some("bin.dat"))["kind"], &lucina_get(l, &format!("/workspaces/{local_id}/file?path=bin.dat"))["kind"]);
    same("read outside", &oracle("read", &rp, Some("../etc/passwd"))["error"], &lucina_get(l, &format!("/workspaces/{local_id}/file?path=../etc/passwd"))["error"]["message"]);

    // Archive: the clean worktree goes on both sides.
    let (pa, la) = (p.json(&["workspace", "archive", &pid]), l.json(&["workspace", "archive", &lid]));
    same("archive", &json!({"status": pa["status"], "keys": keys_of(&pa)}), &json!({"status": la["status"], "keys": keys_of(&la)}));
    for (what, v) in [("paseo", &pw), ("lucina", &lw)] {
        assert!(!std::path::Path::new(v["cwd"].as_str().unwrap()).exists(), "{what} removed the clean worktree");
    }
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `KEY=value` lines of a file, for the given prefix.
fn env_file(path: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Phase 5 (T1, S1): terminals (create, send-keys, capture, ls, kill) and scripts
/// (ls, start, env, ports from `servicePorts.range`, stop).
fn scenario_terminals_scripts(p: &Paseo, l: &Daemon) {
    let root = tempfile::Builder::new().prefix("ldiff-term.").tempdir_in("/tmp").unwrap();
    let mk = |side: &str| {
        let r = root.path().join(side).join("app");
        std::fs::create_dir_all(&r).unwrap();
        std::fs::write(
            r.join("paseo.json"),
            json!({
                "worktree": {"servicePorts": {"range": "4300-4399"}},
                "scripts": {
                    "env": {"command": "env | grep -E '^(PASEO_|HOST=)' | sort > env.txt"},
                    "web": {"command": "env | grep -E '^(PASEO_|HOST=)' | sort > web-env.txt; exec python3 -m http.server \"$PASEO_PORT\" --bind 127.0.0.1", "type": "service"},
                }
            })
            .to_string(),
        )
        .unwrap();
        r
    };
    let (pr, lr) = (mk("paseo"), mk("lucina"));
    let (rp, rl) = (pr.display().to_string(), lr.display().to_string());
    let ws = |v: &Value| v["workspaceId"].as_str().or(v["id"].as_str()).unwrap_or_default().to_string();
    let pw = ws(&p.json(&["workspace", "create", "--isolation", "local", "--path", &rp]));
    let lw = ws(&l.json(&["workspace", "create", "--isolation", "local", "--path", &rl]));

    // Terminals.
    let pt = p.json(&["terminal", "create", "--workspace", &pw, "--name", "diff-term"]);
    let lt = l.json(&["terminal", "create", "--workspace", &lw, "--name", "diff-term"]);
    same("terminal create", &json!({"name": pt["name"], "ws": pt["workspaceId"] == pw.as_str()}), &json!({"name": lt["name"], "ws": lt["workspaceId"] == lw.as_str()}));
    let (pid, lid) = (pt["id"].as_str().unwrap().to_string(), lt["id"].as_str().unwrap().to_string());
    let cap = |side: &str, id: &str| -> Value { if side == "p" { p.json(&["terminal", "capture", id]) } else { l.json(&["terminal", "capture", id]) } };
    let has = |v: &Value, f: &dyn Fn(&str) -> bool| v["lines"].as_array().into_iter().flatten().any(|x| f(x.as_str().unwrap_or("")));
    for (side, id) in [("p", &pid), ("l", &lid)] {
        wait_until("a shell prompt", 30, || has(&cap(side, id), &|x| !x.trim().is_empty()));
    }
    std::thread::sleep(Duration::from_millis(500));
    let ps = p.json(&["terminal", "send-keys", &pid, "echo lucina-$((6*7))", "Enter"]);
    let ls = l.json(&["terminal", "send-keys", &lid, "echo lucina-$((6*7))", "Enter"]);
    same("send-keys", &pick(&ps, &["keysSent"]), &pick(&ls, &["keysSent"]));
    for (side, id) in [("p", &pid), ("l", &lid)] {
        wait_until("the echo", 20, || has(&cap(side, id), &|x| x == "lucina-42"));
    }
    let shape = |v: &Value| json!({"keys": keys(v), "typed": has(v, &|x| x.ends_with("echo lucina-$((6*7))")), "out": has(v, &|x| x == "lucina-42"), "total": v["totalLines"]});
    same("capture after keys", &shape(&cap("p", &pid)), &shape(&cap("l", &lid)));
    let range = |side: &str, id: &str| -> Value {
        let v = if side == "p" { p.json(&["terminal", "capture", id, "--start", "0", "--end", "1"]) } else { l.json(&["terminal", "capture", id, "--start", "0", "--end", "1"]) };
        json!(v["lines"].as_array().map(Vec::len))
    };
    same("capture --start 0 --end 1", &range("p", &pid), &range("l", &lid));
    let pls = p.json(&["terminal", "ls", "--workspace", &pw]);
    let lls = l.json(&["terminal", "ls", "--workspace", &lw]);
    let names = |v: &Value| json!(v.as_array().into_iter().flatten().map(|t| t["name"].clone()).collect::<Vec<_>>());
    same("terminal ls", &names(&pls), &names(&lls));
    same("terminal kill", &pick(&p.json(&["terminal", "kill", &pid]), &["success"]), &pick(&l.json(&["terminal", "kill", &lid]), &["success"]));
    let (pe, le) = (p.cmd(&["terminal", "capture", "nope-nope", "--json"]), l.lagent(&["terminal", "capture", "nope-nope", "--json"]));
    let err_code = |r: &Run| -> Value { serde_json::from_str::<Value>(if r.stdout.trim().is_empty() { &r.stderr } else { &r.stdout }).map(|v| v["error"]["code"].clone()).unwrap_or(Value::Null) };
    same("capture of a missing terminal", &json!([pe.code, err_code(&pe)]), &json!([le.code, err_code(&le)]));

    // Scripts.
    let row = |v: &Value| json!(v.as_array().into_iter().flatten().map(|x| pick(x, &["scriptName", "type", "lifecycle"])).collect::<Vec<_>>());
    same("script ls", &row(&p.json(&["script", "ls", "--workspace", &pw])), &row(&l.json(&["script", "ls", "--workspace", &lw])));
    let pe = p.json(&["script", "start", "env", "--workspace", &pw]);
    let le = l.json(&["script", "start", "env", "--workspace", &lw]);
    same("script start (plain)", &pick(&pe, &["scriptName", "type", "lifecycle", "port"]), &pick(&le, &["scriptName", "type", "lifecycle", "port"]));
    for d in [&pr, &lr] {
        wait_until("env.txt", 20, || d.join("env.txt").exists() && !env_file(&d.join("env.txt")).is_empty());
    }
    // Paseo types into an interactive shell; give both a moment to finish writing.
    std::thread::sleep(Duration::from_millis(300));
    let (penv, lenv) = (env_file(&pr.join("env.txt")), env_file(&lr.join("env.txt")));
    let norm = |m: &std::collections::BTreeMap<String, String>, side: &str, ws: &str| -> Value {
        let base = root.path().join(side).display().to_string();
        json!(m.iter().filter(|(k, _)| !matches!(k.as_str(), "PASEO_ACTIVITY_TOKEN" | "PASEO_TERMINAL_ID")).map(|(k, v)| (k.clone(), v.replace(&base, "<root>").replace(ws, "<ws>"))).collect::<std::collections::BTreeMap<_, _>>())
    };
    let (pn, ln) = (norm(&penv, "paseo", &pw), norm(&lenv, "lucina", &lw));
    let pk: Vec<&String> = pn.as_object().unwrap().keys().collect();
    // Intentional: no Paseo home; the activity URL only with `--listen` (`lagent hooks` uses the socket).
    let missing: Vec<&&String> = pk
        .iter()
        .filter(|k| !matches!(k.as_str(), "PASEO_HOME" | "PASEO_TERMINAL_ACTIVITY_URL"))
        .filter(|k| !ln.as_object().unwrap().contains_key(k.as_str()))
        .collect();
    assert!(missing.is_empty(), "plain script env: Lucina lacks {missing:?}\n--- paseo\n{pn:#}\n--- lucina\n{ln:#}");
    same("plain script PASEO_WORKSPACE_ID", &pn["PASEO_WORKSPACE_ID"], &ln["PASEO_WORKSPACE_ID"]);
    for (what, v) in [("paseo", &pn), ("lucina", &ln)] {
        assert!(v["PASEO_HOOK_CLI"].as_str().is_some_and(|c| std::path::Path::new(c).exists()), "{what}: PASEO_HOOK_CLI");
    }

    let pw_ = p.json(&["script", "start", "web", "--workspace", &pw]);
    let lw_ = l.json(&["script", "start", "web", "--workspace", &lw]);
    same("script start (service)", &pick(&pw_, &["scriptName", "type", "lifecycle"]), &pick(&lw_, &["scriptName", "type", "lifecycle"]));
    for (what, v) in [("paseo", &pw_), ("lucina", &lw_)] {
        let port = v["port"].as_u64().unwrap_or(0);
        assert!((4300..=4399).contains(&port), "{what} service port from servicePorts.range: {v}");
    }
    for d in [&pr, &lr] {
        wait_until("web-env.txt", 20, || !env_file(&d.join("web-env.txt")).is_empty());
    }
    std::thread::sleep(Duration::from_millis(300));
    let (pwe, lwe) = (norm(&env_file(&pr.join("web-env.txt")), "paseo", &pw), norm(&env_file(&lr.join("web-env.txt")), "lucina", &lw));
    let names_of = |v: &Value| json!(v.as_object().unwrap().keys().filter(|k| k.starts_with("PASEO_PORT") || k.starts_with("PASEO_SERVICE_") || *k == "HOST" || *k == "PASEO_URL").cloned().collect::<Vec<_>>());
    same("service env names", &names_of(&pwe), &names_of(&lwe));
    same("service HOST", &pwe["HOST"], &lwe["HOST"]);
    same("service PASEO_PORT = its port", &json!(pwe["PASEO_PORT"] == json!(pw_["port"].to_string())), &json!(lwe["PASEO_PORT"] == json!(lw_["port"].to_string())));
    let stop = |v: Value| pick(&v, &["scriptName", "lifecycle"]);
    same("script stop", &stop(p.json(&["script", "stop", "web", "--workspace", &pw])), &stop(l.json(&["script", "stop", "web", "--workspace", &lw])));
    let (pst, lst) = (p.cmd(&["script", "stop", "web", "--workspace", &pw, "--json"]), l.lagent(&["script", "stop", "web", "--workspace", &lw, "--json"]));
    same("stop a stopped script", &json!([pst.code, err_code(&pst)]), &json!([lst.code, err_code(&lst)]));
}

/// Two settled answers, the same text on both sides (no timing in them).
fn two_answers(p: &Paseo, l: &Daemon) -> (String, String) {
    let prompt = |n: &str| format!("emit settled assistant image markdown: ![{n}](https://example.test/{n}.png)");
    let first = prompt("one");
    let args = ["run", "--provider", "mock/e2e-fast-stream", "--title", "Rewind source", "--cwd", "/tmp", first.as_str()];
    let (pr, lr) = (p.json(&args), l.json(&args));
    let (pid, lid) = (id(&pr), id(&lr));
    let second = prompt("two");
    p.json(&["send", &pid, &second]);
    l.json(&["send", &lid, &second]);
    p.json(&["wait", &pid]);
    l.json(&["wait", &lid]);
    (pid, lid)
}

fn rows_of(timeline: &Value) -> Vec<Value> {
    timeline["rows"].as_array().cloned().unwrap_or_default()
}

fn of_kind(rows: &[Value], kind: &str) -> Vec<Value> {
    rows.iter().filter(|r| r["item"]["type"] == kind).cloned().collect()
}

fn user_texts(rows: &[Value]) -> Value {
    json!(of_kind(rows, "user_message").iter().map(|r| r["item"]["text"].clone()).collect::<Vec<_>>())
}

/// The fork-context attachment and the rewind modes (parity row A16), through Paseo's
/// client (`agent.fork_context.request`, `agent.rewind.request`).
fn scenario_rewind_fork(p: &Paseo, l: &Daemon) {
    let (pid, lid) = two_answers(p, l);
    let ptl = agent_oracle("timeline", &pid, None, None);
    let ltl = lucina_get(l, &format!("/agents/{lid}/timeline"));
    let (prows, lrows) = (rows_of(&ptl), rows_of(&ltl));
    same("user prompts", &user_texts(&prows), &user_texts(&lrows));

    // The attachment, whole and up to each boundary.
    let context = |boundary: Value| -> (Value, Value) {
        let asked = if boundary.is_null() { json!({}) } else { boundary };
        let paseo = agent_oracle("fork-context", &pid, Some(&asked.to_string()), None);
        let (_, lucina) = lucina_call(l, "POST", &format!("/agents/{lid}/fork-context"), &asked);
        (paseo, lucina)
    };
    let shown = |v: &Value| json!({"attachment": v["attachment"], "itemCount": v["itemCount"], "error": v["error"]});
    let (pc, lc) = context(Value::Null);
    same("fork context, whole history", &shown(&pc), &shown(&lc));
    assert!(pc["attachment"]["text"].as_str().is_some_and(|t| t.contains("[User] emit settled") && t.contains("[Assistant] ![one]")), "{pc}");
    let first_answer = |rows: &[Value]| of_kind(rows, "assistant_message").into_iter().next().expect("an answer");
    let (pa, la) = (first_answer(&prows), first_answer(&lrows));
    // Each side's own ids: the text compared is the same.
    let paseo = agent_oracle("fork-context", &pid, Some(&json!({"boundaryMessageId": pa["item"]["messageId"]}).to_string()), None);
    let (_, lucina) = lucina_call(l, "POST", &format!("/agents/{lid}/fork-context"), &json!({"boundaryMessageId": la["item"]["messageId"]}));
    same("fork context, by assistant message id", &shown(&paseo), &shown(&lucina));
    assert_eq!(paseo["itemCount"], 2, "{paseo}");
    let paseo = agent_oracle("fork-context", &pid, Some(&json!({"boundaryCursor": {"epoch": ptl["epoch"], "seq": pa["seq"]}}).to_string()), None);
    let (_, lucina) = lucina_call(l, "POST", &format!("/agents/{lid}/fork-context"), &json!({"boundaryCursor": {"epoch": ltl["epoch"], "seq": la["seq"]}}));
    same("fork context, by cursor", &shown(&paseo), &shown(&lucina));
    // Boundaries that are gone: Paseo's message, as an error.
    let gone = |paseo_body: Value, lucina_body: Value| {
        let paseo = agent_oracle("fork-context", &pid, Some(&paseo_body.to_string()), None);
        let (status, lucina) = lucina_call(l, "POST", &format!("/agents/{lid}/fork-context"), &lucina_body);
        assert_eq!(status, 409, "{lucina}");
        same("fork context, a boundary that is gone", &paseo["error"], &lucina["error"]["message"]);
    };
    gone(json!({"boundaryMessageId": "nope"}), json!({"boundaryMessageId": "nope"}));
    gone(json!({"boundaryCursor": {"epoch": "stale", "seq": 2}}), json!({"boundaryCursor": {"epoch": "stale", "seq": 2}}));
    gone(json!({"boundaryCursor": {"epoch": ptl["epoch"], "seq": 99}}), json!({"boundaryCursor": {"epoch": ltl["epoch"], "seq": 99}}));

    // Files and both are accepted by the mock on both sides; the conversation is what both
    // keep of the first prompt (Paseo's mock keeps only that message, Lucina's the rows
    // before the target: the prompts compared are the same).
    let second = |rows: &[Value]| of_kind(rows, "user_message")[1]["item"]["messageId"].clone();
    let (pm, lm) = (second(&prows), second(&lrows));
    assert!(pm.is_string() && lm.is_string(), "{pm} {lm}");
    let paseo = agent_oracle("rewind", &pid, Some(pm.as_str().unwrap()), Some("files"));
    let (status, lucina) = lucina_call(l, "POST", &format!("/agents/{lid}/rewind"), &json!({"messageId": lm, "mode": "files"}));
    assert_eq!(status, 200, "{lucina}");
    same("rewind files", &json!(paseo["ok"]), &lucina["ok"]);
    same("files rewind keeps the prompts", &user_texts(&rows_of(&agent_oracle("timeline", &pid, None, None))), &user_texts(&rows_of(&lucina_get(l, &format!("/agents/{lid}/timeline")))));
    // Not a message of the agent: both refuse (the messages differ, see the table above).
    let paseo = agent_oracle("rewind", &pid, Some("nope"), Some("conversation"));
    let (status, lucina) = lucina_call(l, "POST", &format!("/agents/{lid}/rewind"), &json!({"messageId": "nope", "mode": "conversation"}));
    assert!(paseo["error"].is_string() && status == 404, "{paseo} {lucina}");
    for mode in ["conversation", "both"] {
        let (pid, lid) = two_answers(p, l);
        let (prows, lrows) = (rows_of(&agent_oracle("timeline", &pid, None, None)), rows_of(&lucina_get(l, &format!("/agents/{lid}/timeline"))));
        let (pm, lm) = (second(&prows), second(&lrows));
        let paseo = agent_oracle("rewind", &pid, Some(pm.as_str().unwrap()), Some(mode));
        let (status, lucina) = lucina_call(l, "POST", &format!("/agents/{lid}/rewind"), &json!({"messageId": lm, "mode": mode}));
        assert_eq!(status, 200, "{lucina}");
        same(&format!("rewind {mode}"), &json!(paseo["ok"]), &lucina["ok"]);
        same(
            &format!("prompts left after rewind {mode}"),
            &user_texts(&rows_of(&agent_oracle("timeline", &pid, None, None))),
            &user_texts(&rows_of(&lucina_get(l, &format!("/agents/{lid}/timeline")))),
        );
    }
}

fn keys_of(v: &Value) -> Value {
    keys(v)
}

#[test]
fn paseo_parity() {
    if std::env::var("PASEO_DIFF").as_deref() != Ok("1") {
        eprintln!("paseo_parity: skipped (set PASEO_DIFF=1 to compare against a dev Paseo on 127.0.0.1:{PORT})");
        return;
    }
    let paseo = Paseo::start();
    // Lucina's worktrees go to a temporary root, never ~/Projects/.worktrees.
    let roots = tempfile::tempdir().expect("roots");
    let lucina = Daemon::start_with_env(&[
        ("WT_ROOT", roots.path().join("wt").display().to_string()),
        ("WT_PROJECTS", roots.path().join("proj").display().to_string()),
        ("LUCINA_AGENTD_METADATA_PROVIDER", "off".to_string()),
    ]);
    type Scenario = fn(&Paseo, &Daemon);
    let scenarios: [(&str, Scenario); 9] = [
        ("a run/logs/inspect/ls", scenario_run_logs_inspect),
        ("b turn failure", scenario_turn_failure),
        ("c plan approval + stop", scenario_plan_approval_stop),
        ("d send --interrupt + wait", scenario_send_wait),
        ("e archive/ls/delete/not found", scenario_archive_delete),
        ("f permit ls/allow/deny + errors", scenario_permit),
        ("g projects/workspaces/worktrees, diffs, files, archive", scenario_workspaces),
        ("h terminals, send-keys, capture; scripts, env, ports", scenario_terminals_scripts),
        ("i fork context, rewind modes", scenario_rewind_fork),
    ];
    // PASEO_DIFF_ONLY=i runs one scenario (the letter before its name).
    let only = std::env::var("PASEO_DIFF_ONLY").ok();
    for (name, scenario) in scenarios {
        if only.as_deref().is_some_and(|o| !name.starts_with(&format!("{o} "))) {
            continue;
        }
        let start = Instant::now();
        scenario(&paseo, &lucina);
        eprintln!("paseo_parity: {name}: same ({:.1}s)", start.elapsed().as_secs_f32());
    }
}
