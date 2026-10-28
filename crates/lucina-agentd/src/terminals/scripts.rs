//! Workspace scripts and services (Paseo `utils/worktree.js getScriptConfigs`,
//! `workspace-service-port-allocator.js`, `workspace-service-env.js`,
//! `script-status-projection.js`, `script-health-monitor.js`).
//!
//! - Config: `paseo.json` `scripts.<name> = {command, type?: "service", port?}`, read
//!   unchanged, then `.lucina/agents.toml` `[scripts.<name>]` with `cmd` (or
//!   `command`), `port = "auto" | <n>`, `health = "/path"`, `type`. A port or a health
//!   path makes a service. A name in both files takes the TOML entry.
//! - Ports: the script's own `port`, else `worktree.servicePorts` (`portScript`: no
//!   shell, 10 s, args `name workspace branch cwd`, one port on stdout; `range`), else
//!   Lucina's range 4100-4999. A port another running script holds is never handed
//!   out twice; each candidate is probed on 127.0.0.1.
//! - A script runs in its own terminal as `/bin/sh -c <command>`, inside
//!   `systemd-run --user --scope -p CPUQuota=…` when systemd-run is there (owner's
//!   rule for background CPU work). Paseo types the command into a shell and keeps the
//!   shell; here the terminal ends with the script, and its output stays capturable.
//! - Env: setup's (`PASEO_WORKTREE_PATH`, …), `PASEO_WORKSPACE_ID`,
//!   `PASEO_SCRIPTNAME`; services also `HOST`, `PASEO_PORT` (and `PORT`), `PASEO_URL`
//!   and `PASEO_SERVICE_<NAME>_{PORT,URL}` for each running service of the workspace,
//!   with `LUCINA_*` aliases and `LUCINA_SERVICE_PATH`.
//! - Health: services with a `health` path get an HTTP GET (< 400 is healthy); others
//!   a TCP connect, as Paseo. `null` until the first check.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{lock, Hub, HubEvent, Spawn, Term};
use crate::workspaces::setup;

pub const LUCINA_CONFIG: &str = ".lucina/agents.toml";
pub const DEFAULT_RANGE: (u16, u16) = (4100, 4999);
const PORT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DNS_LABEL: usize = 63;

#[derive(Debug, Clone, PartialEq)]
pub struct ScriptConfig {
    pub name: String,
    pub command: String,
    pub service: bool,
    pub port: Option<u16>,
    pub health: Option<String>,
    pub source: &'static str,
}

/// The scripts configured in `dir`, by name.
pub fn configs(dir: &Path) -> BTreeMap<String, ScriptConfig> {
    let mut out = BTreeMap::new();
    let paseo = setup::read_config(dir);
    if let Some(scripts) = paseo["scripts"].as_object() {
        for (name, entry) in scripts {
            let Some(command) = entry["command"].as_str().map(str::trim).filter(|c| !c.is_empty()) else { continue };
            let service = entry["type"] == "service";
            let port = if service { entry["port"].as_f64().filter(|p| p.fract() == 0.0 && (1.0..=65535.0).contains(p)).map(|p| p as u16) } else { None };
            out.insert(name.clone(), ScriptConfig { name: name.clone(), command: command.to_string(), service, port, health: None, source: "paseo.json" });
        }
    }
    let toml: Option<toml::Value> = std::fs::read_to_string(dir.join(LUCINA_CONFIG)).ok().and_then(|t| toml::from_str(&t).ok());
    if let Some(scripts) = toml.as_ref().and_then(|t| t.get("scripts")).and_then(|s| s.as_table()) {
        for (name, entry) in scripts {
            let get = |k: &str| entry.get(k);
            let Some(command) = get("cmd").or_else(|| get("command")).and_then(|c| c.as_str()).map(str::trim).filter(|c| !c.is_empty()) else { continue };
            let port_value = get("port");
            let port = port_value.and_then(|p| p.as_integer()).and_then(|p| u16::try_from(p).ok()).filter(|p| *p > 0);
            let auto = port_value.and_then(|p| p.as_str()) == Some("auto");
            let health = get("health").and_then(|h| h.as_str()).filter(|h| h.starts_with('/') && h.len() <= 200).map(str::to_string);
            let service = port.is_some() || auto || health.is_some() || get("type").and_then(|t| t.as_str()) == Some("service");
            out.insert(name.clone(), ScriptConfig { name: name.clone(), command: command.to_string(), service, port, health, source: LUCINA_CONFIG });
        }
    }
    out
}

