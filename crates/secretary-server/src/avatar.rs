//! Lucina's wardrobe: which outfit her 3D avatar wears.
//!
//! - `GET /v1/avatar` → [`AvatarSettings`]: the pinned outfit (`null` = none),
//!   whether the outfit follows the app section (`auto`), and the outfit per
//!   section (`rules`). Defaults when nothing is stored yet.
//! - `PUT /v1/avatar` merges a patch (`{outfit?, auto?, rules?: {gym?,
//!   shopping?, default?}}`; absent fields keep their value, `outfit: null`
//!   unpins) and answers the new settings. Unknown fields are refused. When
//!   the wardrobe is installed, every id must name one of its outfits
//!   (`422 unknown_outfit`).
//! - `GET /v1/avatar/wardrobe` → the valid entries of
//!   `{SECRETARY_PRIVATE_ASSETS_DIR}/wardrobe.json` (`[]` when it is missing).
//!
//! Who may call: a device (Bearer, or the cookie plus `X-Secretary-CSRF: 1`
//! for the PUT) or Lucina with the service token, so she can change her own
//! outfit when asked in chat. The settings live in `$SECRETARY_DATA_DIR/
//! avatar.json`, written atomically (temp file 0600, fsync, rename).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::devices::save_json_atomic;
use crate::error::{ApiError, JsonBody};
use crate::AppState;

pub const AVATAR_FILE: &str = "avatar.json";
pub const WARDROBE_FILE: &str = "wardrobe.json";
/// Outfit ids: `[a-z0-9][a-z0-9_-]*`, at most this long.
pub const MAX_ID_LEN: usize = 40;
/// At most this many wardrobe entries are served.
pub const MAX_OUTFITS: usize = 200;
/// The languages an entry's `name` may carry.
pub const NAME_LANGS: &[&str] = &["it", "en", "da"];

/// Outfit per app section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub gym: String,
    pub shopping: String,
    pub default: String,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            gym: "gym".into(),
            shopping: "chef".into(),
            default: "uniform".into(),
        }
    }
}

/// What `GET /v1/avatar` answers (and `avatar.json` holds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvatarSettings {
    /// Pinned outfit: worn everywhere until unpinned. `None` = follow `auto`.
    pub outfit: Option<String>,
    /// Change outfit with the app section (`rules`); off = `rules.default`.
    pub auto: bool,
    pub rules: Rules,
    /// Milliseconds since the epoch of the last change; 0 = never changed.
    #[serde(default)]
    pub updated_ms: u64,
    /// `web` | `ios` | `device` | `lucina`; empty = never changed.
    #[serde(default)]
    pub updated_by: String,
}

impl Default for AvatarSettings {
    fn default() -> Self {
        Self {
            outfit: None,
            auto: true,
            rules: Rules::default(),
            updated_ms: 0,
            updated_by: String::new(),
        }
    }
}

impl AvatarSettings {
    /// Every outfit id the settings name.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.outfit.as_deref().into_iter().chain([
            self.rules.gym.as_str(),
            self.rules.shopping.as_str(),
            self.rules.default.as_str(),
        ])
    }

    /// The outfit to show in `section` (`gym` | `shopping` | anything else).
    pub fn resolve(&self, section: &str) -> &str {
        if let Some(o) = &self.outfit {
            return o;
        }
        if !self.auto {
            return &self.rules.default;
        }
        match section {
            "gym" => &self.rules.gym,
            "shopping" => &self.rules.shopping,
            _ => &self.rules.default,
        }
    }
}

/// `Some(v)` for a present field, so `null` and "absent" differ.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesPatch {
    pub gym: Option<String>,
    pub shopping: Option<String>,
    pub default: Option<String>,
}

/// The `PUT /v1/avatar` body.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvatarPatch {
    #[serde(default, deserialize_with = "present")]
    pub outfit: Option<Option<String>>,
    pub auto: Option<bool>,
    pub rules: Option<RulesPatch>,
}

