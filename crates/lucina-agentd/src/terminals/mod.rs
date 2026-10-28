//! Terminals, scripts and services (Paseo `terminal/*`, `worktree-bootstrap.js
//! spawnWorkspaceScript`, `script-status-projection.js`, `service-proxy.js`; design:
//! "Terminals, scripts and services").
//!
//! - A terminal is a PTY (`portable-pty`) whose program is a session leader (setsid).
//!   Its output feeds a `vt100` screen with 10 000 lines of scrollback, for
//!   [`Hub::capture`] and the snapshot a new viewer starts from, and a 1 MiB buffer of
//!   raw bytes numbered by offset, so a viewer that reconnects gets what it missed.
//!   Terminals outlive their viewers, not the daemon: shutdown kills them, and their
//!   pids are in the process registry so the next start reaps any that survived.
//! - Every terminal belongs to a workspace and has an owner: `owner` (the owner's
//!   devices or `lagent`), `agent:<id>` or `mcp:<token>`. `agentId` is the agent it
//!   serves. Who may touch it is decided in [`crate::api`] (`terminals.rs`).
//! - Processes in a terminal count as agent processes ([`Hub::session_pids`]): they
//!   can't answer permission requests, whoever opened the terminal.
//! - Scripts ([`scripts`]) run in terminals of their own; services are scripts with a
//!   port, proxied by [`crate::api`].

pub mod keys;
pub mod procs;
pub mod scripts;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tokio::sync::{broadcast, watch};

use crate::now_iso;
use crate::process::Registry;
use procs::{Listen, Table};

const SCROLLBACK: usize = 10_000;
const OUTPUT_KEEP: usize = 1 << 20;
const KILL_GRACE: Duration = Duration::from_secs(3);
/// How often ports and health are refreshed.
pub const TICK: Duration = Duration::from_secs(5);
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_COLS: u16 = 80;
/// Pasted files (images, documents) up to this size.
pub const PASTE_LIMIT: usize = 8 << 20;

/// A terminal as clients see it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalView {
    pub id: String,
    pub name: String,
    pub title: String,
    pub cwd: String,
    pub workspace_id: String,
    pub agent_id: Option<String>,
    pub owner: String,
    pub script_name: Option<String>,
    pub command: String,
    pub rows: u16,
    pub cols: u16,
    pub pid: Option<u32>,
    pub status: &'static str,
    pub exit_code: Option<i32>,
    pub activity: Option<String>,
    pub ports: Vec<Listen>,
    pub created_at: String,
    pub exited_at: Option<String>,
}

/// What the hub tells the rest of the daemon (turned into bus events by the manager).
#[derive(Debug, Clone)]
pub enum HubEvent {
    Terminal(Box<TerminalView>),
    Removed { terminal_id: String, workspace_id: String },
    Scripts { workspace_id: String, scripts: Vec<serde_json::Value> },
    Attention { terminal_id: String, workspace_id: String, reason: String, title: String, body: String },
}

pub type Publish = Arc<dyn Fn(HubEvent) + Send + Sync>;

/// What to start.
#[derive(Debug, Clone, Default)]
pub struct Spawn {
    pub name: String,
    pub cwd: PathBuf,
    pub workspace_id: String,
    pub agent_id: Option<String>,
    pub owner: String,
    pub script_name: Option<String>,
    /// `None`: the owner's shell (`$SHELL`, else `/bin/bash`).
    pub program: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub rows: u16,
    pub cols: u16,
}

/// A chunk of output, or the end.
#[derive(Debug, Clone)]
pub enum Frame {
    /// Raw bytes; `end` is the offset after them.
    Output { end: u64, bytes: Arc<Vec<u8>> },
    Exit { code: Option<i32> },
}

/// Where a viewer starts.
pub enum Start {
    /// The screen as it is now (ANSI), at `offset`.
    Snapshot { rows: u16, cols: u16, data: String, offset: u64 },
    /// The bytes after the viewer's offset.
    Replay { bytes: Vec<u8>, end: u64 },
}

