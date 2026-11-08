//! `lucina-link`: probe routes, print the best one, or connect through it (SSH ProxyCommand, curl).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::process::{Command, ExitCode};
use std::time::Instant;

use lucina_link::config::{expand_home, Config, Route, Service, Target};
use lucina_link::policy::{Media, Traffic};
use lucina_link::probe::{probe_service, Outcome, ServiceReport};
use lucina_link::secrets::{self, AccessToken};

const HELP: &str = "\
lucina-link: pick the best route to a Lucina/Secretary service (LAN, tailnet, public, Cloudflare, SSH jump)

USAGE:
  lucina-link probe [SERVICE...] [TRAFFIC] [--json]   probe every allowed route, mark the chosen one
  lucina-link route SERVICE [TRAFFIC] [--json]        print only the chosen route (exit 2 if none)
  lucina-link proxy SERVICE                           SSH ProxyCommand: connect stdin/stdout through the best route
  lucina-link ssh SERVICE [-- SSH ARGS...]            ssh to the service through `proxy`
  lucina-link curl SERVICE PATH [-- CURL ARGS...]     curl an HTTP service through its best API route
  lucina-link services                                list the configured services

TRAFFIC:
  --traffic api|shell|bulk   (probe/route default: shell for *-ssh services, api otherwise)
  --bytes N                  body or transfer size
  --media image|audio|video|other

Routes: $LUCINA_LINK_CONFIG, ~/.config/lucina-link/routes.toml, or the built-in table.
Access token: LUCINA_LINK_CF_CLIENT_ID/SECRET, or ~/.config/lucina-link/secrets.env (mode 600).
  --via ROUTE[,ROUTE]        consider only these route ids (also $LUCINA_LINK_VIA, which reaches `proxy`)

LUCINA_LINK_VERBOSE=1 prints the route choice of proxy/ssh/curl on stderr.
";

struct Opts {
    positional: Vec<String>,
    traffic: Option<String>,
    bytes: u64,
    media: Media,
    json: bool,
    via: Option<String>,
    rest: Vec<String>,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        positional: Vec::new(),
        traffic: None,
        bytes: 0,
        media: Media::None,
        json: false,
        via: None,
        rest: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                o.rest = it.cloned().collect();
                break;
            }
            "--json" => o.json = true,
            "--via" => o.via = Some(it.next().ok_or("--via needs route ids")?.clone()),
            "--traffic" => o.traffic = Some(it.next().ok_or("--traffic needs a value")?.clone()),
            "--bytes" => {
                o.bytes = it
                    .next()
                    .ok_or("--bytes needs a value")?
                    .parse()
                    .map_err(|_| "--bytes must be a number")?
            }
            "--media" => {
                o.media = match it.next().map(String::as_str) {
                    Some("image") => Media::Image,
                    Some("audio") => Media::Audio,
                    Some("video") => Media::Video,
                    Some("other") => Media::Other,
                    Some("none") => Media::None,
                    _ => return Err("--media must be image, audio, video, other or none".into()),
                }
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => o.positional.push(a.clone()),
        }
    }
    Ok(o)
}

fn traffic_for(o: &Opts, service: &str) -> Result<Traffic, String> {
    let class = o.traffic.clone().unwrap_or_else(|| {
        if o.media != Media::None {
            "bulk".into()
        } else if service.ends_with("-ssh") {
            "shell".into()
        } else {
            "api".into()
        }
    });
    Ok(match class.as_str() {
        "api" => Traffic::Api {
            body_bytes: o.bytes,
        },
        "shell" => Traffic::Shell,
        "bulk" => Traffic::Bulk {
            bytes: o.bytes,
            media: o.media,
        },
        other => return Err(format!("unknown traffic {other}")),
    })
}

fn verbose() -> bool {
    std::env::var("LUCINA_LINK_VERBOSE").is_ok_and(|v| v == "1")
}

fn load_token() -> Option<AccessToken> {
    match secrets::load() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lucina-link: {e}");
            None
        }
    }
}

fn find_service<'a>(cfg: &'a Config, name: &str) -> Result<&'a Service, String> {
    cfg.service(name).ok_or_else(|| {
        let names: Vec<&str> = cfg.services.keys().map(String::as_str).collect();
        format!("unknown service {name} (known: {})", names.join(", "))
    })
}

fn print_report(rep: &ServiceReport) {
    println!("{}  ({:?})", rep.service, rep.traffic);
    for r in &rep.routes {
        let state = match &r.outcome {
            Outcome::Up { rtt_ms, note } => match note {
                Some(n) => format!("up {rtt_ms} ms ({n})"),
                None => format!("up {rtt_ms} ms"),
            },
            Outcome::Down { reason } => format!("down: {reason}"),
            Outcome::Unknown { reason } => format!("unknown: {reason}"),
            Outcome::Skipped { reason } => format!("skipped: {reason}"),
        };
        let mark = if rep.chosen.as_deref() == Some(r.id.as_str()) {
            "  → chosen"
        } else {
            ""
        };
        println!("  {:<11} {:<10} {state}{mark}", r.id, r.kind.as_str());
    }
    if rep.chosen.is_none() {
        println!("  (no usable route)");
    }
}