impl AvatarPatch {
    pub fn apply(self, mut s: AvatarSettings) -> AvatarSettings {
        if let Some(o) = self.outfit {
            s.outfit = o;
        }
        if let Some(a) = self.auto {
            s.auto = a;
        }
        if let Some(r) = self.rules {
            if let Some(v) = r.gym {
                s.rules.gym = v;
            }
            if let Some(v) = r.shopping {
                s.rules.shopping = v;
            }
            if let Some(v) = r.default {
                s.rules.default = v;
            }
        }
        s
    }
}

/// `[a-z0-9][a-z0-9_-]*`, 1..=[`MAX_ID_LEN`] bytes.
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_ID_LEN
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

fn bad_request(msg: String) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

/// Shape checks on new settings (ids well formed, and known when the
/// wardrobe lists outfits).
pub fn validate(s: &AvatarSettings, wardrobe: &[Value]) -> Result<(), ApiError> {
    for (field, id) in [
        ("outfit", s.outfit.as_deref()),
        ("rules.gym", Some(s.rules.gym.as_str())),
        ("rules.shopping", Some(s.rules.shopping.as_str())),
        ("rules.default", Some(s.rules.default.as_str())),
    ] {
        if let Some(id) = id {
            if !valid_id(id) {
                return Err(bad_request(format!(
                    "{field}: must match [a-z0-9][a-z0-9_-]* (at most {MAX_ID_LEN} characters)"
                )));
            }
        }
    }
    if wardrobe.is_empty() {
        return Ok(());
    }
    let known: Vec<&str> = wardrobe
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str))
        .collect();
    if let Some(bad) = s.ids().find(|id| !known.contains(id)) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_outfit",
            format!(
                "no outfit {bad:?} in the wardrobe; known: {}",
                known.join(", ")
            ),
        ));
    }
    Ok(())
}

/// A wardrobe entry worth serving: a valid `id`, a `.glb` asset name, an
/// optional `.png`/`.webp` thumbnail, `name` strings per language, string
/// `tags`. Other fields pass through unchanged.
fn valid_entry(e: &Value) -> bool {
    let Some(o) = e.as_object() else { return false };
    let id_ok = o.get("id").and_then(Value::as_str).is_some_and(valid_id);
    let glb_ok = o
        .get("glb")
        .and_then(Value::as_str)
        .is_some_and(|g| g.ends_with(".glb") && crate::assets::content_type(g).is_some());
    let thumb_ok = match o.get("thumbnail") {
        None | Some(Value::Null) => true,
        Some(Value::String(t)) => {
            (t.ends_with(".png") || t.ends_with(".webp"))
                && crate::assets::content_type(t).is_some()
        }
        _ => false,
    };
    let name_ok = match o.get("name") {
        Some(Value::Object(n)) => {
            !n.is_empty()
                && n.iter().all(|(k, v)| {
                    NAME_LANGS.contains(&k.as_str())
                        && v.as_str().is_some_and(|s| s.chars().count() <= 80)
                })
        }
        Some(Value::String(s)) => s.chars().count() <= 80,
        _ => false,
    };
    let tags_ok = match o.get("tags") {
        None => true,
        Some(Value::Array(t)) => {
            t.len() <= 16 && t.iter().all(|v| v.as_str().is_some_and(|s| s.len() <= 32))
        }
        _ => false,
    };
    id_ok && glb_ok && thumb_ok && name_ok && tags_ok
}

/// The valid entries of `dir/wardrobe.json` (unique ids, file order).
/// Missing file: empty. Unreadable or not a JSON array: empty, logged.
pub fn load_wardrobe(dir: &Path) -> Vec<Value> {
    let path = dir.join(WARDROBE_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "wardrobe unreadable");
            return Vec::new();
        }
    };
    let Ok(Value::Array(entries)) = serde_json::from_slice::<Value>(&bytes) else {
        tracing::warn!(path = %path.display(), "wardrobe.json is not a JSON array");
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for e in entries {
        if out.len() >= MAX_OUTFITS {
            break;
        }
        if !valid_entry(&e) {
            tracing::warn!(id = ?e.get("id"), "wardrobe entry skipped (invalid)");
            continue;
        }
        let id = e["id"].as_str().unwrap_or_default().to_owned();
        if seen.insert(id) {
            out.push(e);
        }
    }
    out
}