struct Out {
    buf: VecDeque<u8>,
    /// Offset of `buf[0]`.
    start: u64,
}

impl Out {
    fn end(&self) -> u64 {
        self.start + self.buf.len() as u64
    }
}

struct State {
    title: String,
    rows: u16,
    cols: u16,
    exit: Option<Option<i32>>,
    exited_at: Option<String>,
    activity: Option<String>,
    ports: Vec<Listen>,
}

pub struct Term {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub workspace_id: String,
    pub agent_id: Option<String>,
    pub owner: String,
    pub script_name: Option<String>,
    pub command: String,
    pub created_at: String,
    pub pid: Option<u32>,
    /// The per-terminal capability for `lagent hooks` (`PASEO_ACTIVITY_TOKEN`).
    token: String,
    state: Mutex<State>,
    screen: Mutex<vt100::Parser>,
    out: Mutex<Out>,
    tx: broadcast::Sender<Frame>,
    exited: watch::Sender<bool>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Term {
    pub fn running(&self) -> bool {
        lock(&self.state).exit.is_none()
    }

    pub fn view_with(&self, ports: Vec<Listen>) -> TerminalView {
        let st = lock(&self.state);
        TerminalView {
            id: self.id.clone(),
            name: self.name.clone(),
            title: st.title.clone(),
            cwd: self.cwd.clone(),
            workspace_id: self.workspace_id.clone(),
            agent_id: self.agent_id.clone(),
            owner: self.owner.clone(),
            script_name: self.script_name.clone(),
            command: self.command.clone(),
            rows: st.rows,
            cols: st.cols,
            pid: self.pid,
            status: if st.exit.is_none() { "running" } else { "exited" },
            exit_code: st.exit.flatten(),
            activity: st.activity.clone(),
            ports,
            created_at: self.created_at.clone(),
            exited_at: st.exited_at.clone(),
        }
    }

    /// The view with the ports last seen (no `/proc` scan).
    pub fn view(&self) -> TerminalView {
        let ports = lock(&self.state).ports.clone();
        self.view_with(ports)
    }

    pub fn exit_code(&self) -> Option<Option<i32>> {
        lock(&self.state).exit
    }

    pub fn input_mode(&self) -> keys::InputMode {
        keys::InputMode { application_cursor: lock(&self.screen).screen().application_cursor() }
    }

    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        if !self.running() {
            return Err(format!("Terminal {} has exited", self.id));
        }
        let mut w = lock(&self.writer);
        w.write_all(data).and_then(|_| w.flush()).map_err(|e| e.to_string())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        let (rows, cols) = (rows.clamp(2, 500), cols.clamp(10, 1000));
        lock(&self.master).resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
        lock(&self.screen).set_size(rows, cols);
        let mut st = lock(&self.state);
        st.rows = rows;
        st.cols = cols;
        Ok(())
    }

    /// Paseo `captureTerminalLines`: scrollback then screen, one string per row
    /// without trailing blanks; `start`/`end` are line indexes (negative counts from
    /// the end), clamped. `ansi` keeps colours and attributes (Paseo's cells carry
    /// none, so its `--ansi` is plain text too).
    pub fn capture(&self, start: Option<i64>, end: Option<i64>, ansi: bool) -> (Vec<String>, usize) {
        let all = {
            let mut p = lock(&self.screen);
            all_lines(&mut p, ansi)
        };
        let total = all.len();
        if total == 0 {
            return (Vec::new(), 0);
        }
        let index = |n: Option<i64>, default: usize| -> usize {
            match n {
                None => default,
                Some(n) => {
                    let i = if n < 0 { total as i64 + n } else { n };
                    i.clamp(0, total as i64 - 1) as usize
                }
            }
        };
        let (s, e) = (index(start, 0), index(end, total - 1));
        if s > e {
            return (Vec::new(), total);
        }
        (all[s..=e].to_vec(), total)
    }

