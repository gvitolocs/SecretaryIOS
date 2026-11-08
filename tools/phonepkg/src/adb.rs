//! Driving the SDK `adb` binary: configuration, run-with-timeout, the private adb
//! server whose guard always kills it, the device session and the rearm procedure.

use crate::parse::{self, Gate};
use crate::scan;
use anyhow::{bail, ensure, Context, Result};
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
pub const UNREACHABLE: &str =
    "S20 unreachable. After a reboot: on any Wi-Fi, tap the Wireless debugging tile, then retry.";

pub struct Config {
    pub adb: PathBuf,
    pub aapt2: PathBuf,
    pub target: String,
    pub serial: String,
    pub port: u16,
    pub state: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var("HOME").context("HOME is not set")?;
        let var = |name: &str, default: String| std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or(default);
        let port_text = var("PHONEPKG_ADB_PORT", "5094".into());
        let port: u16 = port_text.parse().with_context(|| format!("PHONEPKG_ADB_PORT={port_text:?} is not a port"))?;
        ensure!(port != 5037, "PHONEPKG_ADB_PORT must not be 5037 (the shared adb server)");
        let target = var("PHONEPKG_TARGET", "100.89.7.91:5555".into());
        parse::target_port(&target)?;
        Ok(Config {
            adb: var("PHONEPKG_ADB", format!("{home}/Android/Sdk/platform-tools/adb")).into(),
            aapt2: var("PHONEPKG_AAPT2", format!("{home}/Android/Sdk/build-tools/36.0.0/aapt2")).into(),
            target,
            serial: var("PHONEPKG_SERIAL", "RF8T31C33FW".into()),
            port,
            state: var("PHONEPKG_STATE", format!("{home}/.local/state/phonepkg")).into(),
        })
    }
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
}

/// SIGHUP, SIGINT and SIGTERM only set a flag; every wait loop checks it and returns an
/// error, which unwinds to main and drops the server guard (so `kill-server` still runs).
pub fn install_signal_handlers() {
    for sig in [1, 2, 15] {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe {
            signal(sig, on_signal);
        }
    }
}

pub fn check_interrupt() -> Result<()> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        bail!("interrupted");
    }
    Ok(())
}

pub fn sleep_checked(d: Duration) -> Result<()> {
    let end = Instant::now() + d;
    loop {
        check_interrupt()?;
        let now = Instant::now();
        if now >= end {
            return Ok(());
        }
        thread::sleep((end - now).min(Duration::from_millis(200)));
    }
}

pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn text(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Spawns `cmd`, polls `try_wait`, kills it after `timeout` (or on a signal when
/// `honor_interrupt`). Pipes are drained on threads so a chatty child never blocks; once
/// the child has exited, output still held open by a grandchild is waited for at most 2 s.
pub fn run(mut cmd: Command, what: &str, timeout: Duration, capture: bool, honor_interrupt: bool) -> Result<Output> {
    cmd.stdin(Stdio::null());
    if capture {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = cmd.spawn().with_context(|| format!("cannot start {what}"))?;
    let out_rx = child.stdout.take().map(drain);
    let err_rx = child.stderr.take().map(drain);
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().with_context(|| format!("waiting for {what}"))? {
            break status;
        }
        if honor_interrupt && check_interrupt().is_err() {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{what}: interrupted");
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{what}: timed out after {} s", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(50));
    };
    Ok(Output { success: status.success(), stdout: collect(out_rx), stderr: collect(err_rx) })
}

fn drain<R: Read + Send + 'static>(mut r: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

fn collect(rx: Option<mpsc::Receiver<Vec<u8>>>) -> String {
    rx.and_then(|rx| rx.recv_timeout(Duration::from_secs(2)).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn describe(args: &[&str]) -> String {
    let mut s = format!("adb {}", args.join(" "));
    if s.len() > 80 {
        let cut = (0..=77).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
        s.truncate(cut);
        s.push_str("...");
    }
    s
}

pub struct Adb {
    pub cfg: Config,
}

impl Adb {
    pub fn new(cfg: Config) -> Self {
        Adb { cfg }
    }

    /// Always phonepkg's own server: `-P <port>`, and none of the variables that would
    /// point adb at another server or device.
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.cfg.adb);
        c.env_remove("ADB_SERVER_SOCKET")
            .env_remove("ANDROID_ADB_SERVER_PORT")
            .env_remove("ANDROID_SERIAL")
            .arg("-P")
            .arg(self.cfg.port.to_string())
            .args(args);
        c
    }

    pub fn run(&self, args: &[&str], timeout: Duration) -> Result<Output> {
        run(self.command(args), &describe(args), timeout, true, true)
    }

    /// `adb -s <serial> <args>`.
    pub fn on(&self, serial: &str, args: &[&str], timeout: Duration) -> Result<Output> {
        let mut full = vec!["-s", serial];
        full.extend_from_slice(args);
        self.run(&full, timeout)
    }

    pub fn shell(&self, serial: &str, cmd: &str, timeout: Duration) -> Result<Output> {
        self.on(serial, &["shell", cmd], timeout)
    }

    /// Starts phonepkg's adb server. The guard exists before the server does, so even a
    /// half-failed start is followed by `kill-server`.
    pub fn start_server(&self) -> Result<Server<'_>> {
        let guard = Server { adb: self };
        let out = run(self.command(&["start-server"]), "adb start-server", DEFAULT_TIMEOUT, false, true)?;
        ensure!(out.success, "adb start-server failed (port {}, adb {})", self.cfg.port, self.cfg.adb.display());
        Ok(guard)
    }

    /// `adb connect <addr>` and wait until `get-state` says `device`.
    pub fn attach(&self, addr: &str) -> Result<()> {
        let out = self.run(&["connect", addr], DEFAULT_TIMEOUT)?;
        let text = out.text();
        ensure!(parse::connect_succeeded(&text), "adb connect {addr}: {}", text.trim());
        self.wait_device(addr)
    }

    /// Right after a connect the transport may still be `offline` / authorizing.
    fn wait_device(&self, addr: &str) -> Result<()> {
        let mut last = String::new();
        for attempt in 0..5 {
            if attempt > 0 {
                sleep_checked(Duration::from_secs(1))?;
            }
            let out = self.on(addr, &["get-state"], DEFAULT_TIMEOUT)?;
            if out.success && out.stdout.trim() == "device" {
                return Ok(());
            }
            last = out.text().trim().to_string();
        }
        bail!("{addr} is not a ready device (get-state: {last})")
    }

    pub fn serial_of(&self, addr: &str) -> Result<String> {
        let out = self.shell(addr, "getprop ro.serialno", DEFAULT_TIMEOUT)?;
        let serial = out.stdout.trim().to_string();
        ensure!(out.success && !serial.is_empty(), "cannot read ro.serialno on {addr}: {}", out.text().trim());
        Ok(serial)
    }

    /// The rearm procedure: find the wireless-debugging TLS port (mDNS, else a TCP scan of
    /// the target host), connect over it, switch adbd back to legacy TCP on the target port.
    /// Needs a running server. Returns the address it went through.
    pub fn rearm(&self, full: bool) -> Result<String> {
        let target = self.cfg.target.as_str();
        let host = parse::target_host(target);
        let target_port = parse::target_port(target)?;

        eprintln!("rearm: asking mDNS for _adb-tls-connect._tcp");
        let mut candidates = match self.run(&["mdns", "services"], DEFAULT_TIMEOUT) {
            Ok(out) => parse::parse_mdns_tls(&out.stdout),
            Err(e) => {
                check_interrupt()?;
                eprintln!("rearm: adb mdns services failed: {e:#}");
                Vec::new()
            }
        };
        if candidates.is_empty() {
            let ip: IpAddr = (host, 0)
                .to_socket_addrs()
                .with_context(|| format!("cannot resolve {host}"))?
                .next()
                .with_context(|| format!("{host} has no address"))?
                .ip();
            let range = if full { 1024..=65535 } else { 30000..=50000 };
            eprintln!("rearm: mDNS found nothing; scanning {ip} ports {}-{}", range.start(), range.end());
            let open = scan::scan(ip, range, &[target_port], 256, Duration::from_millis(400))?;
            candidates = open.iter().map(|p| format!("{host}:{p}")).collect();
        }
        if candidates.is_empty() {
            bail!("rearm: no wireless-debugging port found (is Wireless debugging on?)");
        }
        eprintln!("rearm: candidates {}", candidates.join(", "));

        for cand in &candidates {
            check_interrupt()?;
            let result = self.rearm_via(cand, target, target_port);
            let _ = self.run(&["disconnect", cand], DEFAULT_TIMEOUT);
            match result {
                Ok(()) => {
                    eprintln!("rearm: {cand} switched adbd to TCP {target_port}; {target} is a device again");
                    return Ok(cand.clone());
                }
                Err(e) => {
                    check_interrupt()?;
                    eprintln!("rearm: {cand}: {e:#}");
                }
            }
        }
        bail!("rearm: none of {} candidate(s) worked", candidates.len())
    }

    fn rearm_via(&self, cand: &str, target: &str, target_port: u16) -> Result<()> {
        self.attach(cand)?;
        let serial = self.serial_of(cand)?;
        ensure!(serial == self.cfg.serial, "serial {serial} is not the S20 ({})", self.cfg.serial);
        let port = target_port.to_string();
        let out = self.on(cand, &["tcpip", &port], DEFAULT_TIMEOUT)?;
        ensure!(out.success, "adb tcpip {port}: {}", out.text().trim());
        sleep_checked(Duration::from_secs(3))?;
        self.attach(target)
    }
}

/// Phonepkg's adb server; dropping it kills the server.
pub struct Server<'a> {
    adb: &'a Adb,
}

impl Drop for Server<'_> {
    fn drop(&mut self) {
        let _ = run(self.adb.command(&["kill-server"]), "adb kill-server", Duration::from_secs(10), true, false);
    }
}

