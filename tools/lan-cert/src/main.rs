//! lan-cert: keeps the home-network host of Lucina's live view working.
//!
//! The owner's devices at home load the live view from nezopt directly
//! (`https://lan.getsecretary.tech:9443`, served by lucina-rs, see
//! `crates/lucina-server/src/lan.rs`). That needs two things this tool keeps true:
//!
//! 1. `lan.getsecretary.tech` resolves to nezopt's current LAN address: an A record in
//!    Cloudflare (DNS only, never proxied), updated when DHCP hands nezopt another address.
//! 2. A publicly trusted certificate for it, although the host is not reachable from the
//!    internet: Let's Encrypt with the DNS-01 challenge (a TXT record in the same zone).
//!    It reuses an existing ACME account (`acme-account.url` + `acme-account.key`, PKCS#8
//!    PEM) instead of registering one. The certificate is renewed after 60 days.
//!
//! lucina-rs re-reads `fullchain.pem`/`key.pem` when they change, so nothing restarts.
//! Run it from a timer (`deploy/lucina-rs/lucina-lan-cert.timer`); it is a no-op when both
//! are already right. Secrets (the Cloudflare token, keys) are never printed.
//!
//! usage: lan-cert [--domain lan.getsecretary.tech] [--dir ~/.config/lucina-rs/lan]
//!                 [--cf-env ~/.config/pokoin/cloudflare.env] [--force] [--dns-only] [--staging]

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt, NewOrder, OrderStatus, RetryPolicy,
};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::PrivatePkcs8KeyDer;
use serde_json::{json, Value};

/// Renew once the certificate is this old (Let's Encrypt issues 90-day certificates).
const RENEW_AFTER: Duration = Duration::from_secs(60 * 24 * 3600);
const CF: &str = "https://api.cloudflare.com/client/v4";

struct Opts {
    domain: String,
    dir: PathBuf,
    cf_env: PathBuf,
    force: bool,
    dns_only: bool,
    staging: bool,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into()))
}

fn opts() -> Result<Opts> {
    let mut o = Opts {
        domain: "lan.getsecretary.tech".into(),
        dir: home().join(".config/lucina-rs/lan"),
        cf_env: home().join(".config/pokoin/cloudflare.env"),
        force: false,
        dns_only: false,
        staging: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| anyhow!("{a} needs a value"));
        match a.as_str() {
            "--domain" => o.domain = value()?,
            "--dir" => o.dir = PathBuf::from(value()?),
            "--cf-env" => o.cf_env = PathBuf::from(value()?),
            "--force" => o.force = true,
            "--dns-only" => o.dns_only = true,
            "--staging" => o.staging = true,
            "-h" | "--help" => {
                println!("usage: lan-cert [--domain D] [--dir DIR] [--cf-env FILE] [--force] [--dns-only] [--staging]");
                std::process::exit(0);
            }
            _ => bail!("unknown argument {a}"),
        }
    }
    if !o.domain.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') || o.domain.split('.').count() < 3 {
        bail!("--domain must be a host name under a zone, like lan.example.com");
    }
    Ok(o)
}

/// `KEY=value` lines; `export` and quotes tolerated.
fn read_env(path: &Path) -> Result<std::collections::HashMap<String, String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches("export ");
            let (k, v) = l.split_once('=')?;
            (!k.starts_with('#')).then(|| (k.trim().to_owned(), v.trim().trim_matches(|c| c == '"' || c == '\'').to_owned()))
        })
        .collect())
}

/// The address this host uses towards the internet: on nezopt, its LAN address.
fn lan_ip() -> Result<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0")?;
    s.connect("1.1.1.1:53")?;
    match s.local_addr()?.ip() {
        IpAddr::V4(ip) if ip.is_private() => Ok(ip),
        other => bail!("the default route's source address {other} is not a private LAN address"),
    }
}

/// How the API is called: the scoped token first; the account key from the same file when
/// the token may not edit this zone's DNS (the token in cloudflare.env is scoped to other zones).
enum Auth {
    Token(String),
    Key { email: String, key: String },
}

struct Cloudflare {
    http: reqwest::Client,
    auth: Auth,
    fallback: Option<Auth>,
    zone: String,
}