    /// Where a viewer that last saw `offset` starts, and the live frames after it.
    /// The output lock is held while subscribing, so nothing is lost or repeated.
    pub fn attach(&self, offset: Option<u64>) -> (Start, broadcast::Receiver<Frame>) {
        let out = lock(&self.out);
        let rx = self.tx.subscribe();
        let end = out.end();
        if let Some(o) = offset.filter(|o| *o >= out.start && *o <= end) {
            let skip = (o - out.start) as usize;
            let bytes: Vec<u8> = out.buf.iter().skip(skip).copied().collect();
            return (Start::Replay { bytes, end }, rx);
        }
        let p = lock(&self.screen);
        let (rows, cols) = p.screen().size();
        let data = String::from_utf8_lossy(&p.screen().state_formatted()).to_string();
        (Start::Snapshot { rows, cols, data, offset: end }, rx)
    }

    pub fn exited(&self) -> watch::Receiver<bool> {
        self.exited.subscribe()
    }
}

/// Scrollback and screen rows (`vt100` shows scrollback one screenful at a time).
fn all_lines(p: &mut vt100::Parser, ansi: bool) -> Vec<String> {
    let (rows, cols) = p.screen().size();
    p.set_scrollback(usize::MAX);
    let back = p.screen().scrollback();
    let mut lines = vec![String::new(); back + rows as usize];
    let mut k = back;
    loop {
        p.set_scrollback(k);
        let top = back - k;
        let screen = p.screen();
        if ansi {
            for (i, r) in screen.rows_formatted(0, cols).enumerate() {
                lines[top + i] = String::from_utf8_lossy(&r).trim_end().to_string();
            }
        } else {
            for (i, r) in screen.rows(0, cols).enumerate() {
                lines[top + i] = r.trim_end().to_string();
            }
        }
        if k == 0 {
            break;
        }
        k = k.saturating_sub(rows as usize);
    }
    p.set_scrollback(0);
    lines
}

/// Settings from the daemon.
#[derive(Debug, Clone)]
pub struct Config {
    /// Pasted files go under `<dir>/<terminal id>/`.
    pub uploads: PathBuf,
    /// The daemon socket, for `lagent hooks` inside terminals.
    pub socket: Option<PathBuf>,
    /// Scripts run in `systemd-run --user --scope -p CPUQuota=<this>` (None: no scope).
    pub cpu_quota: Option<String>,
    /// Public base of the service proxy (`LUCINA_AGENTD_PUBLIC_URL`, e.g. the Secretary URL).
    pub public_url: Option<String>,
    /// agentd's loopback TCP port, for `<label>.localhost:<port>` service names.
    pub listen_port: Option<u16>,
}

impl Default for Config {
    /// No scope, no public URL; uploads under the temp directory (tests).
    fn default() -> Self {
        Config { uploads: std::env::temp_dir().join("lucina-agentd-terminal-uploads"), socket: None, cpu_quota: None, public_url: None, listen_port: None }
    }
}

impl Config {
    pub fn from_env(state_dir: &Path) -> Self {
        let scope = std::env::var("LUCINA_AGENTD_SCRIPT_SCOPE").unwrap_or_default();
        let cpu_quota = match scope.as_str() {
            "off" | "0" | "false" => None,
            _ if crate::process::resolve_program("systemd-run").is_none() => None,
            _ => Some(std::env::var("LUCINA_AGENTD_SCRIPT_CPU_QUOTA").ok().filter(|q| valid_quota(q)).unwrap_or_else(|| "200%".into())),
        };
        Config {
            uploads: state_dir.join("terminal-uploads"),
            socket: None,
            cpu_quota,
            public_url: std::env::var("LUCINA_AGENTD_PUBLIC_URL").ok().map(|u| u.trim_end_matches('/').to_string()).filter(|u| u.starts_with("https://") || u.starts_with("http://")),
            listen_port: None,
        }
    }
}