fn choose<'a>(
    svc: &'a Service,
    traffic: Traffic,
    token: Option<&AccessToken>,
) -> Result<(&'a Route, ServiceReport), String> {
    let rep = probe_service(svc, traffic, token, &|_| None);
    let route = rep
        .chosen
        .as_deref()
        .and_then(|id| svc.routes.iter().find(|r| r.id == id));
    match route {
        Some(r) => {
            if verbose() {
                eprintln!(
                    "lucina-link: {} via {} ({})",
                    svc.name,
                    r.id,
                    r.kind.as_str()
                );
            }
            Ok((r, rep))
        }
        None => {
            let why: Vec<String> = rep
                .routes
                .iter()
                .map(|r| {
                    format!(
                        "{}: {}",
                        r.id,
                        serde_json::to_string(&r.outcome).unwrap_or_default()
                    )
                })
                .collect();
            Err(format!(
                "no usable route to {} [{}]",
                svc.name,
                why.join("; ")
            ))
        }
    }
}

fn route_json(r: &Route) -> serde_json::Value {
    let target = match &r.target {
        Target::Tcp { host, port } => serde_json::json!({"tcp": format!("{host}:{port}")}),
        Target::Http { base, .. } => serde_json::json!({"url": base}),
        Target::CloudflareSsh { hostname } => serde_json::json!({"ssh_hostname": hostname}),
        Target::SshJump { jump, host, port } => {
            serde_json::json!({"jump": jump, "tcp": format!("{host}:{port}")})
        }
    };
    serde_json::json!({"id": r.id, "kind": r.kind, "target": target})
}

fn find_cloudflared() -> String {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join("cloudflared");
            if p.is_file() {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    for p in [
        "/opt/homebrew/bin/cloudflared",
        "/usr/local/bin/cloudflared",
        "/usr/bin/cloudflared",
    ] {
        if std::path::Path::new(p).is_file() {
            return p.into();
        }
    }
    "cloudflared".into()
}

#[cfg(unix)]
fn exec(mut cmd: Command) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let err = cmd.exec();
    Err(format!("exec {:?}: {err}", cmd.get_program()))
}

#[cfg(not(unix))]
fn exec(mut cmd: Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| e.to_string())?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Pipes stdin → socket and socket → stdout until both directions end.
fn pipe_tcp(host: &str, port: u16, route: &Route) -> Result<(), String> {
    let deadline = Instant::now() + route.timeout;
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    let mut sock = None;
    for a in &addrs {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Ok(s) = TcpStream::connect_timeout(a, left.max(std::time::Duration::from_millis(50)))
        {
            sock = Some(s);
            break;
        }
    }
    let sock = sock.ok_or_else(|| format!("connect {host}:{port} failed"))?;
    let _ = sock.set_nodelay(true);
    let mut up = sock.try_clone().map_err(|e| e.to_string())?;
    let writer = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 16384];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if up.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = up.shutdown(Shutdown::Write);
    });
    let mut down = sock;
    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; 16384];
    loop {
        match down.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stdout
                    .write_all(&buf[..n])
                    .and_then(|_| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
        }
    }
    // The remote closed: ssh will exit; do not wait for stdin.
    drop(writer);
    Ok(())
}

fn cmd_proxy(cfg: &Config, name: &str) -> Result<(), String> {
    let svc = find_service(cfg, name)?;
    let token = load_token();
    let (route, _) = choose(svc, Traffic::Shell, token.as_ref())?;
    match &route.target {
        Target::Tcp { host, port } => pipe_tcp(host, *port, route),
        Target::CloudflareSsh { hostname } => {
            let token = token.ok_or("no service token")?;
            let mut cmd = Command::new(find_cloudflared());
            cmd.args(["access", "ssh", "--hostname", hostname])
                .env("TUNNEL_SERVICE_TOKEN_ID", token.client_id.expose())
                .env("TUNNEL_SERVICE_TOKEN_SECRET", token.client_secret.expose());
            exec(cmd)
        }
        Target::SshJump { jump, host, port } => {
            let mut cmd = Command::new("ssh");
            cmd.args(["-W", &format!("{host}:{port}"), jump]);
            exec(cmd)
        }
        Target::Http { .. } => Err(format!("{} is an HTTP route, not a shell", route.id)),
    }
}

fn cmd_ssh(cfg: &Config, name: &str, rest: &[String], via: Option<&str>) -> Result<(), String> {
    let svc = find_service(cfg, name)?;
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new("ssh");
    if let Some(v) = via {
        cmd.env("LUCINA_LINK_VIA", v);
    }
    cmd.arg("-o")
        .arg(format!("ProxyCommand={} proxy {name}", me.display()))
        .arg("-o")
        .arg(format!("HostKeyAlias={name}"));
    if let Some(id) = &svc.identity {
        cmd.arg("-i")
            .arg(expand_home(id))
            .args(["-o", "IdentitiesOnly=yes"]);
    }
    let dest = match &svc.user {
        Some(u) => format!("{u}@{name}"),
        None => name.to_string(),
    };
    cmd.arg(dest).args(rest);
    exec(cmd)
}