impl Cloudflare {
    async fn new(env: &Path, domain: &str) -> Result<Self> {
        let vars = read_env(env)?;
        let token = vars.get("CLOUDFLARE_API_TOKEN").cloned();
        let email = vars.get("CLOUDFLARE_API_EMAIL").or_else(|| vars.get("CLOUDFLARE_EMAIL")).cloned();
        let key = vars.get("CLOUDFLARE_API_KEY").cloned();
        let account = email.zip(key).map(|(email, key)| Auth::Key { email, key });
        let (auth, fallback) = match token {
            Some(t) => (Auth::Token(t), account),
            None => (account.ok_or_else(|| anyhow!("no Cloudflare credentials in {}", env.display()))?, None),
        };
        let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
        let zone_name = domain.split_once('.').map_or(domain, |(_, zone)| zone).to_owned();
        let mut cf = Self { http, auth, fallback, zone: String::new() };
        let zones = cf.call(reqwest::Method::GET, &format!("/zones?name={zone_name}"), None).await?;
        cf.zone = zones["result"][0]["id"].as_str().ok_or_else(|| anyhow!("no Cloudflare zone {zone_name}"))?.to_owned();
        // Can these credentials read the zone's records? If not, use the account key.
        let probe = format!("/zones/{}/dns_records?per_page=1", cf.zone);
        if cf.call(reqwest::Method::GET, &probe, None).await.is_err() {
            if let Some(f) = cf.fallback.take() {
                cf.auth = f;
                cf.call(reqwest::Method::GET, &probe, None).await.context("Cloudflare DNS records with the account key")?;
                println!("lan-cert: the API token may not edit {zone_name}; using the account key from the same file");
            }
        }
        Ok(cf)
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self.http.request(method, format!("{CF}{path}"));
        req = match &self.auth {
            Auth::Token(t) => req.bearer_auth(t),
            Auth::Key { email, key } => req.header("X-Auth-Email", email).header("X-Auth-Key", key),
        };
        if let Some(b) = body {
            req = req.json(&b);
        }
        let v: Value = req.send().await?.json().await?;
        if v["success"] != json!(true) {
            bail!("Cloudflare {path}: {}", v["errors"]);
        }
        Ok(v)
    }

    async fn records(&self, kind: &str, name: &str) -> Result<Vec<Value>> {
        let v = self.call(reqwest::Method::GET, &format!("/zones/{}/dns_records?type={kind}&name={name}", self.zone), None).await?;
        Ok(v["result"].as_array().cloned().unwrap_or_default())
    }

    /// The A record, DNS only. Returns whether it changed.
    async fn ensure_a(&self, name: &str, ip: Ipv4Addr) -> Result<bool> {
        let want = json!({"type": "A", "name": name, "content": ip.to_string(), "ttl": 300, "proxied": false,
                          "comment": "nezopt LAN address: Lucina's live view at home (lan-cert)"});
        let found = self.records("A", name).await?;
        match found.first() {
            Some(r) if r["content"] == json!(ip.to_string()) && r["proxied"] == json!(false) => Ok(false),
            Some(r) => {
                let id = r["id"].as_str().unwrap_or_default();
                self.call(reqwest::Method::PUT, &format!("/zones/{}/dns_records/{id}", self.zone), Some(want)).await?;
                Ok(true)
            }
            None => {
                self.call(reqwest::Method::POST, &format!("/zones/{}/dns_records", self.zone), Some(want)).await?;
                Ok(true)
            }
        }
    }

    async fn add_txt(&self, name: &str, value: &str) -> Result<String> {
        let v = self
            .call(reqwest::Method::POST, &format!("/zones/{}/dns_records", self.zone), Some(json!({"type": "TXT", "name": name, "content": value, "ttl": 60})))
            .await?;
        Ok(v["result"]["id"].as_str().unwrap_or_default().to_owned())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.call(reqwest::Method::DELETE, &format!("/zones/{}/dns_records/{id}", self.zone), None).await?;
        Ok(())
    }
}