fn valid_quota(q: &str) -> bool {
    q.strip_suffix('%').is_some_and(|n| !n.is_empty() && n.len() <= 5 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The terminals and scripts.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

struct Inner {
    terms: Mutex<BTreeMap<String, Arc<Term>>>,
    runs: Mutex<HashMap<(String, String), scripts::Run>>,
    publish: Publish,
    registry: Option<Registry>,
    config: Mutex<Config>,
}

fn new_id() -> String {
    format!("term_{}", &uuid::Uuid::new_v4().simple().to_string()[..16])
}

/// `lagent` next to the running binary (the daemon's install dir), else on `PATH`.
fn hook_cli() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("lagent"))).filter(|p| p.is_file());
    beside.or_else(|| crate::process::resolve_program("lagent"))
}

fn default_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| s.starts_with('/') && Path::new(s).exists()).unwrap_or_else(|| "/bin/bash".into())
}

impl Hub {
    pub fn new(config: Config, registry: Option<Registry>, publish: Publish) -> Self {
        Hub { inner: Arc::new(Inner { terms: Mutex::new(BTreeMap::new()), runs: Mutex::new(HashMap::new()), publish, registry, config: Mutex::new(config) }) }
    }

    pub fn config(&self) -> Config {
        lock(&self.inner.config).clone()
    }

    /// Set at startup, once the daemon knows its socket and TCP port.
    pub fn configure(&self, f: impl FnOnce(&mut Config)) {
        f(&mut lock(&self.inner.config));
    }