/// Paseo `toHostnameLabel`.
fn hostname_label(v: &str) -> String {
    let mut out = String::new();
    for c in v.to_lowercase().chars().filter(|c| !('\u{300}'..='\u{36f}').contains(c)) {
        let c = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        };
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    if t.is_empty() {
        "untitled".into()
    } else {
        t.to_string()
    }
}

fn sha_hex(s: &str, n: usize) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>()[..n].to_string()
}

/// Paseo `buildServiceProxyLabel`: `<script>[--<branch>]--<project>`, capped at a DNS label.
pub fn proxy_label(project_slug: &str, branch: Option<&str>, script: &str) -> String {
    let mut parts = vec![hostname_label(script)];
    if let Some(b) = branch.filter(|b| !matches!(*b, "main" | "master")) {
        parts.push(hostname_label(b));
    }
    parts.push(hostname_label(project_slug));
    let label = parts.join("--");
    if label.len() <= MAX_DNS_LABEL {
        return label;
    }
    let suffix = sha_hex(&label, 8);
    let keep = MAX_DNS_LABEL - suffix.len() - 2;
    let prefix = label[..keep].trim_end_matches('-');
    format!("{}--{suffix}", if prefix.is_empty() { "svc" } else { prefix })
}

/// Paseo `deriveProjectServiceSlug`.
pub fn project_service_slug(root_path: &str, project_id: &str) -> String {
    let base = Path::new(root_path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let slug = lucina_agents::branch_slug::slugify(&base);
    format!("{}-{}", if slug.is_empty() { "untitled".into() } else { slug }, sha_hex(project_id, 8))
}

pub fn service_path(workspace_id: &str, script: &str) -> String {
    format!("/v1/agents/workspaces/{workspace_id}/services/{script}/")
}

/// What a workspace's scripts need to know about it.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub workspace_id: String,
    pub cwd: PathBuf,
    /// The main checkout (setup's `PASEO_SOURCE_CHECKOUT_PATH`).
    pub source: PathBuf,
    pub branch: Option<String>,
    pub project_slug: String,
    pub worktree_port: Option<u16>,
}

/// A script that runs or ran since the daemon started.
#[derive(Debug, Clone)]
pub struct Run {
    pub service: bool,
    pub running: bool,
    pub terminal_id: String,
    pub exit_code: Option<i32>,
    pub port: Option<u16>,
    pub health_path: Option<String>,
    pub health: Option<&'static str>,
    pub ctx: Ctx,
}

struct Urls {
    local: Option<String>,
    public: Option<String>,
    proxy: Option<String>,
}

impl Hub {
    fn urls(&self, ctx: &Ctx, script: &str) -> Urls {
        let cfg = self.config();
        let label = proxy_label(&ctx.project_slug, ctx.branch.as_deref(), script);
        let local = cfg.listen_port.map(|p| format!("http://{label}.localhost:{p}"));
        let public = cfg.public_url.as_ref().map(|u| format!("{u}{}", service_path(&ctx.workspace_id, script)));
        let proxy = public.clone().or_else(|| local.clone()).or_else(|| Some(service_path(&ctx.workspace_id, script)));
        Urls { local, public, proxy }
    }

    /// Paseo's script payloads for a workspace (configured, then orphaned runs), by name.
    pub fn scripts(&self, ctx: &Ctx) -> Vec<Value> {
        let cfgs = configs(&ctx.cwd);
        let runs = lock(&self.inner.runs);
        let mut out = Vec::new();
        for (name, c) in &cfgs {
            let run = runs.get(&(ctx.workspace_id.clone(), name.clone()));
            out.push(self.payload(ctx, name, Some(c), run));
        }
        for ((ws, name), run) in runs.iter() {
            if ws == &ctx.workspace_id && run.running && !cfgs.contains_key(name) {
                out.push(self.payload(ctx, name, None, Some(run)));
            }
        }
        out.sort_by_key(|v| v["scriptName"].as_str().unwrap_or_default().to_lowercase());
        out
    }