/// `avatar.json` in the data dir; one writer at a time.
pub struct AvatarStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl AvatarStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.to_owned(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(AVATAR_FILE)
    }

    /// The stored settings, or the defaults (missing file; a corrupt or
    /// invalid one is logged and ignored until the next save replaces it).
    pub fn load(&self) -> AvatarSettings {
        let path = self.path();
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<AvatarSettings>(&bytes) {
                Ok(s) if validate(&s, &[]).is_ok() => s,
                _ => {
                    tracing::warn!(path = %path.display(), "avatar.json invalid; using defaults");
                    AvatarSettings::default()
                }
            },
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), error = %e, "avatar.json unreadable; using defaults");
                }
                AvatarSettings::default()
            }
        }
    }

    /// Read, patch, validate against `wardrobe`, save. Returns the new settings.
    pub fn update(
        &self,
        patch: AvatarPatch,
        wardrobe: &[Value],
        by: &str,
        now_ms: u64,
    ) -> Result<AvatarSettings, ApiError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut next = patch.apply(self.load());
        validate(&next, wardrobe)?;
        next.updated_ms = now_ms;
        next.updated_by = by.to_owned();
        save_json_atomic(&self.dir, AVATAR_FILE, &next).map_err(|e| {
            tracing::error!(error = %e, "avatar.json write failed");
            ApiError::internal("could not save the avatar settings")
        })?;
        Ok(next)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Who made a change: `lucina` (service token), else the client header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Caller(&'static str);

fn client_label(headers: &HeaderMap) -> &'static str {
    match headers
        .get(crate::routes::CLIENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        Some("web") => "web",
        Some("ios") => "ios",
        _ => "device",
    }
}

/// A device (Bearer or cookie + CSRF for writes) or the service token.
async fn device_or_service(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if crate::service::service_bearer(req.headers()).is_some() {
        req.extensions_mut().insert(Caller("lucina"));
        return crate::service::require_service(State(st), req, next).await;
    }
    let label = client_label(req.headers());
    req.extensions_mut().insert(Caller(label));
    crate::auth::require_device(State(st), req, next).await
}

async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> R + Send + 'static,
) -> Result<R, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("avatar task failed"))
}

async fn get_avatar(State(st): State<AppState>) -> Result<Json<AvatarSettings>, ApiError> {
    let store = st.avatar.clone();
    Ok(Json(blocking(move || store.load()).await?))
}

async fn put_avatar(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    JsonBody(patch): JsonBody<AvatarPatch>,
) -> Result<Json<AvatarSettings>, ApiError> {
    let store = st.avatar.clone();
    let assets = st.private_assets_dir.clone();
    let saved = blocking(move || {
        let wardrobe = load_wardrobe(&assets);
        store.update(patch, &wardrobe, caller.0, now_ms())
    })
    .await??;
    tracing::info!(outfit = ?saved.outfit, auto = saved.auto, by = saved.updated_by, "avatar settings changed");
    Ok(Json(saved))
}

async fn wardrobe(State(st): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let assets = st.private_assets_dir.clone();
    Ok(Json(blocking(move || load_wardrobe(&assets)).await?))
}