/// Waits until a public resolver (Cloudflare's DoH) returns the TXT value.
async fn wait_for_txt(http: &reqwest::Client, name: &str, value: &str) -> Result<()> {
    for _ in 0..40 {
        let r: Value = http
            .get("https://cloudflare-dns.com/dns-query")
            .query(&[("name", name), ("type", "TXT")])
            .header("accept", "application/dns-json")
            .send()
            .await?
            .json()
            .await?;
        let seen = r["Answer"].as_array().is_some_and(|a| a.iter().any(|x| x["data"].as_str().is_some_and(|d| d.trim_matches('"') == value)));
        if seen {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    bail!("{name} TXT did not appear in public DNS within two minutes")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Whether the certificate in `dir` is missing or old enough to renew.
fn due(dir: &Path) -> bool {
    let issued = std::fs::read_to_string(dir.join("issued-at")).ok().and_then(|s| s.trim().parse::<u64>().ok());
    !dir.join("fullchain.pem").exists() || !dir.join("key.pem").exists() || issued.is_none_or(|t| now().saturating_sub(t) > RENEW_AFTER.as_secs())
}

/// Write-then-rename, so lucina-rs never reads half a file.
fn write_atomic(path: &Path, data: &str, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

async fn issue(o: &Opts, cf: &Cloudflare) -> Result<()> {
    let url = std::fs::read_to_string(o.dir.join("acme-account.url")).context("acme-account.url (the ACME account to reuse)")?;
    let key = PrivatePkcs8KeyDer::from_pem_file(o.dir.join("acme-account.key")).map_err(|e| anyhow!("acme-account.key: {e}"))?;
    let directory = if o.staging { LetsEncrypt::Staging.url() } else { LetsEncrypt::Production.url() };
    let account = Account::builder()?.from_parts(url.trim().to_owned(), key, directory.to_owned()).await?;

    let ids = [Identifier::Dns(o.domain.clone())];
    let mut order = account.new_order(&NewOrder::new(&ids)).await?;
    let record = format!("_acme-challenge.{}", o.domain);
    let mut txt_ids = Vec::new();
    let result = async {
        let mut authorizations = order.authorizations();
        while let Some(authz) = authorizations.next().await {
            let mut authz = authz?;
            match authz.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                other => bail!("authorization is {other:?}"),
            }
            let mut challenge = authz.challenge(ChallengeType::Dns01).ok_or_else(|| anyhow!("no dns-01 challenge offered"))?;
            let value = challenge.key_authorization().dns_value();
            txt_ids.push(cf.add_txt(&record, &value).await?);
            wait_for_txt(&cf.http, &record, &value).await?;
            challenge.set_ready().await?;
        }
        let status = order.poll_ready(&RetryPolicy::default()).await?;
        if status != OrderStatus::Ready {
            bail!("order is {status:?}, not ready");
        }
        let key_pem = order.finalize().await?;
        let chain_pem = order.poll_certificate(&RetryPolicy::default()).await?;
        Ok((key_pem, chain_pem))
    }
    .await;
    for id in txt_ids {
        if let Err(e) = cf.delete(&id).await {
            eprintln!("lan-cert: could not remove the challenge TXT record: {e}");
        }
    }
    let (key_pem, chain_pem) = result?;
    // The key first: lucina-rs reloads when fullchain.pem changes.
    write_atomic(&o.dir.join("key.pem"), &key_pem, 0o600)?;
    write_atomic(&o.dir.join("fullchain.pem"), &chain_pem, 0o644)?;
    write_atomic(&o.dir.join("issued-at"), &format!("{}\n", now()), 0o644)?;
    println!("lan-cert: issued a certificate for {}{}", o.domain, if o.staging { " (staging)" } else { "" });
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let o = opts()?;
    std::fs::create_dir_all(&o.dir)?;
    let ip = lan_ip()?;
    let cf = Cloudflare::new(&o.cf_env, &o.domain).await?;
    if cf.ensure_a(&o.domain, ip).await? {
        println!("lan-cert: {} -> {ip} (DNS only)", o.domain);
    } else {
        println!("lan-cert: {} already -> {ip}", o.domain);
    }
    if o.dns_only {
        return Ok(());
    }
    if o.force || due(&o.dir) {
        issue(&o, &cf).await?;
    } else {
        println!("lan-cert: certificate is fresh");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewal_is_due_without_files_or_after_sixty_days() {
        let dir = std::env::temp_dir().join(format!("lan-cert-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(due(&dir), "nothing issued yet");
        std::fs::write(dir.join("fullchain.pem"), "x").unwrap();
        std::fs::write(dir.join("key.pem"), "x").unwrap();
        std::fs::write(dir.join("issued-at"), format!("{}", now() - 3600)).unwrap();
        assert!(!due(&dir), "issued an hour ago");
        std::fs::write(dir.join("issued-at"), format!("{}", now() - 61 * 24 * 3600)).unwrap();
        assert!(due(&dir), "61 days old");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_files_are_parsed_without_quotes_or_export() {
        let p = std::env::temp_dir().join(format!("lan-cert-env-{}", std::process::id()));
        std::fs::write(&p, "# c\nexport A=\"1\"\nB='two'\nC=3\n").unwrap();
        let v = read_env(&p).unwrap();
        assert_eq!((v["A"].as_str(), v["B"].as_str(), v["C"].as_str()), ("1", "two", "3"));
        std::fs::remove_file(&p).unwrap();
    }
}