    fn payload(&self, ctx: &Ctx, name: &str, cfg: Option<&ScriptConfig>, run: Option<&Run>) -> Value {
        let service = cfg.map(|c| c.service).or(run.map(|r| r.service)).unwrap_or(false);
        let lifecycle = if run.is_some_and(|r| r.running) { "running" } else { "stopped" };
        let common = json!({
            "scriptName": name,
            "lifecycle": lifecycle,
            "exitCode": run.and_then(|r| r.exit_code),
            "terminalId": run.map(|r| r.terminal_id.clone()),
            "command": cfg.map(|c| c.command.clone()),
            "source": cfg.map(|c| c.source),
        });
        let mut v = common;
        if !service {
            v["type"] = json!("script");
            v["hostname"] = json!(name);
            v["port"] = Value::Null;
            v["proxyUrl"] = Value::Null;
            v["health"] = Value::Null;
            v["servicePath"] = Value::Null;
            return v;
        }
        let running = run.filter(|r| r.running);
        let urls = self.urls(ctx, name);
        v["type"] = json!("service");
        v["hostname"] = json!(format!("{}.localhost", proxy_label(&ctx.project_slug, ctx.branch.as_deref(), name)));
        v["port"] = json!(running.and_then(|r| r.port).or(cfg.and_then(|c| c.port)));
        v["localProxyUrl"] = json!(urls.local);
        v["publicProxyUrl"] = json!(urls.public);
        v["proxyUrl"] = json!(urls.proxy);
        v["health"] = json!(running.and_then(|r| r.health));
        v["servicePath"] = json!(service_path(&ctx.workspace_id, name));
        v
    }

    fn publish_scripts(&self, ctx: &Ctx) {
        let scripts = self.scripts(ctx);
        self.publish(HubEvent::Scripts { workspace_id: ctx.workspace_id.clone(), scripts });
    }

    fn reserved_ports(&self) -> HashSet<u16> {
        lock(&self.inner.runs).values().filter(|r| r.running).filter_map(|r| r.port).collect()
    }

    async fn allocate(&self, ctx: &Ctx, cfg: &ScriptConfig) -> Result<u16, String> {
        let reserved = self.reserved_ports();
        if let Some(p) = cfg.port {
            if reserved.contains(&p) {
                return Err(format!("Port {p} of script '{}' is used by another running script", cfg.name));
            }
            return Ok(p);
        }
        let ports = &setup::read_config(&ctx.cwd)["worktree"]["servicePorts"];
        if let Some(script) = ports["portScript"].as_str().filter(|s| !s.trim().is_empty()) {
            return port_from_script(script, ctx, &cfg.name, &reserved).await;
        }
        let range = match ports["range"].as_str() {
            Some(r) => parse_range(r)?,
            None => DEFAULT_RANGE,
        };
        port_in_range(range, &reserved)
    }

