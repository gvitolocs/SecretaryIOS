//! Where the notes go: the owner's canonical vault, `/home/nez/mnt/mybook/Obsidian` on
//! nezopt (a local `dir:` store; moved from the Pi on 2026-11-08), never a git
//! repo (Honcho `obsidian-pi-rule-20260828`, updated that day). The `ssh:` store
//! stays for a remote vault.
//!
//! - [`SshStore`]: the Pi over SSH. Each operation is one `ssh <host> python3 -`
//!   call (a shared control connection keeps it cheap) running a small helper
//!   that confines paths to the vault root, refuses symlinks, and writes with
//!   compare-and-swap (the page must still be what was read) through a temp
//!   file and an atomic rename. A connection failure is [`StoreError::Offline`]:
//!   the item stays queued and delivery retries later.
//! - [`LocalStore`]: a directory (tests, dry runs on a scratch copy). It refuses
//!   a directory inside a git repo.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The vault host cannot be reached: keep the item, try later.
    Offline(String),
    /// The page changed since it was read: read again and redo the change.
    Conflict(String),
    Other(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Offline(s) => write!(f, "vault offline: {s}"),
            StoreError::Conflict(s) => write!(f, "vault page changed meanwhile: {s}"),
            StoreError::Other(s) => write!(f, "vault: {s}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// The version a write expects: the sha256 of the text that was read.
pub fn version(text: Option<&str>) -> Option<String> {
    text.map(|t| {
        Sha256::digest(t.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    })
}

/// A confined relative `.md` path (no `..`, hidden parts, NUL, backslash).
pub fn check_rel(rel: &str) -> Result<(), StoreError> {
    lucina_ops::vault::check_rel(rel)
        .map(|_| ())
        .map_err(|e| StoreError::Other(e.to_string()))
}

fn check_dir(rel: &str) -> Result<(), StoreError> {
    let ok = !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains(['\\', '\0'])
        && rel
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.starts_with('.'));
    if ok {
        Ok(())
    } else {
        Err(StoreError::Other(format!("bad vault directory {rel:?}")))
    }
}

#[async_trait::async_trait]
pub trait VaultStore: Send + Sync {
    /// The page's text, `None` when it does not exist.
    async fn read(&self, rel: &str) -> Result<Option<String>, StoreError>;
    /// The `.md` pages directly in a directory (empty when it is missing).
    async fn list(&self, dir: &str) -> Result<Vec<String>, StoreError>;
    /// Replace the page if it still is the text `expected` (its [`version`];
    /// `None`: it must not exist). Returns whether it changed.
    async fn write(
        &self,
        rel: &str,
        text: &str,
        expected: Option<&str>,
    ) -> Result<bool, StoreError>;
    /// Where it writes (status, logs).
    fn describe(&self) -> String;
}

// ---------------------------------------------------------------- local

/// A vault in a local directory (tests and dry runs).
#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
}

/// Whether `p` or one of its parents holds a `.git`.
pub fn inside_git_repo(p: &Path) -> bool {
    p.ancestors().any(|a| a.join(".git").exists())
}

impl LocalStore {
    pub fn open(root: &Path) -> Result<Self, StoreError> {
        let root = root
            .canonicalize()
            .map_err(|e| StoreError::Other(format!("{}: {e}", root.display())))?;
        if !root.is_dir() {
            return Err(StoreError::Other(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        if inside_git_repo(&root) {
            return Err(StoreError::Other(format!(
                "{} is inside a git repo; notes never go there",
                root.display()
            )));
        }
        Ok(Self { root })
    }

    /// The parent directory of `rel` (created when `create`), refusing symlinks.
    fn dir_of(&self, rel: &str, create: bool) -> Result<Option<PathBuf>, StoreError> {
        let parts: Vec<&str> = rel.split('/').collect();
        let mut d = self.root.clone();
        for p in &parts[..parts.len() - 1] {
            d.push(p);
            match std::fs::symlink_metadata(&d) {
                Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                    return Err(StoreError::Other(format!("{rel}: not a plain directory")))
                }
                Ok(_) => {}
                Err(_) if create => {
                    std::fs::create_dir(&d).map_err(|e| StoreError::Other(e.to_string()))?
                }
                Err(_) => return Ok(None),
            }
        }
        Ok(Some(d))
    }

    fn read_now(&self, rel: &str) -> Result<Option<String>, StoreError> {
        check_rel(rel)?;
        let Some(d) = self.dir_of(rel, false)? else {
            return Ok(None);
        };
        let f = d.join(rel.rsplit('/').next().unwrap_or(rel));
        match std::fs::symlink_metadata(&f) {
            Ok(m) if m.file_type().is_symlink() => {
                Err(StoreError::Other(format!("{rel}: symlink")))
            }
            Ok(_) => std::fs::read(&f)
                .map(|b| Some(String::from_utf8_lossy(&b).into_owned()))
                .map_err(|e| StoreError::Other(e.to_string())),
            Err(_) => Ok(None),
        }
    }
}

#[async_trait::async_trait]
impl VaultStore for LocalStore {
    async fn read(&self, rel: &str) -> Result<Option<String>, StoreError> {
        self.read_now(rel)
    }

    async fn list(&self, dir: &str) -> Result<Vec<String>, StoreError> {
        check_dir(dir)?;
        let Some(d) = self.dir_of(&format!("{dir}/x.md"), false)? else {
            return Ok(Vec::new());
        };
        let mut out: Vec<String> = std::fs::read_dir(&d)
            .map_err(|e| StoreError::Other(e.to_string()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".md") && !n.starts_with('.'))
            .collect();
        out.sort();
        Ok(out)
    }

    async fn write(
        &self,
        rel: &str,
        text: &str,
        expected: Option<&str>,
    ) -> Result<bool, StoreError> {
        let cur = self.read_now(rel)?;
        if version(cur.as_deref()).as_deref() != expected {
            return Err(StoreError::Conflict(rel.to_owned()));
        }
        if cur.as_deref() == Some(text) {
            return Ok(false);
        }
        let d = self.dir_of(rel, true)?.expect("created");
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let tmp = d.join(format!(".{name}.ingest-tmp"));
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, d.join(name)))
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(true)
    }

    fn describe(&self) -> String {
        format!("dir:{}", self.root.display())
    }
}

// ---------------------------------------------------------------- ssh

/// Runs on the vault host with the request inlined as a JSON string literal.
const HELPER: &str = r#"
import hashlib, json, os
req = json.loads(REQ)
root = os.path.realpath(req["root"])
def sha(t):
    return hashlib.sha256(t.encode("utf-8")).hexdigest()
def parts_of(rel):
    ps = rel.split("/")
    if rel.startswith("/") or any(p in ("", ".", "..") or p.startswith(".") or "\\" in p or "\0" in p for p in ps):
        raise ValueError("bad path " + rel)
    return ps
def walk(ps, create):
    d = root
    for p in ps:
        d = os.path.join(d, p)
        if os.path.islink(d):
            raise ValueError("symlink in path")
        if not os.path.isdir(d):
            if not create:
                return None
            os.mkdir(d, 0o755)
    return d
def read(f):
    if f is None or not os.path.lexists(f):
        return None
    if os.path.islink(f) or not os.path.isfile(f):
        raise ValueError("not a plain file")
    with open(f, encoding="utf-8", errors="replace") as h:
        return h.read()
try:
    if not os.path.isdir(root):
        raise ValueError("vault root missing")
    op = req["op"]
    if op == "read":
        ps = parts_of(req["rel"])
        d = walk(ps[:-1], False)
        out = {"ok": True, "text": read(None if d is None else os.path.join(d, ps[-1]))}
    elif op == "list":
        d = walk(parts_of(req["dir"]), False)
        names = [] if d is None else sorted(n for n in os.listdir(d)
            if n.endswith(".md") and not n.startswith(".")
            and os.path.isfile(os.path.join(d, n)) and not os.path.islink(os.path.join(d, n)))
        out = {"ok": True, "names": names}
    elif op == "write":
        ps = parts_of(req["rel"])
        d = walk(ps[:-1], True)
        f = os.path.join(d, ps[-1])
        cur = read(f)
        if (None if cur is None else sha(cur)) != req["expected"]:
            out = {"ok": False, "conflict": True}
        elif cur == req["text"]:
            out = {"ok": True, "changed": False}
        else:
            tmp = os.path.join(d, "." + ps[-1] + ".ingest-tmp")
            with open(tmp, "w", encoding="utf-8") as h:
                h.write(req["text"])
                h.flush()
                os.fsync(h.fileno())
            os.replace(tmp, f)
            out = {"ok": True, "changed": True}
    else:
        raise ValueError("bad op")
except Exception as e:
    out = {"ok": False, "error": str(e)}
print(json.dumps(out))
"#;

/// A remote vault over SSH (the Pi until 2026-11-08).
#[derive(Debug, Clone)]
pub struct SshStore {
    /// Tried in order while the connection fails (`pi-cursor`, then a tunnel alias).
    hosts: Vec<String>,
    root: String,
    /// The control socket directory (connection reuse).
    control_dir: PathBuf,
}

impl SshStore {
    pub fn new(hosts: Vec<String>, root: &str, control_dir: &Path) -> Result<Self, StoreError> {
        let host_ok = |h: &String| {
            !h.is_empty()
                && !h.starts_with('-')
                && h.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
        };
        if hosts.is_empty() || !hosts.iter().all(host_ok) {
            return Err(StoreError::Other(format!("bad vault hosts {hosts:?}")));
        }
        if !root.starts_with('/') || root.split('/').any(|p| p == "..") {
            return Err(StoreError::Other(format!("bad vault root {root:?}")));
        }
        Ok(Self {
            hosts,
            root: root.trim_end_matches('/').to_owned(),
            control_dir: control_dir.to_owned(),
        })
    }

    async fn call(&self, req: Value) -> Result<Value, StoreError> {
        let mut req = req;
        req["root"] = json!(self.root);
        let literal = serde_json::to_string(&req.to_string())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let script = HELPER.replacen("REQ", &literal, 1);
        let control = format!("ControlPath={}/lvi-%C", self.control_dir.display());
        let mut last = String::new();
        for host in &self.hosts {
            let mut cmd = tokio::process::Command::new("ssh");
            cmd.args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ControlMaster=auto",
                "-o",
                &control,
                "-o",
                "ControlPersist=120",
                host.as_str(),
                "python3",
                "-",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
            let mut child = cmd
                .spawn()
                .map_err(|e| StoreError::Other(format!("ssh: {e}")))?;
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                stdin
                    .write_all(script.as_bytes())
                    .await
                    .map_err(|e| StoreError::Other(format!("ssh stdin: {e}")))?;
            }
            let out = match tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
                .await
            {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => return Err(StoreError::Other(format!("ssh: {e}"))),
                Err(_) => {
                    last = format!("{host}: timed out");
                    continue;
                }
            };
            // 255: ssh itself failed (no route, refused, auth): try the next host.
            if out.status.code() == Some(255) {
                last = format!(
                    "{host}: {}",
                    crate::clip(String::from_utf8_lossy(&out.stderr).trim(), 200)
                );
                continue;
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let v: Value = serde_json::from_str(text.trim()).map_err(|_| {
                StoreError::Other(format!(
                    "{host}: helper failed: {}",
                    crate::clip(String::from_utf8_lossy(&out.stderr).trim(), 300)
                ))
            })?;
            if v["ok"].as_bool() == Some(true) {
                return Ok(v);
            }
            if v["conflict"].as_bool() == Some(true) {
                return Err(StoreError::Conflict(
                    req["rel"].as_str().unwrap_or("").to_owned(),
                ));
            }
            return Err(StoreError::Other(format!(
                "{host}: {}",
                v["error"].as_str().unwrap_or("unknown error")
            )));
        }
        Err(StoreError::Offline(last))
    }
}

#[async_trait::async_trait]
impl VaultStore for SshStore {
    async fn read(&self, rel: &str) -> Result<Option<String>, StoreError> {
        check_rel(rel)?;
        let v = self.call(json!({"op": "read", "rel": rel})).await?;
        Ok(v["text"].as_str().map(str::to_owned))
    }

    async fn list(&self, dir: &str) -> Result<Vec<String>, StoreError> {
        check_dir(dir)?;
        let v = self.call(json!({"op": "list", "dir": dir})).await?;
        Ok(v["names"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn write(
        &self,
        rel: &str,
        text: &str,
        expected: Option<&str>,
    ) -> Result<bool, StoreError> {
        check_rel(rel)?;
        let v = self
            .call(json!({"op": "write", "rel": rel, "text": text, "expected": expected}))
            .await?;
        Ok(v["changed"].as_bool().unwrap_or(false))
    }

    fn describe(&self) -> String {
        format!("ssh:{}:{}", self.hosts.join(","), self.root)
    }
}

/// `ssh:<host>[,<host>…]:<root>` or `dir:<path>`.
pub fn open_target(spec: &str, control_dir: &Path) -> Result<Box<dyn VaultStore>, StoreError> {
    if let Some(rest) = spec.strip_prefix("ssh:") {
        let (hosts, root) = rest
            .split_once(':')
            .ok_or_else(|| StoreError::Other(format!("{spec:?}: expected ssh:<hosts>:<root>")))?;
        let hosts = hosts.split(',').map(|h| h.trim().to_owned()).collect();
        return Ok(Box::new(SshStore::new(hosts, root.trim(), control_dir)?));
    }
    if let Some(p) = spec.strip_prefix("dir:") {
        return Ok(Box::new(LocalStore::open(Path::new(p.trim()))?));
    }
    Err(StoreError::Other(format!(
        "{spec:?}: expected ssh:<hosts>:<root> or dir:<path>"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_store_is_confined_and_compares_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let s = LocalStore::open(dir.path()).unwrap();
        assert!(s.write("A/Permanent/x.md", "1", None).await.unwrap());
        assert_eq!(
            s.read("A/Permanent/x.md").await.unwrap().as_deref(),
            Some("1")
        );
        // Stale expectation: refused, nothing written.
        assert_eq!(
            s.write("A/Permanent/x.md", "2", None).await,
            Err(StoreError::Conflict("A/Permanent/x.md".into()))
        );
        let v = version(Some("1"));
        assert!(!s
            .write("A/Permanent/x.md", "1", v.as_deref())
            .await
            .unwrap());
        assert!(s
            .write("A/Permanent/x.md", "2", v.as_deref())
            .await
            .unwrap());
        assert_eq!(s.list("A/Permanent").await.unwrap(), ["x.md"]);
        assert!(s.list("Nope").await.unwrap().is_empty());
        for bad in ["../x.md", "/etc/x.md", "A/.obsidian/x.md", "A/x.txt"] {
            assert!(s.write(bad, "x", None).await.is_err(), "{bad}");
        }
        std::os::unix::fs::symlink("/tmp", dir.path().join("L")).unwrap();
        assert!(s.write("L/x.md", "x", None).await.is_err(), "symlinked dir");
    }

    #[test]
    fn never_inside_a_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("vault")).unwrap();
        assert!(LocalStore::open(&dir.path().join("vault")).is_err());
    }

    #[test]
    fn targets() {
        let d = std::path::Path::new("/tmp");
        assert_eq!(
            open_target("ssh:pi-cursor,pi-cursor-remote:/home/nes/Obsidian", d)
                .unwrap()
                .describe(),
            "ssh:pi-cursor,pi-cursor-remote:/home/nes/Obsidian"
        );
        for bad in [
            "ssh:-oProxyCommand=x:/v",
            "ssh:pi:relative",
            "ssh:pi:/a/../b",
            "ftp:x",
            "ssh:pi",
        ] {
            assert!(open_target(bad, d).is_err(), "{bad}");
        }
    }

    /// The real channel: `VAULT_INGEST_SSH_TEST=<host>:<existing scratch dir>`
    /// (e.g. `pi-cursor:/tmp/vault-ingest-sshtest`), run with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn ssh_store_against_a_real_host() {
        let spec = std::env::var("VAULT_INGEST_SSH_TEST").expect("VAULT_INGEST_SSH_TEST=host:/dir");
        let (host, root) = spec.split_once(':').unwrap();
        let ctl = tempfile::tempdir().unwrap();
        let s = SshStore::new(vec![host.into()], root, ctl.path()).unwrap();
        let rel = "P/Permanent/A claim with spaces, quotes \"x\" and ü.md";
        let text = "line 1\n`code` and \\ backslash\n";
        let before = s.read(rel).await.unwrap();
        s.write(rel, text, version(before.as_deref()).as_deref())
            .await
            .unwrap();
        assert_eq!(s.read(rel).await.unwrap().as_deref(), Some(text));
        assert!(s
            .list("P/Permanent")
            .await
            .unwrap()
            .iter()
            .any(|n| rel.ends_with(n.as_str())));
        assert!(matches!(
            s.write(rel, "stale", None).await,
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            s.read(rel).await.unwrap().as_deref(),
            Some(text),
            "a stale write changes nothing"
        );
        // An unreachable host is "offline", and the next host is tried.
        let bad = SshStore::new(vec!["no-such-host.invalid".into()], root, ctl.path()).unwrap();
        assert!(matches!(bad.read(rel).await, Err(StoreError::Offline(_))));
        let both = SshStore::new(
            vec!["no-such-host.invalid".into(), host.into()],
            root,
            ctl.path(),
        )
        .unwrap();
        assert_eq!(both.read(rel).await.unwrap().as_deref(), Some(text));
    }

    /// The helper itself, run with the local python3 against a temp root.
    #[tokio::test]
    async fn the_helper_works() {
        if std::process::Command::new("python3")
            .arg("-V")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let run = |req: Value| {
            let mut req = req;
            req["root"] = json!(dir.path().display().to_string());
            let lit = serde_json::to_string(&req.to_string()).unwrap();
            let script = HELPER.replacen("REQ", &lit, 1);
            let mut c = std::process::Command::new("python3")
                .arg("-")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write;
            c.stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            let o = c.wait_with_output().unwrap();
            serde_json::from_slice::<Value>(&o.stdout).unwrap()
        };
        let text = "a \"quoted\" note\nwith ünïcode and \\ backslash\n";
        let w =
            run(json!({"op": "write", "rel": "P/Permanent/n.md", "text": text, "expected": null}));
        assert_eq!(w["changed"], true, "{w}");
        assert_eq!(
            run(json!({"op": "read", "rel": "P/Permanent/n.md"}))["text"],
            text
        );
        assert_eq!(
            run(json!({"op": "list", "dir": "P/Permanent"}))["names"],
            json!(["n.md"])
        );
        let stale =
            run(json!({"op": "write", "rel": "P/Permanent/n.md", "text": "x", "expected": null}));
        assert_eq!(stale["conflict"], true);
        let ok = run(
            json!({"op": "write", "rel": "P/Permanent/n.md", "text": "x", "expected": version(Some(text))}),
        );
        assert_eq!(ok["changed"], true);
        assert_eq!(
            run(json!({"op": "read", "rel": "../etc/passwd"}))["ok"],
            false
        );
        assert_eq!(
            run(json!({"op": "read", "rel": "P/missing.md"}))["text"],
            Value::Null
        );
    }
}
