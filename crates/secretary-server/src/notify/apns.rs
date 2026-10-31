//! APNs adapter for the native iOS app.
//!
//! Remote push to the native app needs a paid Apple Developer Program team
//! (the free Personal Team cannot use the `aps-environment` entitlement)
//! and an APNs auth key (`AuthKey_<KEYID>.p8`). Without a key the adapter
//! is off: `POST /v1/push/apns` still saves the device token (answering
//! `apnsEnabled: false`) and nothing is sent to Apple.
//!
//! | variable | |
//! |---|---|
//! | `SECRETARY_APNS_TEAM_ID` | the 10-character team id |
//! | `SECRETARY_APNS_KEY_FILE` | the `.p8` key (PKCS#8 PEM), mode 0600 (fixed-key mode) |
//! | `SECRETARY_APNS_KEY_ID` | its 10-character key id (fixed-key mode) |
//! | `SECRETARY_APNS_TOPIC` | bundle id, default `tech.getsecretary.app` |
//! | `SECRETARY_APNS_ENV` | `sandbox` (Xcode builds, default) or `production` (TestFlight/App Store) |
//!
//! Two modes:
//!
//! - **Installed key**: only `SECRETARY_APNS_TEAM_ID` (with `TOPIC`/`ENV`).
//!   The owner adds the key with Add secret, name `APNS_AUTH_KEY_<KEYID>`;
//!   the server writes it to `$SECRETARY_DATA_DIR/apns/AuthKey_<KEYID>.p8`
//!   (0600, dir 0700) and push is on at once, no restart. A key file put
//!   there by hand is picked up within 30 s ([`ApnsSlot`]).
//! - **Fixed key**: `KEY_FILE`, `KEY_ID` and `TEAM_ID` together; the key is
//!   loaded at start (a bad key fails start) and cannot be replaced at run time.
//!
//! Requests go over HTTP/2 to `api.push.apple.com` (or the sandbox host)
//! with a provider JWT (ES256, `kid` = key id, `iss` = team id, `iat`),
//! refreshed every 50 minutes as Apple requires (20–60 min).

use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use serde_json::json;

use super::push::{apns_body, Payload, PushOpts};

pub const DEFAULT_TOPIC: &str = "tech.getsecretary.app";
pub const PRODUCTION_URL: &str = "https://api.push.apple.com";
pub const SANDBOX_URL: &str = "https://api.sandbox.push.apple.com";
/// Sub-directory of the data dir holding the installed key.
pub const KEY_DIR: &str = "apns";
const TOKEN_REFRESH: Duration = Duration::from_secs(50 * 60);
/// How often a slot without a key looks for an installed one.
const RESCAN: Duration = Duration::from_secs(30);
const BAD_KEY: &str = "not an APNs key (.p8, P-256 PKCS#8)";

/// 10 ASCII letters/digits (Apple key and team ids).
pub fn valid_id(s: &str) -> bool {
    s.len() == 10 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApnsConfig {
    /// Both `None`: use the key installed in the data dir.
    pub key_file: Option<PathBuf>,
    pub key_id: Option<String>,
    pub team_id: String,
    pub topic: String,
    pub sandbox: bool,
    /// Override of the APNs origin (tests).
    pub base_url: Option<String>,
}

impl ApnsConfig {
    /// `None` when no key/team variable is set; installed-key mode with
    /// `SECRETARY_APNS_TEAM_ID` alone; fixed-key mode with all three; an
    /// error for any other mix.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let key_file = get("SECRETARY_APNS_KEY_FILE");
        let key_id = get("SECRETARY_APNS_KEY_ID");
        let team_id = get("SECRETARY_APNS_TEAM_ID");
        let (key_file, key_id, team_id) = match (key_file, key_id, team_id) {
            (None, None, None) => return Ok(None),
            (None, None, Some(t)) => (None, None, t),
            (Some(f), Some(k), Some(t)) => (Some(f), Some(k), t),
            _ => bail!(
                "APNs needs SECRETARY_APNS_TEAM_ID alone (key installed with Add secret) \
                 or SECRETARY_APNS_KEY_FILE, SECRETARY_APNS_KEY_ID and SECRETARY_APNS_TEAM_ID together"
            ),
        };
        if !key_id.as_deref().is_none_or(valid_id) || !valid_id(&team_id) {
            bail!("SECRETARY_APNS_KEY_ID and SECRETARY_APNS_TEAM_ID are 10 letters/digits");
        }
        let sandbox = match get("SECRETARY_APNS_ENV").as_deref() {
            None | Some("sandbox") | Some("development") => true,
            Some("production") => false,
            Some(v) => bail!("SECRETARY_APNS_ENV={v}: expected sandbox or production"),
        };
        Ok(Some(Self {
            key_file: key_file.map(PathBuf::from),
            key_id,
            team_id,
            topic: get("SECRETARY_APNS_TOPIC").unwrap_or_else(|| DEFAULT_TOPIC.into()),
            sandbox,
            base_url: None,
        }))
    }

    /// Installed-key mode: no key file in the environment.
    pub fn installed_key(&self) -> bool {
        self.key_file.is_none()
    }

    pub fn origin(&self) -> &str {
        self.base_url.as_deref().unwrap_or(if self.sandbox {
            SANDBOX_URL
        } else {
            PRODUCTION_URL
        })
    }
}