    /// Start a configured script. `owner` and `agent` go on its terminal.
    pub async fn start_script(&self, ctx: &Ctx, name: &str, owner: &str, agent: Option<String>) -> Result<Value, String> {
        let cfg = configs(&ctx.cwd).remove(name).ok_or_else(|| format!("Script '{name}' is not configured in paseo.json or {LUCINA_CONFIG}"))?;
        let key = (ctx.workspace_id.clone(), name.to_string());
        if lock(&self.inner.runs).get(&key).is_some_and(|r| r.running) {
            return Err(format!("Script '{name}' is already running"));
        }
        let port = if cfg.service { Some(self.allocate(ctx, &cfg).await?) } else { None };
        let mut env = setup::env(&ctx.source, &ctx.cwd, ctx.branch.as_deref().unwrap_or_default(), ctx.worktree_port);
        env.extend([
            ("PASEO_SCRIPTNAME".into(), name.to_string()),
            ("LUCINA_SCRIPT_NAME".into(), name.to_string()),
        ]);
        if let Some(p) = port {
            let mut peers: Vec<(String, u16)> = lock(&self.inner.runs)
                .iter()
                .filter(|((ws, n), r)| ws == &ctx.workspace_id && n != name && r.running && r.port.is_some())
                .map(|((_, n), r)| (n.clone(), r.port.unwrap_or_default()))
                .collect();
            peers.push((name.to_string(), p));
            let envs: Vec<String> = peers.iter().map(|(n, _)| service_env_name(n)).collect();
            if let Some(dup) = envs.iter().find(|e| envs.iter().filter(|x| x == e).count() > 1) {
                return Err(format!("Service env name collision for {dup}"));
            }
            let url = self.urls(ctx, name).proxy.unwrap_or_default();
            env.extend([
                ("HOST".into(), "127.0.0.1".into()),
                ("PASEO_PORT".into(), p.to_string()),
                ("LUCINA_PORT".into(), p.to_string()),
                ("PORT".into(), p.to_string()),
                ("PASEO_URL".into(), url.clone()),
                ("LUCINA_SERVICE_URL".into(), url),
                ("LUCINA_SERVICE_PATH".into(), service_path(&ctx.workspace_id, name)),
            ]);
            for (n, port) in &peers {
                let e = service_env_name(n);
                let u = self.urls(ctx, n).proxy.unwrap_or_default();
                env.push((format!("PASEO_SERVICE_{e}_PORT"), port.to_string()));
                env.push((format!("PASEO_SERVICE_{e}_URL"), u.clone()));
                env.push((format!("LUCINA_SERVICE_{e}_PORT"), port.to_string()));
                env.push((format!("LUCINA_SERVICE_{e}_URL"), u));
            }
        }
        let (program, args) = match self.config().cpu_quota {
            Some(q) => (
                "systemd-run".to_string(),
                vec!["--user".into(), "--scope".into(), "--quiet".into(), "--collect".into(), "-p".into(), format!("CPUQuota={q}"), "--".into(), "/bin/sh".into(), "-c".into(), cfg.command.clone()],
            ),
            None => ("/bin/sh".to_string(), vec!["-c".into(), cfg.command.clone()]),
        };
        let term = self.spawn(Spawn {
            name: name.to_string(),
            cwd: ctx.cwd.clone(),
            workspace_id: ctx.workspace_id.clone(),
            agent_id: agent,
            owner: owner.to_string(),
            script_name: Some(name.to_string()),
            program: Some(program),
            args,
            env,
            ..Spawn::default()
        })?;
        lock(&self.inner.runs).insert(
            key,
            Run { service: cfg.service, running: term.running(), terminal_id: term.id.clone(), exit_code: term.exit_code().flatten(), port, health_path: cfg.health.clone(), health: None, ctx: ctx.clone() },
        );
        self.publish_scripts(ctx);
        self.scripts(ctx).into_iter().find(|s| s["scriptName"] == name).ok_or_else(|| format!("Script '{name}' did not produce a status record"))
    }

    pub async fn stop_script(&self, ctx: &Ctx, name: &str) -> Result<Value, String> {
        let run = lock(&self.inner.runs).get(&(ctx.workspace_id.clone(), name.to_string())).cloned();
        let run = run.filter(|r| r.running).ok_or_else(|| format!("Script '{name}' is not running"))?;
        let Some(term) = self.get(&run.terminal_id) else { return Err(format!("Terminal for script '{name}' is no longer available")) };
        super::stop_term(&term).await;
        self.mark_stopped(&term.id, term.exit_code().flatten());
        self.publish_scripts(ctx);
        self.scripts(ctx).into_iter().find(|s| s["scriptName"] == name).ok_or_else(|| format!("Script '{name}' did not produce a status record"))
    }

    fn mark_stopped(&self, terminal_id: &str, code: Option<i32>) -> Option<Ctx> {
        let mut runs = lock(&self.inner.runs);
        let run = runs.values_mut().find(|r| r.terminal_id == terminal_id && r.running)?;
        run.running = false;
        run.exit_code = code;
        run.health = None;
        Some(run.ctx.clone())
    }