fn cmd_curl(cfg: &Config, name: &str, path: &str, o: &Opts) -> Result<(), String> {
    let svc = find_service(cfg, name)?;
    let token = load_token();
    let traffic = match o.traffic.as_deref() {
        None => Traffic::Api {
            body_bytes: o.bytes,
        },
        Some(_) => traffic_for(o, name)?,
    };
    let (route, _) = choose(svc, traffic, token.as_ref())?;
    let Target::Http { base, .. } = &route.target else {
        return Err(format!("{} is not an HTTP route", route.id));
    };
    let mut cmd = Command::new("curl");
    cmd.arg("-sS");
    // Access headers go through a 0600 curl config on stdin, never argv.
    if route.kind == lucina_link::RouteKind::Cloudflare {
        let t = token.ok_or("no service token")?;
        cmd.args(["-K", "-"]).stdin(std::process::Stdio::piped());
        let mut child = cmd
            .arg(format!("{base}{path}"))
            .args(&o.rest)
            .spawn()
            .map_err(|e| e.to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(
                stdin,
                "header = \"CF-Access-Client-Id: {}\"\nheader = \"CF-Access-Client-Secret: {}\"",
                t.client_id.expose(),
                t.client_secret.expose()
            );
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        std::process::exit(status.code().unwrap_or(1));
    }
    cmd.arg(format!("{base}{path}")).args(&o.rest);
    exec(cmd)
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let Some(cmd) = args.first() else {
        print!("{HELP}");
        return Ok(ExitCode::from(1));
    };
    if matches!(cmd.as_str(), "-h" | "--help" | "help") {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }
    let o = parse_opts(&args[1..])?;
    let mut cfg = Config::load().map_err(|e| e.to_string())?;
    let via = o.via.clone().or_else(|| {
        std::env::var("LUCINA_LINK_VIA")
            .ok()
            .filter(|v| !v.is_empty())
    });
    if let Some(v) = &via {
        let ids: Vec<&str> = v.split(',').map(str::trim).collect();
        for s in cfg.services.values_mut() {
            s.routes.retain(|r| ids.contains(&r.id.as_str()));
        }
    }
    match cmd.as_str() {
        "services" => {
            for (name, s) in &cfg.services {
                let ids: Vec<&str> = s.routes.iter().map(|r| r.id.as_str()).collect();
                println!(
                    "{name:<13} {}  [{}]",
                    s.description.as_deref().unwrap_or(""),
                    ids.join(", ")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        "probe" => {
            let names: Vec<String> = if o.positional.is_empty() {
                cfg.services.keys().cloned().collect()
            } else {
                o.positional.clone()
            };
            let token = load_token();
            let svcs: Vec<&Service> = names
                .iter()
                .map(|n| find_service(&cfg, n))
                .collect::<Result<_, _>>()?;
            let traffics: Vec<Traffic> = names
                .iter()
                .map(|n| traffic_for(&o, n))
                .collect::<Result<_, _>>()?;
            // Services in parallel too: the whole table takes about as long as its slowest route.
            let reports: Vec<ServiceReport> = std::thread::scope(|s| {
                let handles: Vec<_> = svcs
                    .iter()
                    .zip(&traffics)
                    .map(|(svc, t)| {
                        let token = token.as_ref();
                        s.spawn(move || probe_service(svc, *t, token, &|_| None))
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("probe thread"))
                    .collect()
            });
            if o.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reports).map_err(|e| e.to_string())?
                );
            } else {
                for r in &reports {
                    print_report(r);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "route" => {
            let name = o.positional.first().ok_or("route needs a SERVICE")?;
            let svc = find_service(&cfg, name)?;
            let token = load_token();
            match choose(svc, traffic_for(&o, name)?, token.as_ref()) {
                Ok((r, rep)) => {
                    if o.json {
                        let mut v = route_json(r);
                        v["service"] = serde_json::json!(name);
                        v["report"] = serde_json::to_value(&rep).map_err(|e| e.to_string())?;
                        println!("{v}");
                    } else {
                        println!("{} {} {}", r.id, r.kind.as_str(), route_json(r)["target"]);
                    }
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!("lucina-link: {e}");
                    Ok(ExitCode::from(2))
                }
            }
        }
        "proxy" => {
            cmd_proxy(&cfg, o.positional.first().ok_or("proxy needs a SERVICE")?)?;
            Ok(ExitCode::SUCCESS)
        }
        "ssh" => {
            cmd_ssh(
                &cfg,
                o.positional.first().ok_or("ssh needs a SERVICE")?,
                &o.rest,
                via.as_deref(),
            )?;
            Ok(ExitCode::SUCCESS)
        }
        "curl" => {
            let name = o.positional.first().ok_or("curl needs a SERVICE")?;
            let path = o.positional.get(1).map(String::as_str).unwrap_or("/");
            cmd_curl(&cfg, name, path, &o)?;
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other} (see --help)")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("lucina-link: {e}");
            ExitCode::from(1)
        }
    }
}