/// The avatar routes with their own auth (device or service token).
pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/avatar", get(get_avatar).put(put_avatar))
        .route("/v1/avatar/wardrobe", get(wardrobe))
        .route_layer(middleware::from_fn_with_state(state, device_or_service))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_and_resolution() {
        for ok in ["uniform", "gym", "maid-2", "a", "x_1"] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", "-a", "_a", "A", "a b", "a/b", "a.b", &"a".repeat(41)] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        let mut s = AvatarSettings::default();
        assert_eq!(s.resolve("gym"), "gym");
        assert_eq!(s.resolve("shopping"), "chef");
        assert_eq!(s.resolve("chat"), "uniform");
        s.auto = false;
        assert_eq!(s.resolve("gym"), "uniform");
        s.outfit = Some("maid".into());
        assert_eq!(s.resolve("shopping"), "maid");
    }

    #[test]
    fn patch_distinguishes_null_and_absent() {
        let pinned = AvatarSettings {
            outfit: Some("maid".into()),
            ..AvatarSettings::default()
        };
        let p: AvatarPatch = serde_json::from_value(json!({"auto": false})).unwrap();
        let s = p.apply(pinned.clone());
        assert_eq!(s.outfit.as_deref(), Some("maid"));
        assert!(!s.auto);
        let p: AvatarPatch = serde_json::from_value(json!({"outfit": null})).unwrap();
        assert_eq!(p.apply(pinned).outfit, None);
        let p: AvatarPatch = serde_json::from_value(json!({"rules": {"gym": "sport"}})).unwrap();
        let s = p.apply(AvatarSettings::default());
        assert_eq!(s.rules.gym, "sport");
        assert_eq!(s.rules.default, "uniform");
        assert!(serde_json::from_value::<AvatarPatch>(json!({"colour": "red"})).is_err());
        assert!(serde_json::from_value::<AvatarPatch>(json!({"rules": {"chat": "x"}})).is_err());
        assert!(serde_json::from_value::<AvatarPatch>(json!({"auto": "yes"})).is_err());
    }

    #[test]
    fn wardrobe_entries_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WARDROBE_FILE),
            json!([
                {"id": "uniform", "name": {"it": "Divisa", "en": "Uniform"}, "glb": "lucina.glb", "thumbnail": "outfit-uniform.webp", "tags": ["default"]},
                {"id": "gym", "name": {"en": "Gym"}, "glb": "lucina-gym.glb"},
                {"id": "gym", "name": {"en": "Dup"}, "glb": "lucina-gym2.glb"},
                {"id": "Bad", "name": {"en": "x"}, "glb": "x.glb"},
                {"id": "evil", "name": {"en": "x"}, "glb": "../x.glb"},
                {"id": "notglb", "name": {"en": "x"}, "glb": "x.png"},
                {"id": "badname", "name": {"fr": "x"}, "glb": "x.glb"},
                {"id": "badthumb", "name": {"en": "x"}, "glb": "x.glb", "thumbnail": "x.json"},
                "string"
            ])
            .to_string(),
        )
        .unwrap();
        let w = load_wardrobe(dir.path());
        let ids: Vec<_> = w.iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["uniform", "gym"]);
        assert_eq!(w[0]["tags"], json!(["default"]));
        std::fs::write(dir.path().join(WARDROBE_FILE), "{}").unwrap();
        assert!(load_wardrobe(dir.path()).is_empty());
        assert!(load_wardrobe(&dir.path().join("nope")).is_empty());
    }

    #[test]
    fn store_round_trip_is_0600_and_validated() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = AvatarStore::new(&dir.path().join("state"));
        assert_eq!(store.load(), AvatarSettings::default());
        let wardrobe = vec![
            json!({"id": "uniform"}),
            json!({"id": "gym"}),
            json!({"id": "chef"}),
            json!({"id": "maid"}),
        ];
        let p: AvatarPatch = serde_json::from_value(json!({"outfit": "maid"})).unwrap();
        let s = store.update(p, &wardrobe, "web", 42).unwrap();
        assert_eq!(
            (s.outfit.as_deref(), s.updated_ms, s.updated_by.as_str()),
            (Some("maid"), 42, "web")
        );
        assert_eq!(store.load(), s);
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let p: AvatarPatch = serde_json::from_value(json!({"outfit": "pirate"})).unwrap();
        let e = store.update(p, &wardrobe, "web", 43).unwrap_err();
        assert_eq!(
            (e.status, e.code.as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "unknown_outfit")
        );
        assert_eq!(store.load(), s, "a refused patch changes nothing");
        let p: AvatarPatch = serde_json::from_value(json!({"outfit": "Bad Id"})).unwrap();
        assert_eq!(
            store.update(p, &[], "web", 44).unwrap_err().code,
            "bad_request"
        );
        std::fs::write(store.path(), "{not json").unwrap();
        assert_eq!(store.load(), AvatarSettings::default());
    }
}