    pub(super) fn on_exit(&self, term: &Term, code: Option<i32>) {
        if let Some(ctx) = self.mark_stopped(&term.id, code) {
            self.publish_scripts(&ctx);
        }
    }

    /// The port of a running service of a workspace.
    pub fn service_port(&self, workspace_id: &str, name: &str) -> Option<u16> {
        lock(&self.inner.runs).get(&(workspace_id.to_string(), name.to_string())).filter(|r| r.running && r.service).and_then(|r| r.port)
    }

    /// Running services by `.localhost` label (the host-routed proxy).
    pub fn port_for_label(&self, label: &str) -> Option<u16> {
        lock(&self.inner.runs)
            .iter()
            .filter(|(_, r)| r.running && r.service)
            .find(|((_, name), r)| proxy_label(&r.ctx.project_slug, r.ctx.branch.as_deref(), name) == label)
            .and_then(|(_, r)| r.port)
    }

    /// Running services of a workspace: `(name, port, terminal, health)`.
    pub fn running_services(&self, workspace_id: &str) -> Vec<(String, u16, String, Option<&'static str>)> {
        let mut v: Vec<_> = lock(&self.inner.runs)
            .iter()
            .filter(|((ws, _), r)| ws == workspace_id && r.running && r.service)
            .filter_map(|((_, n), r)| Some((n.clone(), r.port?, r.terminal_id.clone(), r.health)))
            .collect();
        v.sort();
        v
    }

    /// Stop a workspace's scripts and kill its terminals (workspace archive).
    pub async fn stop_workspace(&self, workspace_id: &str) -> usize {
        let n = self.kill_where(|t| t.workspace_id == workspace_id).await;
        lock(&self.inner.runs).retain(|(ws, _), _| ws != workspace_id);
        n
    }

    pub(super) async fn check_health(&self) {
        let targets: Vec<((String, String), u16, Option<String>)> =
            lock(&self.inner.runs).iter().filter(|(_, r)| r.running && r.service).filter_map(|(k, r)| Some((k.clone(), r.port?, r.health_path.clone()))).collect();
        for (key, port, path) in targets {
            let healthy = match &path {
                Some(p) => http_ok(port, p).await,
                None => tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect(("127.0.0.1", port))).await.is_ok_and(|r| r.is_ok()),
            };
            let health = if healthy { "healthy" } else { "unhealthy" };
            let ctx = {
                let mut runs = lock(&self.inner.runs);
                match runs.get_mut(&key) {
                    Some(r) if r.running && r.health != Some(health) => {
                        r.health = Some(health);
                        Some(r.ctx.clone())
                    }
                    _ => None,
                }
            };
            if let Some(ctx) = ctx {
                self.publish_scripts(&ctx);
            }
        }
    }
}

async fn http_ok(port: u16, path: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(2)).redirect(reqwest::redirect::Policy::none()).build() else { return false };
    client.get(format!("http://127.0.0.1:{port}{path}")).send().await.is_ok_and(|r| r.status().as_u16() < 400)
}