pub struct Apns {
    pub cfg: ApnsConfig,
    /// The `kid` of the key in use (from the config or the installed file).
    pub key_id: String,
    key: SigningKey,
    http: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

/// An APNs device token: hex, 64–200 characters.
pub fn valid_device_token(t: &str) -> bool {
    (64..=200).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The signing key in a `.p8` as Apple gives it, also pasted with CRLF,
/// with the newlines turned into spaces, or as the bare base64 body. The
/// error never includes the input.
pub fn parse_p8(text: &str) -> anyhow::Result<SigningKey> {
    const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
    const END: &str = "-----END PRIVATE KEY-----";
    let body = match text.find(BEGIN) {
        Some(i) => {
            let rest = &text[i + BEGIN.len()..];
            rest.find(END).map_or(rest, |j| &rest[..j])
        }
        None => text,
    };
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let der = STANDARD.decode(b64).map_err(|_| anyhow::anyhow!(BAD_KEY))?;
    SigningKey::from_pkcs8_der(&der).map_err(|_| anyhow::anyhow!(BAD_KEY))
}

impl Apns {
    /// Fixed-key mode: the key file and id from the config.
    pub fn new(cfg: ApnsConfig) -> anyhow::Result<Self> {
        let (Some(file), Some(key_id)) = (cfg.key_file.clone(), cfg.key_id.clone()) else {
            bail!("SECRETARY_APNS_KEY_FILE and SECRETARY_APNS_KEY_ID are not set");
        };
        let pem = std::fs::read_to_string(&file)
            .with_context(|| format!("SECRETARY_APNS_KEY_FILE {}", file.display()))?;
        let key = parse_p8(&pem).context("SECRETARY_APNS_KEY_FILE")?;
        Self::with_key(cfg, key_id, key)
    }

    pub fn with_key(cfg: ApnsConfig, key_id: String, key: SigningKey) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            cfg,
            key_id,
            key,
            http,
            token: Mutex::new(None),
        })
    }

    /// The provider JWT for `iat` (seconds).
    pub fn jwt_at(&self, iat: u64) -> String {
        let header = json!({"alg": "ES256", "kid": self.key_id});
        let claims = json!({"iss": self.cfg.team_id, "iat": iat});
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig: Signature = self.key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }

    fn jwt(&self) -> String {
        let mut slot = self.token.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, at)) = slot.as_ref() {
            if at.elapsed() < TOKEN_REFRESH {
                return t.clone();
            }
        }
        let iat = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let t = self.jwt_at(iat);
        *slot = Some((t.clone(), Instant::now()));
        t
    }

    /// One POST to APNs: `(status, Retry-After, reason)`; a dead token's
    /// 400 reason is reported as 410 so the caller drops it.
    pub async fn send(
        &self,
        device_token: &str,
        payload: &Payload,
        opts: &PushOpts,
    ) -> Result<(u16, Option<Duration>, String), String> {
        if !valid_device_token(device_token) {
            return Ok((410, None, "BadDeviceToken".into()));
        }
        let url = format!("{}/3/device/{device_token}", self.cfg.origin());
        let expiration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + 6 * 3600;
        let mut req = self
            .http
            .post(url)
            .header("authorization", format!("bearer {}", self.jwt()))
            .header("apns-topic", &self.cfg.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", if opts.urgent { "10" } else { "5" })
            .header("apns-expiration", expiration.to_string())
            .json(&apns_body(payload));
        if let Some(t) = &opts.topic {
            req = req.header("apns-collapse-id", t);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                "timeout".to_owned()
            } else {
                "network error".to_owned()
            }
        })?;
        let status = resp.status().as_u16();
        let retry = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()?.trim().parse().ok())
            .map(Duration::from_secs);
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["reason"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let dead = matches!(
            reason.as_str(),
            "BadDeviceToken" | "Unregistered" | "DeviceTokenNotForTopic"
        );
        Ok((if dead { 410 } else { status }, retry, reason))
    }
}

