//! Lucina's own Google account (`mylucina20@gmail.com`): OAuth and
//! read-only Drive.
//!
//! - Client: Google's "web"/"installed" client JSON (`LUCINA_GOOGLE_CLIENT_FILE`).
//! - Grant: authorization code + PKCE S256, `access_type=offline`,
//!   `prompt=consent`, scopes `drive.readonly openid email`. The refresh token
//!   lives in the token file (`LUCINA_DRIVE_TOKEN_FILE`, 0600, written
//!   atomically); access tokens stay in memory until 60 s before they expire.
//! - The account is checked at grant time: a grant for any other account is
//!   refused and nothing is written.
//! - No secret (client secret, access or refresh token) ever appears in an
//!   error, a `Debug` print or command output.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const SCOPE_DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
/// Full Drive (read and write): Lucina's own account, so she may manage her files.
pub const SCOPE_DRIVE: &str = "https://www.googleapis.com/auth/drive";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
pub const DEFAULT_ACCOUNT: &str = "mylucina20@gmail.com";
pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8788/oauth2callback";
/// The OAuth client Lucina's Gmail already uses (project `lucinaporygon`).
pub const DEFAULT_CLIENT_FILE: &str =
    "/home/nez/Projects/Lucina/private/lucina/secrets/gmail-oauth-client.json";

/// `<home>/.config/lucina-rs/<name>`.
pub fn config_file(home: &str, name: &str) -> PathBuf {
    Path::new(home).join(".config/lucina-rs").join(name)
}

/// A pending consent (state + PKCE verifier) is good for this long.
pub const PENDING_MAX_AGE_SECS: i64 = 15 * 60;
/// Largest file `read`/`download` fetches (bytes).
pub const MAX_DOWNLOAD_BYTES: usize = 64 * 1024 * 1024;

const FILE_FIELDS: &str =
    "id,name,mimeType,modifiedTime,size,parents,webViewLink,md5Checksum,sha256Checksum";

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct GoogleError(pub String);

fn err(msg: impl Into<String>) -> GoogleError {
    GoogleError(msg.into())
}

/// Google endpoints (overridable for tests).
#[derive(Debug, Clone)]
pub struct GoogleEndpoints {
    pub auth: String,
    pub token: String,
    pub userinfo: String,
    pub drive: String,
    /// Media uploads (`/upload/drive/v3`).
    pub upload: String,
}

impl Default for GoogleEndpoints {
    fn default() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            userinfo: "https://openidconnect.googleapis.com/v1/userinfo".into(),
            drive: "https://www.googleapis.com/drive/v3".into(),
            upload: "https://www.googleapis.com/upload/drive/v3".into(),
        }
    }
}

/// The OAuth client.
#[derive(Clone)]
pub struct GoogleClient {
    pub client_id: String,
    client_secret: String,
}

impl std::fmt::Debug for GoogleClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleClient")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

impl GoogleClient {
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
        }
    }

    /// Parse Google's client JSON (`{"web": {...}}` or `{"installed": {...}}`).
    pub fn from_json(bytes: &[u8]) -> Result<Self, GoogleError> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|_| err("OAuth client file is not JSON"))?;
        let c = v.get("web").or_else(|| v.get("installed")).unwrap_or(&v);
        let id = c["client_id"].as_str().unwrap_or("").trim();
        let secret = c["client_secret"].as_str().unwrap_or("").trim();
        if id.is_empty() || secret.is_empty() {
            return Err(err("OAuth client file has no client_id/client_secret"));
        }
        Ok(Self::new(id, secret))
    }

    pub fn from_file(path: &Path) -> Result<Self, GoogleError> {
        let bytes = std::fs::read(path).map_err(|e| {
            err(format!(
                "OAuth client file {}: {}",
                path.display(),
                e.kind()
            ))
        })?;
        Self::from_json(&bytes)
    }
}

/// The stored grant.
#[derive(Clone, Serialize, Deserialize)]
pub struct DriveToken {
    refresh_token: String,
    pub scope: String,
    pub account_email: String,
    pub created_at: String,
}

impl std::fmt::Debug for DriveToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveToken")
            .field("refresh_token", &"<redacted>")
            .field("scope", &self.scope)
            .field("account_email", &self.account_email)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl DriveToken {
    pub fn scopes(&self) -> Vec<String> {
        self.scope.split_whitespace().map(str::to_owned).collect()
    }

    /// The grant includes full Drive (write).
    pub fn can_write(&self) -> bool {
        self.scope.split_whitespace().any(|s| s == SCOPE_DRIVE)
    }

    pub fn load(path: &Path) -> Result<Self, GoogleError> {
        let bytes = std::fs::read(path)
            .map_err(|e| err(format!("Drive token file {}: {}", path.display(), e.kind())))?;
        let t: Self = serde_json::from_slice(&bytes)
            .map_err(|_| err(format!("Drive token file {} is not valid", path.display())))?;
        if t.refresh_token.is_empty() {
            return Err(err("Drive token file has no refresh token"));
        }
        Ok(t)
    }

    /// Write 0600 through a temp file and a rename.
    pub fn save(&self, path: &Path) -> Result<(), GoogleError> {
        let body = serde_json::to_vec_pretty(self).map_err(|e| err(e.to_string()))?;
        write_private(path, &body)
    }
}