/// Paseo `normalizeServiceEnvName`.
pub fn service_env_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_uppercase().chars() {
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn parse_range(r: &str) -> Result<(u16, u16), String> {
    let bad = || format!("Invalid service port range '{r}'");
    let (a, b) = r.split_once('-').ok_or_else(bad)?;
    let (a, b): (u16, u16) = (a.trim().parse().map_err(|_| bad())?, b.trim().parse().map_err(|_| bad())?);
    if a == 0 || a > b {
        return Err(bad());
    }
    Ok((a, b))
}

fn free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// A random start in the range, then upwards (Paseo `allocatePortFromRange`).
pub fn port_in_range((a, b): (u16, u16), reserved: &HashSet<u16>) -> Result<u16, String> {
    let count = u32::from(b - a) + 1;
    let start = uuid::Uuid::new_v4().as_u128() as u32 % count;
    (0..count)
        .map(|i| a + ((start + i) % count) as u16)
        .find(|p| !reserved.contains(p) && free(*p))
        .ok_or_else(|| format!("No available service port in configured range {a}-{b}"))
}

async fn port_from_script(script: &str, ctx: &Ctx, name: &str, reserved: &HashSet<u16>) -> Result<u16, String> {
    let fail = |d: String| format!("Service port script '{script}' failed: {d}");
    let branch = ctx.branch.clone().unwrap_or_default();
    let cwd = ctx.cwd.display().to_string();
    let out = tokio::time::timeout(
        PORT_SCRIPT_TIMEOUT,
        tokio::process::Command::new(script)
            .args([name, &ctx.workspace_id, &branch, &cwd])
            .current_dir(&ctx.cwd)
            .env("PASEO_SCRIPTNAME", name)
            .env("PASEO_WORKSPACE_ID", &ctx.workspace_id)
            .env("PASEO_BRANCH_NAME", &branch)
            .env("PASEO_WORKTREE_PATH", &cwd)
            .env_remove(crate::notify::TOKEN_ENV)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| fail("timed out".into()))?
    .map_err(|e| fail(e.to_string()))?;
    if !out.status.success() {
        return Err(fail(format!("exit {}", out.status.code().unwrap_or(-1))));
    }
    let text = String::from_utf8_lossy(&out.stdout[..out.stdout.len().min(1024)]).trim().to_string();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("Service port script '{script}' must print exactly one TCP port"));
    }
    let port: u16 = text.parse().ok().filter(|p| *p > 0).ok_or_else(|| format!("Service port script '{script}' returned invalid TCP port '{text}'"))?;
    if reserved.contains(&port) {
        return Err(format!("Service port script '{script}' returned reserved port {port}"));
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_names_follow_paseo() {
        assert_eq!(proxy_label("secretaryios-1a2b3c4d", Some("feat/x"), "web"), "web--feat-x--secretaryios-1a2b3c4d");
        assert_eq!(proxy_label("p", Some("main"), "Web App"), "web-app--p");
        assert_eq!(proxy_label("p", None, "--"), "untitled--p");
        let long = proxy_label(&"x".repeat(80), None, "web");
        assert_eq!(long.len(), 63);
        assert!(long.starts_with("web--xxx") && long.contains("--"));
        assert_eq!(service_env_name("api-server.v2"), "API_SERVER_V2");
        assert_eq!(service_env_name("_web_"), "WEB");
        assert_eq!(hostname_label("Café"), "cafe");
        assert!(parse_range("4100-4099").is_err());
        assert_eq!(parse_range("4100-4999").unwrap(), (4100, 4999));
    }

    #[test]
    fn ports_skip_reserved_and_taken() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = held.local_addr().unwrap().port();
        assert!(port_in_range((p, p), &HashSet::new()).is_err());
        let a = port_in_range((4100, 4999), &HashSet::new()).unwrap();
        assert!((4100..=4999).contains(&a));
        let all: HashSet<u16> = (4100..=4999).filter(|x| *x != 4321).collect();
        if free(4321) {
            assert_eq!(port_in_range((4100, 4999), &all).unwrap(), 4321);
        }
    }

    #[test]
    fn configs_merge_both_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("paseo.json"),
            r#"{"scripts": {"web": {"command": "npm run dev", "type": "service", "port": 5173}, "lint": {"command": " cargo clippy "}, "bad": {"command": 1}, "api": {"command": "x"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(d.path().join(".lucina")).unwrap();
        std::fs::write(d.path().join(LUCINA_CONFIG), "[scripts.api]\ncmd = \"cargo run\"\nport = \"auto\"\nhealth = \"/health\"\n[scripts.docs]\ncommand = \"mdbook serve\"\n").unwrap();
        let c = configs(d.path());
        assert_eq!(c.keys().collect::<Vec<_>>(), ["api", "docs", "lint", "web"]);
        assert_eq!(c["web"].port, Some(5173));
        assert!(c["web"].service);
        assert_eq!(c["lint"].command, "cargo clippy");
        assert!(!c["lint"].service);
        assert_eq!((c["api"].service, c["api"].health.as_deref(), c["api"].source), (true, Some("/health"), LUCINA_CONFIG));
        assert!(!c["docs"].service);
    }
}