/// A connected, serial-checked S20. Holds the server guard.
pub struct Session<'a> {
    pub adb: &'a Adb,
    pub target: String,
    /// The wireless-debugging address used when the automatic rearm ran.
    pub rearmed_via: Option<String>,
    _server: Server<'a>,
}

impl<'a> Session<'a> {
    pub fn open(adb: &'a Adb) -> Result<Self> {
        let server = adb.start_server()?;
        let target = adb.cfg.target.clone();
        let mut rearmed_via = None;
        if let Err(e) = adb.attach(&target) {
            check_interrupt()?;
            eprintln!("{target}: {e:#}; trying rearm");
            match adb.rearm(false) {
                Ok(via) => rearmed_via = Some(via),
                Err(re) => {
                    check_interrupt()?;
                    bail!("{UNREACHABLE}\n({re:#})");
                }
            }
            if let Err(e) = adb.attach(&target) {
                check_interrupt()?;
                bail!("{UNREACHABLE}\n({e:#})");
            }
        }
        let serial = adb.serial_of(&target)?;
        ensure!(
            serial == adb.cfg.serial,
            "{target} has serial {serial}, expected {}; refusing to touch it",
            adb.cfg.serial
        );
        Ok(Session { adb, target, rearmed_via, _server: server })
    }

    pub fn shell(&self, cmd: &str) -> Result<Output> {
        self.adb.shell(&self.target, cmd, DEFAULT_TIMEOUT)
    }

    /// Shell output that must have exited 0.
    pub fn shell_ok(&self, cmd: &str) -> Result<String> {
        let out = self.shell(cmd)?;
        ensure!(out.success, "adb shell {cmd}: {}", out.text().trim());
        Ok(out.stdout)
    }

    pub fn getprop(&self, name: &str) -> Result<String> {
        Ok(self.shell_ok(&format!("getprop {name}"))?.trim().to_string())
    }

    /// `pm list packages <flags> <pkg>` and an exact match.
    pub fn package_entry(&self, flags: &str, package: &str) -> Result<Option<parse::PmPackage>> {
        ensure!(parse::is_valid_package_name(package), "not a package name: {package:?}");
        let text = self.shell_ok(&format!("pm list packages {flags} {package}"))?;
        Ok(parse::find_exact(&parse::parse_pm_list(&text), package).cloned())
    }

    pub fn installed(&self, package: &str) -> Result<Option<parse::PmPackage>> {
        self.package_entry("--show-versioncode", package)
    }

    /// versionName of each package, one shell call.
    pub fn version_names(&self, packages: &[&str]) -> Result<std::collections::BTreeMap<String, String>> {
        if packages.is_empty() {
            return Ok(Default::default());
        }
        let script = parse::version_name_script(packages)?;
        let out = self.adb.shell(&self.target, &script, INSTALL_TIMEOUT)?;
        Ok(parse::parse_version_names(&out.stdout))
    }

    /// The three dumpsys reads; a failed read becomes empty text, which the gate treats as busy.
    pub fn wolt_gate(&self) -> Gate {
        let read = |cmd: &str| match self.shell(cmd) {
            Ok(out) if out.success => out.stdout,
            _ => String::new(),
        };
        let activities = read("dumpsys activity activities");
        let services = read(&format!("dumpsys activity services {}", parse::WOLT));
        let location = read("dumpsys location");
        parse::wolt_gate(&activities, &services, &location)
    }

    /// Checks the gate now and, with `wait_minutes`, every 30 s until idle or the deadline.
    pub fn wait_for_wolt_idle(&self, wait_minutes: u64) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(wait_minutes.saturating_mul(60));
        loop {
            let gate = self.wolt_gate();
            if !gate.busy {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                bail!("Wolt may be in use, refusing: {}", gate.reasons.join("; "));
            }
            eprintln!("Wolt busy ({}); re-checking in 30 s", gate.reasons.join("; "));
            sleep_checked((deadline - now).min(Duration::from_secs(30)))?;
        }
    }
}