/// Write `body` to `path` with mode 0600, atomically.
pub fn write_private(path: &Path, body: &[u8]) -> Result<(), GoogleError> {
    use std::io::Write;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| err(format!("{}: {}", dir.display(), e.kind())))?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(body)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(format!("{}: {}", path.display(), e.kind())));
    }
    Ok(())
}

/// A consent in progress (`state` + PKCE verifier).
#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
    pub state: String,
    verifier: String,
    /// Unix seconds.
    pub created_at: i64,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("state", &self.state)
            .field("verifier", &"<redacted>")
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl Pending {
    pub fn new(now_unix: i64) -> Self {
        let r = || uuid::Uuid::new_v4().simple().to_string();
        Self {
            state: r(),
            // 64 chars of [0-9a-f], 244 random bits: a valid PKCE verifier.
            verifier: format!("{}{}", r(), r()),
            created_at: now_unix,
        }
    }

    pub fn challenge(&self) -> String {
        pkce_challenge(&self.verifier)
    }

    pub fn load(path: &Path) -> Result<Self, GoogleError> {
        let bytes = std::fs::read(path)
            .map_err(|_| err("no consent in progress: run `lucina google-auth drive` first"))?;
        serde_json::from_slice(&bytes).map_err(|_| err("the pending consent file is not valid"))
    }

    pub fn save(&self, path: &Path) -> Result<(), GoogleError> {
        let body = serde_json::to_vec(self).map_err(|e| err(e.to_string()))?;
        write_private(path, &body)
    }
}

