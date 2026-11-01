//! `phonepkg`: over-the-air app manager for the owner's Samsung S20 FE (SM-G780G).
//!
//! Drives the SDK `adb` binary against the phone over Tailscale, on its own adb server
//! port, and kills that server when it is done. Installs and uninstalls are refused while
//! the Wolt courier app may be in use (the Wolt gate) and every attempt is logged to
//! `$PHONEPKG_STATE/log.jsonl`. See README.md.

mod adb;
mod parse;
mod scan;

use adb::{Adb, Config, Session};
use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

const PROTECTED: &[&str] = &[
    "com.wolt.courierapp",
    "tech.getsecretary.secretary",
    "tech.getsecretary.lucinaotp",
    "com.lucina.maps",
    "com.android.vending",
    "com.google.android.gms",
];

#[derive(Parser)]
#[command(name = "phonepkg", version, about = "Over-the-air app manager for the S20 FE (adb over Tailscale)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List installed packages (third-party by default).
    List {
        /// Include system packages.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Install or update an APK, keeping its data. Downgrades are always refused.
    Install {
        apk: PathBuf,
        /// Minutes to wait for Wolt to go idle (re-checked every 30 s).
        #[arg(long, default_value_t = 0)]
        wait: u64,
        #[arg(long)]
        json: bool,
    },
    /// Uninstall a package. System packages are always refused.
    Uninstall {
        package: String,
        /// Keep the app's data and cache.
        #[arg(short = 'k', long)]
        keep_data: bool,
        /// Allow uninstalling a protected package (Wolt, Secretary, Lucina, Play).
        #[arg(long)]
        force: bool,
        /// Minutes to wait for Wolt to go idle (re-checked every 30 s).
        #[arg(long, default_value_t = 0)]
        wait: u64,
        #[arg(long)]
        json: bool,
    },
    /// Show connection, device facts and the Wolt gate verdict.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Re-enable adbd on TCP 5555 through the wireless-debugging port (after a reboot).
    Rearm {
        /// Scan ports 1024-65535 instead of 30000-50000.
        #[arg(long)]
        full: bool,
    },
    /// Pair with Wireless debugging (Developer options -> Wireless debugging -> Pair device with pairing code).
    Pair {
        /// Pairing address shown on the phone, ip:port.
        addr: String,
        /// The 6-digit pairing code.
        code: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    adb::install_signal_handlers();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("phonepkg: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let adb = Adb::new(Config::from_env()?);
    match cli.cmd {
        Cmd::List { all, json } => list(&adb, all, json),
        Cmd::Install { apk, wait, json } => {
            let mut rec = LogEntry::new("install");
            let result = install(&adb, &apk, wait, json, &mut rec);
            log_attempt(&adb.cfg.state, rec, &result);
            result
        }
        Cmd::Uninstall { package, keep_data, force, wait, json } => {
            let mut rec = LogEntry::new("uninstall");
            rec.package = Some(package.clone());
            let result = uninstall(&adb, &package, keep_data, force, wait, json, &mut rec);
            log_attempt(&adb.cfg.state, rec, &result);
            result
        }
        Cmd::Status { json } => status(&adb, json),
        Cmd::Rearm { full } => {
            let _server = adb.start_server()?;
            let via = adb.rearm(full).map_err(|e| anyhow::anyhow!("{e:#}\n{}", adb::UNREACHABLE))?;
            println!("rearmed {} via {via}", adb.cfg.target);
            Ok(())
        }
        Cmd::Pair { addr, code } => pair(&adb, &addr, &code),
    }
}

#[derive(Serialize)]
struct Row {
    package: String,
    version_name: Option<String>,
    version_code: Option<u64>,
    installer: Option<String>,
}

fn list(adb: &Adb, all: bool, json: bool) -> Result<()> {
    let s = Session::open(adb)?;
    let flags = if all { "" } else { " -3" };
    let text = s.shell_ok(&format!("pm list packages{flags} --show-versioncode -i"))?;
    let mut pkgs = parse::parse_pm_list(&text);
    pkgs.sort_by(|a, b| a.package.cmp(&b.package));
    let names: Vec<&str> = pkgs.iter().map(|p| p.package.as_str()).collect();
    let mut version_names = s.version_names(&names)?;
    let rows: Vec<Row> = pkgs
        .into_iter()
        .map(|p| Row {
            version_name: version_names.remove(&p.package),
            package: p.package,
            version_code: p.version_code,
            installer: p.installer,
        })
        .collect();
    drop(s);

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let table: Vec<[String; 4]> = rows
        .iter()
        .map(|r| {
            [
                r.package.clone(),
                r.version_name.clone().unwrap_or_else(|| "-".into()),
                r.version_code.map_or_else(|| "-".into(), |c| c.to_string()),
                r.installer.clone().unwrap_or_else(|| "null".into()),
            ]
        })
        .collect();
    let header = ["package", "versionName", "versionCode", "installer"].map(String::from);
    let mut widths = header.clone().map(|h| h.chars().count());
    for row in &table {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&table) {
        let line = row
            .iter()
            .zip(widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect::<Vec<_>>()
            .join("  ");
        println!("{}", line.trim_end());
    }
    Ok(())
}

fn version_label(name: Option<&str>, code: Option<u64>) -> String {
    match (name.filter(|n| !n.is_empty()), code) {
        (Some(n), Some(c)) => format!("{n} ({c})"),
        (Some(n), None) => n.to_string(),
        (None, Some(c)) => format!("({c})"),
        (None, None) => "?".to_string(),
    }
}

fn badging(adb: &Adb, apk: &Path) -> Result<parse::Badging> {
    ensure!(apk.is_file(), "{} is not a file", apk.display());
    let mut cmd = Command::new(&adb.cfg.aapt2);
    cmd.args(["dump", "badging"]).arg(apk);
    let out = adb::run(cmd, "aapt2 dump badging", adb::DEFAULT_TIMEOUT, true, true)?;
    ensure!(out.success, "aapt2 dump badging {}: {}", apk.display(), out.text().trim());
    parse::parse_badging(&out.stdout)
}

#[derive(Serialize)]
struct InstallReport {
    package: String,
    from_version: Option<String>,
    version_name: String,
    version_code: u64,
}

fn install(adb: &Adb, apk: &Path, wait: u64, json: bool, rec: &mut LogEntry) -> Result<()> {
    let apk_info = badging(adb, apk)?;
    rec.package = Some(apk_info.package.clone());
    rec.to_version = Some(version_label(Some(&apk_info.version_name), Some(apk_info.version_code)));
    ensure!(parse::is_valid_package_name(&apk_info.package), "APK package {:?} is not a package name", apk_info.package);
    let apk_str = apk.to_str().context("APK path is not UTF-8")?;

    let s = Session::open(adb)?;
    let before = s.installed(&apk_info.package)?;
    let old_name = match &before {
        Some(_) => s.version_names(&[apk_info.package.as_str()])?.remove(&apk_info.package),
        None => None,
    };
    if let Some(b) = &before {
        rec.from_version = Some(version_label(old_name.as_deref(), b.version_code));
        ensure!(b.version_code.is_some(), "cannot read the installed versionCode of {}", apk_info.package);
    }
    parse::check_version(before.as_ref().and_then(|b| b.version_code), apk_info.version_code)?;
    s.wait_for_wolt_idle(wait)?;

    let out = s.adb.on(&s.target, &["install", "-r", apk_str], adb::INSTALL_TIMEOUT)?;
    let text = out.text();
    ensure!(text.contains("Success") && !text.contains("Failure"), "adb install failed: {}", text.trim());

    let after = s.installed(&apk_info.package)?.and_then(|p| p.version_code);
    ensure!(
        after == Some(apk_info.version_code),
        "after install {} reports versionCode {:?}, expected {}",
        apk_info.package,
        after,
        apk_info.version_code
    );
    drop(s);

    let from = before.map(|b| old_name.unwrap_or_else(|| version_label(None, b.version_code)));
    if json {
        let report = InstallReport {
            package: apk_info.package,
            from_version: from,
            version_name: apk_info.version_name,
            version_code: apk_info.version_code,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "installed {} {} -> {} ({})",
            apk_info.package,
            from.as_deref().unwrap_or("new"),
            apk_info.version_name,
            apk_info.version_code
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct UninstallReport {
    package: String,
    from_version: Option<String>,
    kept_data: bool,
}

fn uninstall(
    adb: &Adb,
    package: &str,
    keep_data: bool,
    force: bool,
    wait: u64,
    json: bool,
    rec: &mut LogEntry,
) -> Result<()> {
    ensure!(parse::is_valid_package_name(package), "not a package name: {package:?}");
    let s = Session::open(adb)?;
    let Some(entry) = s.package_entry("--show-versioncode", package)? else {
        bail!("{package} is not installed");
    };
    let name = s.version_names(&[package])?.remove(package);
    rec.from_version = Some(version_label(name.as_deref(), entry.version_code));
    if s.package_entry("-s", package)?.is_some() {
        bail!("{package} is a system package; refusing (--force does not override this)");
    }
    if PROTECTED.contains(&package) && !force {
        bail!("{package} is protected; pass --force to uninstall it anyway");
    }
    s.wait_for_wolt_idle(wait)?;

    let out = if keep_data {
        s.adb.shell(&s.target, &format!("cmd package uninstall -k {package}"), adb::INSTALL_TIMEOUT)?
    } else {
        s.adb.on(&s.target, &["uninstall", package], adb::INSTALL_TIMEOUT)?
    };
    let text = out.text();
    ensure!(text.contains("Success") && !text.contains("Failure"), "uninstall failed: {}", text.trim());

    ensure!(s.package_entry("", package)?.is_none(), "{package} is still installed after uninstall");
    if keep_data {
        ensure!(
            s.package_entry("-u", package)?.is_some(),
            "{package} vanished from `pm list packages -u`; its data was not kept"
        );
    }
    drop(s);

    if json {
        let report =
            UninstallReport { package: package.to_string(), from_version: rec.from_version.clone(), kept_data: keep_data };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let from = rec.from_version.as_deref().unwrap_or("?");
        let kept = if keep_data { " (data kept)" } else { "" };
        println!("uninstalled {package} {from}{kept}");
    }
    Ok(())
}

#[derive(Serialize, Default)]
struct Status {
    reachable: bool,
    target: String,
    rearmed_via: Option<String>,
    serial: Option<String>,
    model: Option<String>,
    android_release: Option<String>,
    uptime: Option<String>,
    adb_tcp_port: Option<String>,
    adb_wifi_enabled: Option<String>,
    wolt_busy: Option<bool>,
    wolt_reasons: Vec<String>,
    error: Option<String>,
}

fn status(adb: &Adb, json: bool) -> Result<()> {
    let mut st = Status { target: adb.cfg.target.clone(), ..Default::default() };
    let result = (|| -> Result<()> {
        let s = Session::open(adb)?;
        st.reachable = true;
        st.rearmed_via = s.rearmed_via.clone();
        st.serial = Some(s.getprop("ro.serialno")?);
        st.model = Some(s.getprop("ro.product.model")?);
        st.android_release = Some(s.getprop("ro.build.version.release")?);
        st.uptime = Some(s.shell_ok("uptime")?.trim().to_string());
        st.adb_tcp_port = Some(s.getprop("service.adb.tcp.port")?);
        st.adb_wifi_enabled = Some(s.shell_ok("settings get global adb_wifi_enabled")?.trim().to_string());
        let gate = s.wolt_gate();
        st.wolt_busy = Some(gate.busy);
        st.wolt_reasons = gate.reasons;
        Ok(())
    })();
    if let Err(e) = &result {
        st.error = Some(format!("{e:#}"));
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&st)?);
    } else {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
        println!("reachable         {}", if st.reachable { "yes" } else { "no" });
        println!("target            {}", st.target);
        if let Some(via) = &st.rearmed_via {
            println!("rearmed via       {via}");
        }
        println!("serial            {}", show(&st.serial));
        println!("model             {}", show(&st.model));
        println!("android           {}", show(&st.android_release));
        println!("uptime            {}", show(&st.uptime));
        println!("adb tcp port      {}", show(&st.adb_tcp_port));
        println!("adb_wifi_enabled  {}", show(&st.adb_wifi_enabled));
        match st.wolt_busy {
            Some(false) => println!("wolt gate         idle"),
            Some(true) => println!("wolt gate         BUSY: {}", st.wolt_reasons.join("; ")),
            None => println!("wolt gate         -"),
        }
    }
    result
}

fn pair(adb: &Adb, addr: &str, code: &str) -> Result<()> {
    ensure!(code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()), "the pairing code is 6 digits");
    ensure!(addr.parse::<std::net::SocketAddr>().is_ok(), "pairing address must be ip:port, got {addr:?}");
    let _server = adb.start_server()?;
    let out = adb.run(&["pair", addr, code], adb::DEFAULT_TIMEOUT)?;
    let text = out.text();
    println!("{}", text.trim());
    ensure!(out.success && text.contains("Successfully paired"), "pairing with {addr} failed");
    Ok(())
}

#[derive(Serialize)]
struct LogEntry {
    ts: String,
    action: &'static str,
    package: Option<String>,
    from_version: Option<String>,
    to_version: Option<String>,
    ok: bool,
    error: Option<String>,
}

impl LogEntry {
    fn new(action: &'static str) -> Self {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        LogEntry {
            ts: parse::format_rfc3339(secs),
            action,
            package: None,
            from_version: None,
            to_version: None,
            ok: false,
            error: None,
        }
    }
}

/// One JSON line per attempt; a log failure is reported but never changes the outcome.
fn log_attempt(state: &Path, mut rec: LogEntry, result: &Result<()>) {
    rec.ok = result.is_ok();
    rec.error = result.as_ref().err().map(|e| format!("{e:#}"));
    let write = || -> Result<()> {
        std::fs::create_dir_all(state).with_context(|| format!("creating {}", state.display()))?;
        let path = state.join("log.jsonl");
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{}", serde_json::to_string(&rec)?)?;
        Ok(())
    };
    if let Err(e) = write() {
        eprintln!("phonepkg: warning: could not write the log: {e:#}");
    }
}