/// Why [`ApnsSlot::install`] refused a key. The texts never carry the key.
#[derive(Debug)]
pub enum InstallError {
    /// No installed-key config (`SECRETARY_APNS_TEAM_ID` alone).
    NoTeam,
    /// The server runs with `SECRETARY_APNS_KEY_FILE`.
    FixedKey,
    BadKeyId,
    BadKey,
    Io(io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTeam => f.write_str("APNs installed keys need SECRETARY_APNS_TEAM_ID"),
            Self::FixedKey => f.write_str("APNs uses SECRETARY_APNS_KEY_FILE on this server"),
            Self::BadKeyId => f.write_str("APNs key id: 10 letters/digits"),
            Self::BadKey => f.write_str(BAD_KEY),
            Self::Io(e) => write!(f, "APNs key file: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

#[derive(Default)]
struct Scan {
    at: Option<Instant>,
    /// mtime of the last file that failed to parse (warned once).
    bad: Option<SystemTime>,
}

/// The APNs client, if any: fixed at start, or loaded from the key
/// installed in `<data dir>/apns/` (by [`ApnsSlot::install`] or by hand).
pub struct ApnsSlot {
    cfg: Option<ApnsConfig>,
    dir: PathBuf,
    apns: RwLock<Option<Arc<Apns>>>,
    scan: Mutex<Scan>,
}

impl ApnsSlot {
    /// `data_dir` is `$SECRETARY_DATA_DIR`; a fixed key is loaded now (a bad
    /// one is an error), an installed one on the first [`Self::get`].
    pub fn open(data_dir: &Path, cfg: Option<ApnsConfig>) -> anyhow::Result<Self> {
        let apns = match &cfg {
            Some(c) if !c.installed_key() => Some(Arc::new(Apns::new(c.clone())?)),
            _ => None,
        };
        Ok(Self {
            cfg,
            dir: data_dir.join(KEY_DIR),
            apns: RwLock::new(apns),
            scan: Mutex::default(),
        })
    }

    fn installed_cfg(&self) -> Option<&ApnsConfig> {
        self.cfg.as_ref().filter(|c| c.installed_key())
    }

    /// The client when APNs is on right now.
    pub fn get(&self) -> Option<Arc<Apns>> {
        if let Some(a) = self.apns.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
            return Some(a.clone());
        }
        let cfg = self.installed_cfg()?;
        let mut scan = self.scan.lock().unwrap_or_else(|p| p.into_inner());
        if scan.at.is_some_and(|at| at.elapsed() < RESCAN) {
            return None;
        }
        scan.at = Some(Instant::now());
        // Installed meanwhile by another caller.
        if let Some(a) = self.apns.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
            return Some(a.clone());
        }
        let (path, key_id, mtime) = self.newest_key()?;
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|pem| parse_p8(&pem).ok())
            .and_then(|key| Apns::with_key(cfg.clone(), key_id.clone(), key).ok());
        match loaded {
            Some(apns) => {
                let apns = Arc::new(apns);
                *self.apns.write().unwrap_or_else(|p| p.into_inner()) = Some(apns.clone());
                tracing::info!(key_id = %key_id, "APNs enabled with the installed key");
                Some(apns)
            }
            None => {
                if scan.bad != Some(mtime) {
                    scan.bad = Some(mtime);
                    tracing::warn!(path = %path.display(), "installed APNs key is not usable; APNs stays off");
                }
                None
            }
        }
    }