pub fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The consent URL for Lucina's Drive.
pub fn authorize_url(
    ep: &GoogleEndpoints,
    client: &GoogleClient,
    redirect_uri: &str,
    pending: &Pending,
    login_hint: &str,
    drive_scope: &str,
) -> String {
    let scope = format!("{drive_scope} openid email");
    let challenge = pending.challenge();
    let params = [
        ("client_id", client.client_id.as_str()),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", scope.as_str()),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("include_granted_scopes", "true"),
        ("login_hint", login_hint),
        ("state", pending.state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    match reqwest::Url::parse_with_params(&ep.auth, params) {
        Ok(u) => u.to_string(),
        Err(_) => ep.auth.clone(),
    }
}

/// What the owner pasted: the whole redirect URL, its query, or a bare code.
/// Returns `(code, state)`.
pub fn parse_redirect(input: &str) -> Result<(String, Option<String>), GoogleError> {
    let s = input.trim().trim_matches(|c| c == '\'' || c == '"');
    if s.is_empty() {
        return Err(err("empty code"));
    }
    let query = if let Ok(u) = reqwest::Url::parse(s) {
        Some(u.query().unwrap_or("").to_owned())
    } else if s.contains("code=") || s.contains("error=") {
        Some(s.trim_start_matches('?').to_owned())
    } else {
        None
    };
    let Some(q) = query else {
        return Ok((s.to_owned(), None));
    };
    let pairs: Vec<(String, String)> = reqwest::Url::parse(&format!("http://x/?{q}"))
        .map(|u| u.query_pairs().into_owned().collect())
        .unwrap_or_default();
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    if let Some(e) = get("error") {
        return Err(err(format!("Google refused the consent: {e}")));
    }
    let code = get("code")
        .filter(|c| !c.is_empty())
        .ok_or_else(|| err("no code in that URL"))?;
    Ok((code, get("state")))
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    scope: Option<String>,
}

/// Reduce a Google error body to its codes (never echo request values).
async fn google_error(what: &str, resp: reqwest::Response) -> GoogleError {
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    let code = v["error"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| v["error"]["status"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let msg: String = v["error"]["message"]
        .as_str()
        .or_else(|| v["error_description"].as_str())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    let mut out = format!("{what}: HTTP {status}");
    if !code.is_empty() {
        out.push_str(&format!(" {code}"));
    }
    if !msg.is_empty() {
        out.push_str(&format!(" ({msg})"));
    }
    err(out)
}

fn net(what: &str, e: reqwest::Error) -> GoogleError {
    err(format!("{what}: {}", e.without_url()))
}

/// Finish a consent: check the state and age, exchange the code, and accept
/// only a grant with a refresh token, `drive.readonly`, for `expected_account`.
#[allow(clippy::too_many_arguments)]
pub async fn complete_consent(
    http: &reqwest::Client,
    ep: &GoogleEndpoints,
    client: &GoogleClient,
    redirect_uri: &str,
    pending: &Pending,
    pasted: &str,
    expected_account: &str,
    now_unix: i64,
) -> Result<DriveToken, GoogleError> {
    if now_unix - pending.created_at > PENDING_MAX_AGE_SECS {
        return Err(err(
            "the consent link expired: run `lucina google-auth drive` again",
        ));
    }
    let (code, state) = parse_redirect(pasted)?;
    if let Some(s) = state {
        if s != pending.state {
            return Err(err(
                "state mismatch: that URL belongs to another consent link",
            ));
        }
    }
    let resp = http
        .post(&ep.token)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("redirect_uri", redirect_uri),
            ("code_verifier", pending.verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|e| net("token exchange", e))?;
    if !resp.status().is_success() {
        let e = google_error("token exchange", resp).await;
        if e.0.contains("invalid_grant") {
            return Err(err(format!(
                "{} — the code expired or was already used: run `lucina google-auth drive` again",
                e.0
            )));
        }
        return Err(e);
    }
    let reply: TokenReply = resp
        .json()
        .await
        .map_err(|_| err("token exchange: unexpected reply"))?;
    let refresh = reply
        .refresh_token
        .filter(|r| !r.is_empty())
        .ok_or_else(|| err("Google sent no refresh token (consent must be offline)"))?;
    let scope = reply.scope.unwrap_or_default();
    if !scope
        .split_whitespace()
        .any(|s| s == SCOPE_DRIVE_READONLY || s == SCOPE_DRIVE)
    {
        return Err(err(
            "Drive read access was not granted: tick the Drive box on the consent page",
        ));
    }
    let email = userinfo_email(http, ep, &reply.access_token).await?;
    if !email.eq_ignore_ascii_case(expected_account) {
        return Err(err(format!(
            "that consent is for {email}, not {expected_account}: sign in as {expected_account} and try again"
        )));
    }
    Ok(DriveToken {
        refresh_token: refresh,
        scope,
        account_email: email.to_ascii_lowercase(),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}

async fn userinfo_email(
    http: &reqwest::Client,
    ep: &GoogleEndpoints,
    access_token: &str,
) -> Result<String, GoogleError> {
    let resp = http
        .get(&ep.userinfo)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| net("userinfo", e))?;
    if !resp.status().is_success() {
        return Err(google_error("userinfo", resp).await);
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|_| err("userinfo: unexpected reply"))?;
    v["email"]
        .as_str()
        .filter(|e| !e.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| err("userinfo: no email (the email scope is missing)"))
}

/// A Drive file's metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_time: Option<String>,
    /// Bytes (a string in Drive's JSON); absent for Google-native files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_view_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5_checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileList {
    #[serde(default)]
    pub files: Vec<DriveFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub folder_id: Option<String>,
    pub name_contains: Option<String>,
    pub full_text: Option<String>,
    pub page_size: u32,
    pub page_token: Option<String>,
}

/// Escape a value for a Drive `q` string literal.
pub fn q_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

impl ListQuery {
    pub fn q(&self) -> String {
        let mut q = vec!["trashed = false".to_owned()];
        if let Some(f) = self.folder_id.as_deref().filter(|s| !s.is_empty()) {
            let f = f.strip_prefix("gdrive:").unwrap_or(f);
            q.push(format!("'{}' in parents", q_escape(f)));
        }
        if let Some(n) = self.name_contains.as_deref().filter(|s| !s.is_empty()) {
            q.push(format!("name contains '{}'", q_escape(n)));
        }
        if let Some(t) = self.full_text.as_deref().filter(|s| !s.is_empty()) {
            q.push(format!("fullText contains '{}'", q_escape(t)));
        }
        q.join(" and ")
    }
}

/// How a file's content is fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Google-native: export as this MIME type.
    Export(&'static str),
    /// Stored bytes (`alt=media`).
    Media,
}

/// The export type for Google-native files (`None`: no text form).
pub fn export_type(mime: &str) -> Option<&'static str> {
    match mime {
        "application/vnd.google-apps.document" => Some("text/plain"),
        "application/vnd.google-apps.spreadsheet" => Some("text/csv"),
        "application/vnd.google-apps.presentation" => Some("text/plain"),
        _ => None,
    }
}

pub fn fetch_of(f: &DriveFile) -> Option<Fetch> {
    if f.mime_type.starts_with("application/vnd.google-apps.") {
        export_type(&f.mime_type).map(Fetch::Export)
    } else {
        Some(Fetch::Media)
    }
}

const TEXT_MIMES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/yaml",
    "application/x-yaml",
    "application/toml",
    "application/javascript",
    "application/x-sh",
    "application/sql",
];
const TEXT_EXTS: &[&str] = &[
    "md", "markdown", "txt", "rs", "toml", "json", "yaml", "yml", "csv", "tsv", "xml", "ini", "sh",
    "py", "js", "ts", "html", "css", "sql", "log",
];

/// Readable as text (Google-native with an export, or a text-like file).
pub fn is_text(f: &DriveFile) -> bool {
    if f.mime_type.starts_with("application/vnd.google-apps.") {
        return export_type(&f.mime_type).is_some();
    }
    if f.mime_type.starts_with("text/") || TEXT_MIMES.contains(&f.mime_type.as_str()) {
        return true;
    }
    f.name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| TEXT_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Cut `s` to at most `max` chars (on a char boundary). Returns whether it cut.
pub fn truncate_chars(s: &mut String, max: usize) -> bool {
    match s.char_indices().nth(max) {
        Some((i, _)) => {
            s.truncate(i);
            true
        }
        None => false,
    }
}

/// Lucina's Drive, read-only.
pub struct Drive {
    http: reqwest::Client,
    ep: GoogleEndpoints,
    client: GoogleClient,
    token: DriveToken,
    token_path: PathBuf,
    access: tokio::sync::Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for Drive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Drive")
            .field("account", &self.token.account_email)
            .field("token_path", &self.token_path)
            .finish()
    }
}

impl Drive {
    pub fn new(
        http: reqwest::Client,
        ep: GoogleEndpoints,
        client: GoogleClient,
        token: DriveToken,
        token_path: PathBuf,
    ) -> Self {
        Self {
            http,
            ep,
            client,
            token,
            token_path,
            access: tokio::sync::Mutex::new(None),
        }
    }

    /// Load the client and token files.
    pub fn open(
        http: reqwest::Client,
        ep: GoogleEndpoints,
        client_file: &Path,
        token_file: &Path,
    ) -> Result<Self, GoogleError> {
        let client = GoogleClient::from_file(client_file)?;
        let token = DriveToken::load(token_file)?;
        Ok(Self::new(http, ep, client, token, token_file.to_owned()))
    }

    pub fn account(&self) -> &str {
        &self.token.account_email
    }

    pub fn token_path(&self) -> &Path {
        &self.token_path
    }

    /// A valid access token (refreshed when it is within 60 s of expiry).
    async fn access_token(&self) -> Result<String, GoogleError> {
        let mut guard = self.access.lock().await;
        if let Some((tok, until)) = guard.as_ref() {
            if Instant::now() < *until {
                return Ok(tok.clone());
            }
        }
        let resp = self
            .http
            .post(&self.ep.token)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.token.refresh_token.as_str()),
                ("client_id", self.client.client_id.as_str()),
                ("client_secret", self.client.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|e| net("Google token refresh", e))?;
        if !resp.status().is_success() {
            let e = google_error("Google token refresh", resp).await;
            if e.0.contains("invalid_grant") {
                return Err(err(
                    "Drive access was revoked or expired: rerun `lucina google-auth drive`",
                ));
            }
            return Err(e);
        }
        let reply: TokenReply = resp
            .json()
            .await
            .map_err(|_| err("Google token refresh: unexpected reply"))?;
        let ttl = reply.expires_in.unwrap_or(3600).saturating_sub(60);
        let tok = reply.access_token;
        *guard = (ttl > 0).then(|| (tok.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(tok)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.ep.drive.trim_end_matches('/'))
    }

    pub async fn list(&self, q: &ListQuery) -> Result<FileList, GoogleError> {
        let token = self.access_token().await?;
        let size = q.page_size.clamp(1, 100).to_string();
        let fields = format!("nextPageToken,files({FILE_FIELDS})");
        let mut query: Vec<(&str, String)> = vec![
            ("q", q.q()),
            ("pageSize", size),
            ("fields", fields),
            ("orderBy", "modifiedTime desc".into()),
            ("supportsAllDrives", "true".into()),
            ("includeItemsFromAllDrives", "true".into()),
        ];
        if q.full_text.is_some() {
            // Drive refuses orderBy with fullText.
            query.retain(|(k, _)| *k != "orderBy");
        }
        if let Some(t) = q.page_token.as_deref().filter(|s| !s.is_empty()) {
            query.push(("pageToken", t.to_owned()));
        }
        let resp = self
            .http
            .get(self.url("files"))
            .bearer_auth(&token)
            .query(&query)
            .send()
            .await
            .map_err(|e| net("Drive list", e))?;
        if !resp.status().is_success() {
            return Err(google_error("Drive list", resp).await);
        }
        resp.json()
            .await
            .map_err(|_| err("Drive list: unexpected reply"))
    }

    pub async fn get(&self, id: &str) -> Result<DriveFile, GoogleError> {
        let token = self.access_token().await?;
        let resp = self
            .http
            .get(self.url(&format!("files/{}", path_segment(id)?)))
            .bearer_auth(&token)
            .query(&[("fields", FILE_FIELDS), ("supportsAllDrives", "true")])
            .send()
            .await
            .map_err(|e| net("Drive file", e))?;
        if !resp.status().is_success() {
            return Err(google_error("Drive file", resp).await);
        }
        resp.json()
            .await
            .map_err(|_| err("Drive file: unexpected reply"))
    }

    /// The file's bytes (export for Google-native types). `None` when a
    /// Google-native file has no export form.
    pub async fn content(&self, f: &DriveFile) -> Result<Option<Vec<u8>>, GoogleError> {
        let Some(fetch) = fetch_of(f) else {
            return Ok(None);
        };
        let token = self.access_token().await?;
        let id = path_segment(&f.id)?;
        let req = match fetch {
            Fetch::Export(mime) => self
                .http
                .get(self.url(&format!("files/{id}/export")))
                .query(&[("mimeType", mime)]),
            Fetch::Media => self
                .http
                .get(self.url(&format!("files/{id}")))
                .query(&[("alt", "media"), ("supportsAllDrives", "true")]),
        };
        let resp = req
            .bearer_auth(&token)
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|e| net("Drive download", e))?;
        if !resp.status().is_success() {
            return Err(google_error("Drive download", resp).await);
        }
        if resp
            .content_length()
            .is_some_and(|n| n as usize > MAX_DOWNLOAD_BYTES)
        {
            return Err(err(format!(
                "file too large (over {} MiB)",
                MAX_DOWNLOAD_BYTES / (1024 * 1024)
            )));
        }
        let bytes = resp.bytes().await.map_err(|e| net("Drive download", e))?;
        if bytes.len() > MAX_DOWNLOAD_BYTES {
            return Err(err("file too large"));
        }
        Ok(Some(bytes.to_vec()))
    }

    pub fn can_write(&self) -> bool {
        self.token.can_write()
    }

    fn require_write(&self) -> Result<(), GoogleError> {
        if self.can_write() {
            Ok(())
        } else {
            Err(err(
                "Drive is connected read-only: rerun `lucina google-auth drive` to grant write access",
            ))
        }
    }

    fn upload_url(&self, path: &str) -> String {
        format!("{}/{path}", self.ep.upload.trim_end_matches('/'))
    }

    /// Send a metadata (+ optional media) request and parse the file it returns.
    async fn file_reply(
        &self,
        what: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<DriveFile, GoogleError> {
        let token = self.access_token().await?;
        let resp = req
            .bearer_auth(&token)
            .query(&[("fields", FILE_FIELDS), ("supportsAllDrives", "true")])
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|e| net(what, e))?;
        if !resp.status().is_success() {
            return Err(google_error(what, resp).await);
        }
        resp.json()
            .await
            .map_err(|_| err(format!("{what}: unexpected reply")))
    }

    /// A new folder (in `parent`, else My Drive).
    pub async fn create_folder(
        &self,
        name: &str,
        parent: Option<&str>,
    ) -> Result<DriveFile, GoogleError> {
        self.require_write()?;
        let mut meta = serde_json::json!({"name": name, "mimeType": FOLDER_MIME});
        if let Some(p) = parent {
            meta["parents"] = serde_json::json!([path_segment(p)?]);
        }
        let req = self.http.post(self.url("files")).json(&meta);
        self.file_reply("Drive create folder", req).await
    }

    /// A new file from bytes. `convert_to` (a Google-native MIME type, e.g.
    /// a Doc) makes Drive convert the upload.
    pub async fn upload(
        &self,
        name: &str,
        bytes: &[u8],
        mime: &str,
        parent: Option<&str>,
        convert_to: Option<&str>,
    ) -> Result<DriveFile, GoogleError> {
        self.require_write()?;
        if bytes.len() > MAX_DOWNLOAD_BYTES {
            return Err(err("file too large"));
        }
        let mut meta = serde_json::json!({"name": name});
        if let Some(p) = parent {
            meta["parents"] = serde_json::json!([path_segment(p)?]);
        }
        if let Some(m) = convert_to {
            meta["mimeType"] = m.into();
        }
        let (content_type, body) = multipart_related(&meta, mime, bytes);
        let req = self
            .http
            .post(self.upload_url("files"))
            .query(&[("uploadType", "multipart")])
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
        self.file_reply("Drive upload", req).await
    }

    /// Replace a file's content (new revision).
    pub async fn update_content(
        &self,
        id: &str,
        bytes: &[u8],
        mime: &str,
    ) -> Result<DriveFile, GoogleError> {
        self.require_write()?;
        if bytes.len() > MAX_DOWNLOAD_BYTES {
            return Err(err("file too large"));
        }
        let req = self
            .http
            .patch(self.upload_url(&format!("files/{}", path_segment(id)?)))
            .query(&[("uploadType", "media")])
            .header(reqwest::header::CONTENT_TYPE, mime.to_owned())
            .body(bytes.to_vec());
        self.file_reply("Drive update", req).await
    }

    /// Rename, move (to `move_to`, out of every current parent) or trash
    /// (`trashed`: reversible from the Drive bin; nothing is deleted for good).
    pub async fn modify(
        &self,
        id: &str,
        name: Option<&str>,
        move_to: Option<&str>,
        trashed: Option<bool>,
    ) -> Result<DriveFile, GoogleError> {
        self.require_write()?;
        let id = path_segment(id)?;
        let mut meta = serde_json::Map::new();
        if let Some(n) = name {
            meta.insert("name".into(), n.into());
        }
        if let Some(t) = trashed {
            meta.insert("trashed".into(), t.into());
        }
        let mut req = self.http.patch(self.url(&format!("files/{id}")));
        if let Some(to) = move_to {
            let to = path_segment(to)?;
            let current = self.get(id).await?.parents.join(",");
            req = req.query(&[("addParents", to), ("removeParents", current.as_str())]);
        }
        let req = req.json(&serde_json::Value::Object(meta));
        self.file_reply("Drive modify", req).await
    }
}

/// A `multipart/related` body: JSON metadata, then the media.
fn multipart_related(meta: &Value, mime: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("lucina-{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/related; boundary={boundary}"), body)
}