    fn publish(&self, e: HubEvent) {
        (self.inner.publish)(e)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Term>> {
        lock(&self.inner.terms).get(id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Term>> {
        lock(&self.inner.terms).values().cloned().collect()
    }

    /// Id, unique id prefix, or unique (then partly matching) name, among `candidates`
    /// (Paseo `resolveTerminalIdentifier`).
    pub fn resolve_in(candidates: &[Arc<Term>], query: &str) -> Option<Arc<Term>> {
        if query.is_empty() {
            return None;
        }
        if let Some(t) = candidates.iter().find(|t| t.id == query) {
            return Some(t.clone());
        }
        let q = query.to_lowercase();
        let one = |v: Vec<&Arc<Term>>| if v.len() == 1 { Some(v[0].clone()) } else { None };
        let prefix: Vec<&Arc<Term>> = candidates.iter().filter(|t| t.id.to_lowercase().starts_with(&q)).collect();
        if !prefix.is_empty() {
            return one(prefix);
        }
        one(candidates.iter().filter(|t| t.name.to_lowercase() == q).collect())
            .or_else(|| one(candidates.iter().filter(|t| t.name.to_lowercase().contains(&q)).collect()))
    }

    /// The session leaders of the running terminals (agent processes, for [`crate::peer`]).
    pub fn session_pids(&self) -> HashSet<u32> {
        lock(&self.inner.terms).values().filter(|t| t.running()).filter_map(|t| t.pid).collect()
    }

    pub fn by_pid(&self, pid: u32) -> Option<Arc<Term>> {
        lock(&self.inner.terms).values().find(|t| t.pid == Some(pid)).cloned()
    }

    /// Views with fresh ports (one `/proc` scan for all of them).
    pub fn views(&self, terms: &[Arc<Term>]) -> Vec<TerminalView> {
        let table = Table::read();
        terms.iter().map(|t| t.view_with(self.ports_of(&table, t))).collect()
    }

    fn ports_of(&self, table: &Table, t: &Term) -> Vec<Listen> {
        match (t.running(), t.pid) {
            (true, Some(pid)) => procs::listening(&table.members(&[pid].into(), &HashSet::new())),
            _ => Vec::new(),
        }
    }

    pub fn spawn(&self, s: Spawn) -> Result<Arc<Term>, String> {
        let id = new_id();
        let config = self.config();
        let rows = if s.rows == 0 { DEFAULT_ROWS } else { s.rows.clamp(2, 500) };
        let cols = if s.cols == 0 { DEFAULT_COLS } else { s.cols.clamp(10, 1000) };
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| format!("cannot open a terminal: {e}"))?;
        let program = s.program.clone().unwrap_or_else(default_shell);
        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&s.args);
        cmd.cwd(&s.cwd);
        cmd.env_remove(crate::notify::TOKEN_ENV);
        cmd.env_remove("LUCINA_AGENT_ID");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let token = uuid::Uuid::new_v4().simple().to_string();
        for (k, v) in [
            ("LUCINA_TERMINAL_ID", id.as_str()),
            ("PASEO_TERMINAL_ID", id.as_str()),
            ("PASEO_ACTIVITY_TOKEN", token.as_str()),
            ("LUCINA_WORKSPACE_ID", s.workspace_id.as_str()),
            ("PASEO_WORKSPACE_ID", s.workspace_id.as_str()),
        ] {
            cmd.env(k, v);
        }
        if let Some(sock) = &config.socket {
            cmd.env(crate::config::SOCKET_ENV, sock);
        }
        // What Paseo's terminal agent hooks call: `$PASEO_HOOK_CLI hooks <cli> <event>`.
        if let Some(cli) = hook_cli() {
            cmd.env("PASEO_HOOK_CLI", &cli);
            cmd.env("LUCINA_HOOK_CLI", &cli);
        }
        if let Some(port) = config.listen_port {
            cmd.env("PASEO_TERMINAL_ACTIVITY_URL", format!("http://127.0.0.1:{port}/terminal-activity"));
        }
        for (k, v) in &s.env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| format!("cannot start {program}: {e}"))?;
        drop(pair.slave);
        let pid = child.process_id();
        if let (Some(r), Some(p)) = (&self.inner.registry, pid) {
            r.track(p, &program);
        }
        let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let mut command = std::iter::once(program.clone()).chain(s.args.iter().cloned()).collect::<Vec<_>>().join(" ");
        if command.len() > 500 {
            command = format!("{}…", command.chars().take(500).collect::<String>());
        }
        let term = Arc::new(Term {
            id: id.clone(),
            name: if s.name.trim().is_empty() { format!("Terminal {}", lock(&self.inner.terms).len() + 1) } else { s.name.trim().chars().take(80).collect() },
            cwd: s.cwd.display().to_string(),
            workspace_id: s.workspace_id.clone(),
            agent_id: s.agent_id.clone(),
            owner: s.owner.clone(),
            script_name: s.script_name.clone(),
            command,
            created_at: now_iso(),
            pid,
            token,
            state: Mutex::new(State { title: s.name.trim().chars().take(80).collect(), rows, cols, exit: None, exited_at: None, activity: None, ports: Vec::new() }),
            screen: Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)),
            out: Mutex::new(Out { buf: VecDeque::new(), start: 0 }),
            tx: broadcast::channel(256).0,
            exited: watch::channel(false).0,
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer: Mutex::new(child.clone_killer()),
        });
        if lock(&term.state).title.is_empty() {
            lock(&term.state).title = term.name.clone();
        }
        lock(&self.inner.terms).insert(id.clone(), term.clone());
        self.read_output(term.clone(), reader);
        self.wait_exit(term.clone(), child);
        self.publish(HubEvent::Terminal(Box::new(term.view())));
        Ok(term)
    }

    fn read_output(&self, term: Arc<Term>, mut reader: Box<dyn Read + Send>) {
        std::thread::Builder::new()
            .name(format!("pty-{}", term.id))
            .spawn(move || {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let bytes = &buf[..n];
                    lock(&term.screen).process(bytes);
                    let mut out = lock(&term.out);
                    out.buf.extend(bytes);
                    let over = out.buf.len().saturating_sub(OUTPUT_KEEP);
                    if over > 0 {
                        out.buf.drain(..over);
                        out.start += over as u64;
                    }
                    let _ = term.tx.send(Frame::Output { end: out.end(), bytes: Arc::new(bytes.to_vec()) });
                }
            })
            .ok();
    }

    fn wait_exit(&self, term: Arc<Term>, mut child: Box<dyn portable_pty::Child + Send + Sync>) {
        let hub = self.clone();
        std::thread::Builder::new()
            .name(format!("pty-wait-{}", term.id))
            .spawn(move || {
                let code = child.wait().ok().and_then(|s| if s.signal().is_some() { None } else { i32::try_from(s.exit_code()).ok() });
                // Give the reader a moment for the last output.
                std::thread::sleep(Duration::from_millis(50));
                {
                    let mut st = lock(&term.state);
                    st.exit = Some(code);
                    st.exited_at = Some(now_iso());
                    st.ports.clear();
                }
                if let (Some(r), Some(p)) = (&hub.inner.registry, term.pid) {
                    r.untrack(p);
                }
                let _ = term.tx.send(Frame::Exit { code });
                term.exited.send_replace(true);
                hub.on_exit(&term, code);
                hub.publish(HubEvent::Terminal(Box::new(term.view())));
            })
            .ok();
    }

    /// Paseo `worktree.terminals`: `[{name?, command}]` from `paseo.json`, each opened in
    /// the owner's shell with its command typed in followed by `\r`.
    pub fn open_configured(&self, workspace_id: &str, cwd: &Path) {
        let cfg = crate::workspaces::setup::read_config(cwd);
        for (i, t) in cfg["worktree"]["terminals"].as_array().into_iter().flatten().enumerate() {
            let Some(command) = t["command"].as_str().map(str::trim).filter(|c| !c.is_empty()) else { continue };
            let name = t["name"].as_str().map(str::to_string).unwrap_or_else(|| format!("Terminal {}", i + 1));
            let spawned = self.spawn(Spawn { name, cwd: cwd.to_path_buf(), workspace_id: workspace_id.to_string(), owner: "owner".into(), ..Spawn::default() });
            match spawned {
                Ok(term) => {
                    if let Err(e) = term.write(format!("{command}\r").as_bytes()) {
                        tracing::warn!(workspace = workspace_id, error = %e, "worktree terminal: cannot type its command");
                    }
                }
                Err(e) => tracing::warn!(workspace = workspace_id, error = %e, "worktree terminal did not start"),
            }
        }
    }

    /// Kill a terminal's whole session and forget it. `true` when it existed.
    pub async fn kill(&self, id: &str) -> bool {
        let Some(term) = self.get(id) else { return false };
        stop_term(&term).await;
        lock(&self.inner.terms).remove(id);
        let _ = std::fs::remove_dir_all(self.config().uploads.join(id));
        self.publish(HubEvent::Removed { terminal_id: id.to_string(), workspace_id: term.workspace_id.clone() });
        true
    }

    /// Kill every terminal `pick` selects (agent stop, workspace archive, shutdown).
    pub async fn kill_where(&self, pick: impl Fn(&Term) -> bool) -> usize {
        let ids: Vec<String> = lock(&self.inner.terms).values().filter(|t| pick(t)).map(|t| t.id.clone()).collect();
        let mut set = tokio::task::JoinSet::new();
        for id in &ids {
            let hub = self.clone();
            let id = id.clone();
            set.spawn(async move { hub.kill(&id).await });
        }
        while set.join_next().await.is_some() {}
        ids.len()
    }

    pub fn rename(&self, id: &str, title: &str) -> Option<TerminalView> {
        let t = self.get(id)?;
        lock(&t.state).title = title.trim().chars().take(80).collect();
        let v = t.view();
        self.publish(HubEvent::Terminal(Box::new(v.clone())));
        Some(v)
    }

    pub fn resized(&self, t: &Term) {
        self.publish(HubEvent::Terminal(Box::new(t.view())));
    }

    /// `lagent hooks`: a terminal's agent CLI reports running / idle / needs-input.
    pub fn activity(&self, id: &str, token: &str, state: &str) -> bool {
        let Some(t) = self.get(id) else { return false };
        if t.token.len() != token.len() || !t.token.bytes().zip(token.bytes()).fold(true, |ok, (a, b)| ok & (a == b)) {
            return false;
        }
        if !matches!(state, "running" | "idle" | "needs-input") {
            return false;
        }
        let changed = {
            let mut st = lock(&t.state);
            let changed = st.activity.as_deref() != Some(state);
            st.activity = Some(state.to_string());
            changed
        };
        if changed {
            if state == "needs-input" {
                self.publish(HubEvent::Attention {
                    terminal_id: t.id.clone(),
                    workspace_id: t.workspace_id.clone(),
                    reason: "needs-input".into(),
                    title: "Terminal needs input".into(),
                    body: lock(&t.state).title.clone(),
                });
            }
            self.publish(HubEvent::Terminal(Box::new(t.view())));
        }
        true
    }

    /// Save a pasted or dropped file and type its quoted path into the terminal.
    pub fn paste(&self, t: &Term, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        if bytes.len() > PASTE_LIMIT {
            return Err(format!("Pasted files are limited to {} MB", PASTE_LIMIT >> 20));
        }
        let clean: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).take(80).collect();
        let clean = clean.trim_start_matches('.');
        let clean = if clean.is_empty() { "paste.bin".to_string() } else { clean.to_string() };
        let dir = self.config().uploads.join(&t.id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f");
        let path = dir.join(format!("{stamp}-{clean}"));
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        t.write(format!("{} ", shell_quote(&path.display().to_string())).as_bytes())?;
        Ok(path)
    }

    /// Refresh ports, script health and lifecycles; publish what changed. Run every
    /// few seconds.
    pub async fn tick(&self) {
        let running: Vec<Arc<Term>> = self.all().into_iter().filter(|t| t.running()).collect();
        if running.is_empty() {
            return;
        }
        let me = self.clone();
        let scanned = tokio::task::spawn_blocking(move || {
            let table = Table::read();
            running.into_iter().map(|t| {
                let ports = me.ports_of(&table, &t);
                (t, ports)
            }).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (t, ports) in scanned {
            let changed = {
                let mut st = lock(&t.state);
                let changed = st.ports != ports;
                st.ports = ports;
                changed
            };
            if changed {
                self.publish(HubEvent::Terminal(Box::new(t.view())));
            }
        }
        self.check_health().await;
    }

    /// Kill everything (daemon shutdown).
    pub async fn shutdown(&self) {
        self.kill_where(|_| true).await;
    }

    /// The decoded `{b64}` frame body.
    pub fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }
}