    /// The newest `AuthKey_<KEYID>.p8` in the key dir: (path, key id, mtime).
    fn newest_key(&self) -> Option<(PathBuf, String, SystemTime)> {
        std::fs::read_dir(&self.dir)
            .ok()?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let id = name.strip_prefix("AuthKey_")?.strip_suffix(".p8")?;
                if !valid_id(id) {
                    return None;
                }
                let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
                Some((e.path(), id.to_ascii_uppercase(), meta.modified().ok()?))
            })
            .max_by_key(|(_, _, mtime)| *mtime)
    }

    /// Install `pem` as the key `key_id` and turn APNs on with it: written
    /// atomically (0600) as `apns/AuthKey_<KEYID>.p8`, other keys removed.
    pub fn install(&self, key_id: &str, pem: &str) -> Result<(), InstallError> {
        let cfg = match &self.cfg {
            None => return Err(InstallError::NoTeam),
            Some(c) if !c.installed_key() => return Err(InstallError::FixedKey),
            Some(c) => c.clone(),
        };
        if !valid_id(key_id) {
            return Err(InstallError::BadKeyId);
        }
        let key_id = key_id.to_ascii_uppercase();
        let key = parse_p8(pem).map_err(|_| InstallError::BadKey)?;
        let canonical = key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|_| InstallError::BadKey)?;
        let apns = Apns::with_key(cfg, key_id.clone(), key)
            .map_err(|e| InstallError::Io(io::Error::other(e.to_string())))?;
        self.write_key(&key_id, canonical.as_bytes())
            .map_err(InstallError::Io)?;
        *self.apns.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(apns));
        *self.scan.lock().unwrap_or_else(|p| p.into_inner()) = Scan::default();
        Ok(())
    }

    fn write_key(&self, key_id: &str, pem: &[u8]) -> io::Result<()> {
        crate::devices::ensure_dir(&self.dir)?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        let name = format!("AuthKey_{key_id}.p8");
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(pem)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, self.dir.join(&name))?;
        for e in std::fs::read_dir(&self.dir)?.flatten() {
            let other = e.file_name();
            let other = other.to_string_lossy();
            if other != name && other.starts_with("AuthKey_") && other.ends_with(".p8") {
                std::fs::remove_file(e.path())?;
            }
        }
        std::fs::File::open(&self.dir)?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    fn test_key() -> SigningKey {
        SigningKey::from_slice(&[9u8; 32]).unwrap()
    }

    fn pem() -> String {
        test_key().to_pkcs8_pem(LineEnding::LF).unwrap().to_string()
    }

    fn key_file(dir: &Path) -> (PathBuf, VerifyingKey) {
        let path = dir.join("AuthKey_TEST123456.p8");
        std::fs::write(&path, pem()).unwrap();
        (path, *test_key().verifying_key())
    }

    fn lookup(vars: &[(&str, &str)]) -> anyhow::Result<Option<ApnsConfig>> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        ApnsConfig::from_lookup(&|k: &str| {
            vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
        })
    }

    const FILE: (&str, &str) = ("SECRETARY_APNS_KEY_FILE", "/x.p8");
    const KID: (&str, &str) = ("SECRETARY_APNS_KEY_ID", "ABCDE12345");
    const TEAM: (&str, &str) = ("SECRETARY_APNS_TEAM_ID", "TEAM123456");

    #[test]
    fn config_modes() {
        assert!(lookup(&[]).unwrap().is_none(), "disabled by default");

        let team = lookup(&[TEAM]).unwrap().unwrap();
        assert!(team.installed_key());
        assert_eq!((&team.key_file, &team.key_id), (&None, &None));
        assert_eq!(team.origin(), SANDBOX_URL);

        let full = lookup(&[FILE, KID, TEAM, ("SECRETARY_APNS_ENV", "production")])
            .unwrap()
            .unwrap();
        assert!(!full.installed_key());
        assert_eq!(full.key_file, Some(PathBuf::from("/x.p8")));
        assert_eq!(full.key_id.as_deref(), Some("ABCDE12345"));
        assert_eq!(full.topic, DEFAULT_TOPIC);
        assert_eq!(full.origin(), PRODUCTION_URL);

        for partial in [
            &[KID][..],
            &[FILE],
            &[FILE, KID],
            &[FILE, TEAM],
            &[KID, TEAM],
        ] {
            let e = lookup(partial).unwrap_err().to_string();
            assert!(
                e.contains("SECRETARY_APNS_TEAM_ID alone"),
                "{partial:?}: {e}"
            );
        }
        assert!(lookup(&[("SECRETARY_APNS_TEAM_ID", "short")]).is_err());
        assert!(lookup(&[FILE, ("SECRETARY_APNS_KEY_ID", "bad-id-123"), TEAM]).is_err());
        assert!(lookup(&[TEAM, ("SECRETARY_APNS_ENV", "staging")]).is_err());
    }

    #[test]
    fn p8_is_parsed_leniently() {
        let want = test_key().to_bytes();
        let pem = pem();
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        for text in [
            pem.clone(),
            pem.replace('\n', "\r\n"),
            pem.replace('\n', " "),
            format!("  {}  ", pem.trim().replace('\n', " ")),
            body.clone(),
            pem.lines()
                .filter(|l| !l.starts_with("-----"))
                .collect::<Vec<_>>()
                .join("\n"),
        ] {
            assert_eq!(parse_p8(&text).unwrap().to_bytes(), want, "{text:?}");
        }
        let mut rsa_ish = vec![0x30, 0x82, 0x01, 0x22];
        rsa_ish.extend((0..290u32).map(|i| (i * 37 % 251) as u8));
        for bad in [
            "garbage-not-a-key".to_owned(),
            STANDARD.encode(&rsa_ish),
            String::new(),
            format!(
                "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----",
                STANDARD.encode(&rsa_ish)
            ),
        ] {
            let e = parse_p8(&bad).unwrap_err().to_string();
            assert_eq!(e, BAD_KEY);
        }
    }

    fn installed_cfg() -> Option<ApnsConfig> {
        lookup(&[TEAM]).unwrap()
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn slot_installs_and_loads_keys() {
        let dir = tempfile::tempdir().unwrap();
        let slot = ApnsSlot::open(dir.path(), installed_cfg()).unwrap();
        assert!(slot.get().is_none(), "no key yet");

        let keys = dir.path().join(KEY_DIR);
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::write(keys.join("AuthKey_OLDKEY0001.p8"), "old").unwrap();
        assert!(matches!(
            slot.install("ab12cd34", &pem()),
            Err(InstallError::BadKeyId)
        ));
        assert!(matches!(
            slot.install("AB12CD34EF", "nope"),
            Err(InstallError::BadKey)
        ));
        slot.install("ab12cd34ef", &pem().replace('\n', "\r\n"))
            .unwrap();

        let apns = slot.get().expect("on at once");
        assert_eq!(apns.key_id, "AB12CD34EF");
        let file = keys.join("AuthKey_AB12CD34EF.p8");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            pem(),
            "canonical PEM"
        );
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&keys), 0o700);
        let names: Vec<_> = std::fs::read_dir(&keys)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["AuthKey_AB12CD34EF.p8"], "older keys removed");

        // A new slot (a restart) finds the installed key on its first get().
        let again = ApnsSlot::open(dir.path(), installed_cfg()).unwrap();
        assert_eq!(again.get().unwrap().key_id, "AB12CD34EF");
    }

    #[test]
    fn slot_needs_an_installed_key_config() {
        let dir = tempfile::tempdir().unwrap();
        let off = ApnsSlot::open(dir.path(), None).unwrap();
        assert!(matches!(
            off.install("AB12CD34EF", &pem()),
            Err(InstallError::NoTeam)
        ));
        assert_eq!(
            InstallError::NoTeam.to_string(),
            "APNs installed keys need SECRETARY_APNS_TEAM_ID"
        );
        assert!(off.get().is_none());

        let (path, _) = key_file(dir.path());
        let mut fixed = lookup(&[FILE, KID, TEAM]).unwrap().unwrap();
        fixed.key_file = Some(path);
        let slot = ApnsSlot::open(dir.path(), Some(fixed)).unwrap();
        assert_eq!(slot.get().unwrap().key_id, "ABCDE12345");
        assert!(matches!(
            slot.install("AB12CD34EF", &pem()),
            Err(InstallError::FixedKey)
        ));
    }

    #[test]
    fn a_bad_installed_file_keeps_apns_off() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join(KEY_DIR);
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::write(keys.join("AuthKey_AB12CD34EF.p8"), "not a key").unwrap();
        let slot = ApnsSlot::open(dir.path(), installed_cfg()).unwrap();
        assert!(slot.get().is_none());
        assert!(slot.get().is_none(), "no rescan within 30 s");
    }

    #[test]
    fn provider_jwt_is_es256_with_kid_iss_iat() {
        let dir = tempfile::tempdir().unwrap();
        let (path, vk) = key_file(dir.path());
        let apns = Apns::new(ApnsConfig {
            key_file: Some(path),
            key_id: Some("TEST123456".into()),
            team_id: "TEAM123456".into(),
            topic: DEFAULT_TOPIC.into(),
            sandbox: true,
            base_url: None,
        })
        .unwrap();
        let jwt = apns.jwt_at(1_761_048_000);
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(header, json!({"alg": "ES256", "kid": "TEST123456"}));
        assert_eq!(claims, json!({"iss": "TEAM123456", "iat": 1_761_048_000}));
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
        assert_eq!(apns.jwt(), apns.jwt(), "cached between calls");
        assert!(valid_device_token(&"ab".repeat(32)));
        assert!(!valid_device_token("xyz"));
    }
}