/// A Drive id as one URL path segment (ids are `[A-Za-z0-9_-]`).
fn path_segment(id: &str) -> Result<&str, GoogleError> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > 200
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(err("not a Drive file id"));
    }
    Ok(id)
}

/// A Drive id from an id or a Drive link (`/file/d/<id>/…`, `?id=<id>`).
pub fn file_id_of(input: &str) -> Option<String> {
    let s = input.trim();
    let s = s.strip_prefix("gdrive:").unwrap_or(s);
    if path_segment(s).is_ok() {
        return Some(s.to_owned());
    }
    let u = reqwest::Url::parse(s).ok()?;
    if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == "id") {
        return path_segment(&v).ok().map(str::to_owned);
    }
    let segs: Vec<&str> = u.path_segments()?.collect();
    segs.windows(2)
        .find(|w| w[0] == "d")
        .and_then(|w| path_segment(w[1]).ok().map(str::to_owned))
}

/// A MIME type from a file name (uploads).
pub fn mime_for_name(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        "rs" => "text/x-rust",
        "zip" => "application/zip",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

/// SHA-256 of `bytes`, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET: &str = "client-secret-XYZ";
    const REFRESH: &str = "refresh-token-ABC";
    const ACCESS: &str = "access-token-DEF";

    fn eps(base: &str) -> GoogleEndpoints {
        GoogleEndpoints {
            auth: format!("{base}/auth"),
            token: format!("{base}/token"),
            userinfo: format!("{base}/userinfo"),
            drive: format!("{base}/drive/v3"),
            upload: format!("{base}/upload/drive/v3"),
        }
    }

    fn client() -> GoogleClient {
        GoogleClient::new("cid.apps.googleusercontent.com", SECRET)
    }

    fn token() -> DriveToken {
        DriveToken {
            refresh_token: REFRESH.into(),
            scope: SCOPE_DRIVE_READONLY.into(),
            account_email: DEFAULT_ACCOUNT.into(),
            created_at: "2026-10-24T12:00:00Z".into(),
        }
    }

    fn no_secret(s: &str) {
        for v in [SECRET, REFRESH, ACCESS] {
            assert!(!s.contains(v), "secret leaked in: {s}");
        }
    }

    #[test]
    fn client_json_shapes() {
        let web = br#"{"web":{"client_id":"a","client_secret":"b"}}"#;
        let inst = br#"{"installed":{"client_id":"a","client_secret":"b"}}"#;
        assert_eq!(GoogleClient::from_json(web).unwrap().client_id, "a");
        assert_eq!(GoogleClient::from_json(inst).unwrap().client_id, "a");
        assert!(GoogleClient::from_json(br#"{"web":{"client_id":"a"}}"#).is_err());
        no_secret(&format!("{:?} {:?}", client(), token()));
    }

    #[test]
    fn consent_url_has_pkce_offline_and_hint() {
        let p = Pending::new(0);
        let u = authorize_url(
            &eps("http://g"),
            &client(),
            DEFAULT_REDIRECT_URI,
            &p,
            DEFAULT_ACCOUNT,
            SCOPE_DRIVE,
        );
        let url = reqwest::Url::parse(&u).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge"], pkce_challenge(&p.verifier));
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["prompt"], "consent");
        assert_eq!(q["login_hint"], DEFAULT_ACCOUNT);
        assert_eq!(q["state"], p.state);
        assert!(q["scope"].starts_with(SCOPE_DRIVE) && q["scope"].contains("email"));
        assert!(!u.contains(&p.verifier));
        no_secret(&u);
    }

    #[test]
    fn pkce_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn redirect_parsing() {
        let (c, s) =
            parse_redirect("http://127.0.0.1:8788/oauth2callback?state=st&code=4%2F0Ab&scope=x")
                .unwrap();
        assert_eq!((c.as_str(), s.as_deref()), ("4/0Ab", Some("st")));
        assert_eq!(parse_redirect("'4/0Ab'").unwrap().0, "4/0Ab");
        assert_eq!(
            parse_redirect("?code=abc&state=z").unwrap().1.as_deref(),
            Some("z")
        );
        assert!(
            parse_redirect("http://127.0.0.1:8788/oauth2callback?error=access_denied").is_err()
        );
        assert!(parse_redirect("  ").is_err());
    }

    #[test]
    fn query_building_and_escaping() {
        let q = ListQuery {
            folder_id: Some("F1".into()),
            name_contains: Some("it's a \\ test".into()),
            ..Default::default()
        };
        assert_eq!(
            q.q(),
            "trashed = false and 'F1' in parents and name contains 'it\\'s a \\\\ test'"
        );
        let g = ListQuery {
            folder_id: Some("gdrive:F1".into()),
            ..Default::default()
        };
        assert_eq!(g.q(), "trashed = false and 'F1' in parents");
        let s = ListQuery {
            full_text: Some("outlook".into()),
            ..Default::default()
        };
        assert_eq!(s.q(), "trashed = false and fullText contains 'outlook'");
    }

    #[test]
    fn ids_and_links() {
        assert_eq!(
            file_id_of("https://drive.google.com/file/d/1obFTlrB7Ep3MezNjXaq_4GnTmtbH0yKf/view?utm_source=x")
                .as_deref(),
            Some("1obFTlrB7Ep3MezNjXaq_4GnTmtbH0yKf")
        );
        assert_eq!(
            file_id_of("https://drive.google.com/open?id=abc_DEF-1").as_deref(),
            Some("abc_DEF-1")
        );
        assert_eq!(file_id_of("abc123").as_deref(), Some("abc123"));
        assert_eq!(file_id_of("gdrive:abc_DEF-1").as_deref(), Some("abc_DEF-1"));
        assert!(file_id_of("../etc/passwd").is_none());
    }

    #[test]
    fn text_detection_and_truncation() {
        let f = |name: &str, mime: &str| DriveFile {
            id: "i".into(),
            name: name.into(),
            mime_type: mime.into(),
            modified_time: None,
            size: None,
            parents: vec![],
            web_view_link: None,
            md5_checksum: None,
            sha256_checksum: None,
        };
        assert!(is_text(&f("a.md", "application/octet-stream")));
        assert!(is_text(&f("doc", "application/vnd.google-apps.document")));
        assert!(!is_text(&f("form", "application/vnd.google-apps.form")));
        assert!(!is_text(&f("a.zip", "application/zip")));
        let mut s = "àèìòù".to_owned();
        assert!(truncate_chars(&mut s, 3));
        assert_eq!(s, "àèì");
        assert!(!truncate_chars(&mut s, 10));
    }

    #[test]
    fn token_file_is_private_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/token.json");
        token().save(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let t = DriveToken::load(&p).unwrap();
        assert_eq!(t.account_email, DEFAULT_ACCOUNT);
        assert_eq!(t.refresh_token, REFRESH);
    }

    async fn consent(
        srv: &MockServer,
        pending: &Pending,
        pasted: &str,
    ) -> Result<DriveToken, GoogleError> {
        complete_consent(
            &reqwest::Client::new(),
            &eps(&srv.uri()),
            &client(),
            DEFAULT_REDIRECT_URI,
            pending,
            pasted,
            DEFAULT_ACCOUNT,
            pending.created_at + 10,
        )
        .await
    }

    async fn mock_exchange(srv: &MockServer, refresh: Option<&str>, scope: &str, email: &str) {
        let mut body =
            serde_json::json!({"access_token": ACCESS, "expires_in": 3599, "scope": scope});
        if let Some(r) = refresh {
            body["refresh_token"] = r.into();
        }
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .and(header("authorization", format!("Bearer {ACCESS}").as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"email": email})),
            )
            .mount(srv)
            .await;
    }

    #[tokio::test]
    async fn consent_accepts_lucina_only() {
        let srv = MockServer::start().await;
        let scope =
            format!("openid {SCOPE_DRIVE_READONLY} https://www.googleapis.com/auth/userinfo.email");
        mock_exchange(&srv, Some(REFRESH), &scope, "MyLucina20@gmail.com").await;
        let p = Pending::new(1_000);
        let url = format!("{DEFAULT_REDIRECT_URI}?state={}&code=c1", p.state);
        let t = consent(&srv, &p, &url).await.unwrap();
        assert_eq!(t.account_email, DEFAULT_ACCOUNT);
        assert_eq!(t.refresh_token, REFRESH);
    }

    #[tokio::test]
    async fn consent_refuses_other_account_state_scope_and_missing_refresh() {
        let srv = MockServer::start().await;
        let scope = format!("openid {SCOPE_DRIVE_READONLY}");
        mock_exchange(&srv, Some(REFRESH), &scope, "giuseppe@example.com").await;
        let p = Pending::new(1_000);
        let e = consent(&srv, &p, "c1").await.unwrap_err();
        assert!(e.0.contains("not mylucina20@gmail.com"), "{e}");
        no_secret(&e.0);

        let e = consent(
            &srv,
            &p,
            &format!("{DEFAULT_REDIRECT_URI}?state=other&code=c1"),
        )
        .await
        .unwrap_err();
        assert!(e.0.contains("state mismatch"));

        let mut old = p.clone();
        old.created_at -= PENDING_MAX_AGE_SECS + 60;
        let e = complete_consent(
            &reqwest::Client::new(),
            &eps(&srv.uri()),
            &client(),
            DEFAULT_REDIRECT_URI,
            &old,
            "c1",
            DEFAULT_ACCOUNT,
            p.created_at,
        )
        .await
        .unwrap_err();
        assert!(e.0.contains("expired"));

        let srv2 = MockServer::start().await;
        mock_exchange(&srv2, None, &scope, DEFAULT_ACCOUNT).await;
        let e = consent(&srv2, &p, "c1").await.unwrap_err();
        assert!(e.0.contains("no refresh token"));

        let srv3 = MockServer::start().await;
        mock_exchange(&srv3, Some(REFRESH), "openid email", DEFAULT_ACCOUNT).await;
        let e = consent(&srv3, &p, "c1").await.unwrap_err();
        assert!(e.0.contains("not granted"));
        no_secret(&e.0);
    }

    fn drive(srv: &MockServer) -> Drive {
        Drive::new(
            reqwest::Client::new(),
            eps(&srv.uri()),
            client(),
            token(),
            PathBuf::from("/nonexistent"),
        )
    }

    async fn mock_refresh(srv: &MockServer, expires_in: u64, times: u64) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"access_token": ACCESS, "expires_in": expires_in}),
            ))
            .expect(times)
            .mount(srv)
            .await;
    }

    #[tokio::test]
    async fn access_token_is_cached_then_refreshed() {
        let srv = MockServer::start().await;
        mock_refresh(&srv, 3600, 1).await;
        let d = drive(&srv);
        d.access_token().await.unwrap();
        d.access_token().await.unwrap();
        drop(srv); // verifies exactly one refresh

        let srv = MockServer::start().await;
        // 30 s lifetime is already inside the 60 s margin: every call refreshes.
        mock_refresh(&srv, 30, 2).await;
        let d = drive(&srv);
        d.access_token().await.unwrap();
        d.access_token().await.unwrap();
    }

    #[tokio::test]
    async fn revoked_grant_is_a_clear_error() {
        let srv = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."}),
            ))
            .mount(&srv)
            .await;
        let e = drive(&srv).list(&ListQuery::default()).await.unwrap_err();
        assert!(e.0.contains("rerun `lucina google-auth drive`"), "{e}");
        no_secret(&e.0);
    }

    #[tokio::test]
    async fn list_get_and_content() {
        let srv = MockServer::start().await;
        mock_refresh(&srv, 3600, 1).await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .and(query_param("q", "trashed = false and name contains 'outlook'"))
            .and(header("authorization", format!("Bearer {ACCESS}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files": [{"id": "Z1", "name": "lucina-outlook-cursor-starter.zip", "mimeType": "application/zip",
                           "size": "5", "sha256Checksum": sha256_hex(b"PK..."), "parents": ["root"]}],
                "nextPageToken": "n2"
            })))
            .mount(&srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/Z1"))
            .and(query_param("alt", "media"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK...".to_vec()))
            .mount(&srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/D1/export"))
            .and(query_param("mimeType", "text/plain"))
            .respond_with(ResponseTemplate::new(200).set_body_string("doc text"))
            .mount(&srv)
            .await;
        let d = drive(&srv);
        let l = d
            .list(&ListQuery {
                name_contains: Some("outlook".into()),
                page_size: 20,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(l.next_page_token.as_deref(), Some("n2"));
        let z = &l.files[0];
        let bytes = d.content(z).await.unwrap().unwrap();
        assert_eq!(Some(sha256_hex(&bytes)), z.sha256_checksum);

        let doc = DriveFile {
            id: "D1".into(),
            name: "Spec".into(),
            mime_type: "application/vnd.google-apps.document".into(),
            ..z.clone()
        };
        assert_eq!(d.content(&doc).await.unwrap().unwrap(), b"doc text");
        let form = DriveFile {
            mime_type: "application/vnd.google-apps.form".into(),
            ..doc
        };
        assert!(d.content(&form).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn drive_errors_keep_googles_reason_without_secrets() {
        let srv = MockServer::start().await;
        mock_refresh(&srv, 3600, 1).await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/X"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {"code": 403, "status": "PERMISSION_DENIED",
                          "message": "Google Drive API has not been used in project 1 before or it is disabled."}
            })))
            .mount(&srv)
            .await;
        let e = drive(&srv).get("X").await.unwrap_err();
        assert!(
            e.0.contains("HTTP 403 PERMISSION_DENIED") && e.0.contains("Drive API"),
            "{e}"
        );
        no_secret(&e.0);
    }
}