async fn stop_term(term: &Term) {
    if !term.running() {
        return;
    }
    let Some(pid) = term.pid else { return };
    let mut exited = term.exited();
    procs::signal_session(pid, "HUP");
    procs::signal_session(pid, "TERM");
    if tokio::time::timeout(KILL_GRACE, exited.wait_for(|e| *e)).await.is_err() {
        procs::signal_session(pid, "KILL");
        let _ = lock(&term.killer).kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), exited.wait_for(|e| *e)).await;
    }
    // Anything still in the session after its leader exited (background jobs).
    procs::signal_session(pid, "KILL");
}

/// Single-quote for a POSIX shell.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> (Hub, tempfile::TempDir, Arc<Mutex<Vec<HubEvent>>>) {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let hub = Hub::new(Config { uploads: dir.path().join("up"), ..Config::default() }, None, Arc::new(move |e| lock(&s).push(e)));
        (hub, dir, seen)
    }

    async fn until(what: &str, t: &Term, f: impl Fn() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}: {:?}", t.capture(None, None, false).0);
    }

    #[tokio::test]
    async fn keys_capture_resize_and_kill() {
        let (hub, dir, seen) = hub();
        let t = hub
            .spawn(Spawn { cwd: dir.path().into(), workspace_id: "wks_1".into(), owner: "owner".into(), program: Some("/bin/sh".into()), ..Spawn::default() })
            .unwrap();
        until("a prompt", &t, || t.capture(None, None, false).0.iter().any(|l| l.starts_with('$'))).await;
        let line = keys::resolve("echo lucina-$((6*7))", false, t.input_mode()) + &keys::resolve("Enter", false, t.input_mode());
        t.write(line.as_bytes()).unwrap();
        until("lucina-42", &t, || t.capture(None, None, false).0.iter().any(|l| l.ends_with("lucina-42"))).await;
        let (lines, total) = t.capture(None, None, false);
        assert_eq!(total, DEFAULT_ROWS as usize);
        assert!(lines.iter().any(|l| l.ends_with("echo lucina-$((6*7))")), "{lines:?}");
        let (last, _) = t.capture(Some(-1), None, false);
        assert_eq!(last.len(), 1);
        t.resize(30, 100).unwrap();
        assert_eq!((t.view().rows, t.view().cols), (30, 100));
        // A viewer that saw offset 0 gets everything again; a new one a snapshot.
        let (start, _rx) = t.attach(Some(0));
        assert!(matches!(start, Start::Replay { ref bytes, .. } if String::from_utf8_lossy(bytes).contains("lucina-42")));
        assert!(matches!(t.attach(None).0, Start::Snapshot { .. }));
        // A background job in the session dies with the terminal.
        t.write(b"sleep 300 & echo bg=$!\r").unwrap();
        until("bg=", &t, || t.capture(None, None, false).0.iter().any(|l| l.starts_with("bg="))).await;
        let bg: u32 = t.capture(None, None, false).0.iter().find_map(|l| l.strip_prefix("bg=").and_then(|p| p.parse().ok())).unwrap();
        assert!(hub.session_pids().contains(&t.pid.unwrap()));
        assert!(hub.kill(&t.id).await);
        until("bg gone", &t, || !Path::new(&format!("/proc/{bg}")).exists() || std::fs::read_to_string(format!("/proc/{bg}/stat")).is_ok_and(|s| s.contains(") Z "))).await;
        assert!(hub.get(&t.id).is_none());
        assert!(lock(&seen).iter().any(|e| matches!(e, HubEvent::Removed { .. })));
    }

    #[tokio::test]
    async fn exit_codes_and_activity() {
        let (hub, dir, seen) = hub();
        let t = hub
            .spawn(Spawn { cwd: dir.path().into(), workspace_id: "w".into(), owner: "owner".into(), program: Some("/bin/sh".into()), args: vec!["-c".into(), "echo $PASEO_TERMINAL_ID; exit 3".into()], ..Spawn::default() })
            .unwrap();
        let mut ex = t.exited();
        tokio::time::timeout(Duration::from_secs(5), ex.wait_for(|e| *e)).await.unwrap().unwrap();
        assert_eq!(t.exit_code(), Some(Some(3)));
        assert_eq!(t.view().status, "exited");
        assert!(t.capture(None, None, false).0.iter().any(|l| l == &t.id));
        assert!(t.write(b"x").is_err());
        assert!(!hub.activity(&t.id, "wrong", "idle"));
        let token = t.token.clone();
        assert!(hub.activity(&t.id, &token, "needs-input"));
        assert!(!hub.activity(&t.id, &token, "bogus"));
        assert!(lock(&seen).iter().any(|e| matches!(e, HubEvent::Attention { .. })));
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }
}
