//! Lucina's wardrobe: which outfit, hairstyle and hair colour her 3D avatar
//! wears.
//!
//! - `GET /v1/avatar` → [`AvatarSettings`]: the pinned outfit and hairstyle
//!   (`null` = follow the rules), the hair colour (`null` = natural), whether
//!   the look follows the app section (`auto`), and the look per section
//!   (`rules: {gym: {outfit, hair}, shopping: {…}, default: {…}}`). Defaults
//!   when nothing is stored yet; the gym rule's hair is the ponytail.
//! - `PUT /v1/avatar` merges a patch (absent fields keep their value, `null`
//!   unpins / goes back to the natural colour; a rule may be given as an
//!   outfit id string, the older form) and answers the new settings. Unknown
//!   fields are refused. Ids must be in the installed wardrobe:
//!   `422 unknown_outfit` / `unknown_hairstyle` / `unknown_hair_color`.
//!   `natural` is always a valid hairstyle (her own hair).
//! - `GET /v1/avatar/wardrobe` → `{outfits, hairstyles, colors}`, the valid
//!   entries of `{SECRETARY_PRIVATE_ASSETS_DIR}/wardrobe.json` (which may also
//!   be a bare array of outfits); all empty when it is missing.
//!
//! Who may call: a device (Bearer, or the cookie plus `X-Secretary-CSRF: 1`
//! for the PUT) or Lucina with the service token, so she can change her own
//! look when asked in chat. The settings live in `$SECRETARY_DATA_DIR/
//! avatar.json`, written atomically (temp file 0600, fsync, rename); a file
//! from before hairstyles (rules as outfit strings) still loads.

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
use serde_json::{json, Value};

use crate::devices::save_json_atomic;
use crate::error::{ApiError, JsonBody};
use crate::AppState;

pub const AVATAR_FILE: &str = "avatar.json";
pub const WARDROBE_FILE: &str = "wardrobe.json";
/// Ids: `[a-z0-9][a-z0-9_-]*`, at most this long.
pub const MAX_ID_LEN: usize = 40;
/// At most this many entries per list are served.
pub const MAX_OUTFITS: usize = 200;
/// The languages an entry's `name` may carry.
pub const NAME_LANGS: &[&str] = &["it", "en", "da"];
/// Her own hair (no hairstyle part).
pub const NATURAL_HAIR: &str = "natural";

fn natural() -> String {
    NATURAL_HAIR.to_owned()
}

/// The look for one app section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rule {
    pub outfit: String,
    pub hair: String,
}

impl Rule {
    fn new(outfit: &str, hair: &str) -> Self {
        Self {
            outfit: outfit.into(),
            hair: hair.into(),
        }
    }
}

/// A rule as stored: `{outfit, hair?}`, or a bare outfit id (before hairstyles).
impl<'de> Deserialize<'de> for Rule {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Full {
            outfit: String,
            #[serde(default = "natural")]
            hair: String,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Outfit(String),
            Full(Full),
        }
        Ok(match Either::deserialize(d)? {
            Either::Outfit(outfit) => Rule {
                outfit,
                hair: natural(),
            },
            Either::Full(f) => Rule {
                outfit: f.outfit,
                hair: f.hair,
            },
        })
    }
}

/// The look per app section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub gym: Rule,
    pub shopping: Rule,
    pub default: Rule,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            gym: Rule::new("gym", "ponytail"),
            shopping: Rule::new("chef", NATURAL_HAIR),
            default: Rule::new("uniform", NATURAL_HAIR),
        }
    }
}

/// What `GET /v1/avatar` answers (and `avatar.json` holds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvatarSettings {
    /// Pinned outfit: worn everywhere until unpinned. `None` = follow the rules.
    pub outfit: Option<String>,
    /// Pinned hairstyle (`natural` = her own hair). `None` = follow the rules.
    #[serde(default)]
    pub hair: Option<String>,
    /// Hair colour id; `None` = natural.
    #[serde(default)]
    pub hair_color: Option<String>,
    /// Change the look with the app section (`rules`); off = `rules.default`.
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
            hair: None,
            hair_color: None,
            auto: true,
            rules: Rules::default(),
            updated_ms: 0,
            updated_by: String::new(),
        }
    }
}

impl AvatarSettings {
    fn rule(&self, section: &str) -> &Rule {
        if !self.auto {
            return &self.rules.default;
        }
        match section {
            "gym" => &self.rules.gym,
            "shopping" => &self.rules.shopping,
            _ => &self.rules.default,
        }
    }

    /// `(outfit, hair)` to show in `section` (`gym` | `shopping` | anything else).
    pub fn resolve(&self, section: &str) -> (&str, &str) {
        let rule = self.rule(section);
        (
            self.outfit.as_deref().unwrap_or(&rule.outfit),
            self.hair.as_deref().unwrap_or(&rule.hair),
        )
    }

    fn rules(&self) -> [(&'static str, &Rule); 3] {
        [
            ("gym", &self.rules.gym),
            ("shopping", &self.rules.shopping),
            ("default", &self.rules.default),
        ]
    }
}

/// `Some(v)` for a present field, so `null` and "absent" differ.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// One section in a patch: `{outfit?, hair?}`, or an outfit id string.
#[derive(Debug, Default)]
pub struct RulePatch {
    pub outfit: Option<String>,
    pub hair: Option<String>,
}

impl<'de> Deserialize<'de> for RulePatch {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Full {
            outfit: Option<String>,
            hair: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Outfit(String),
            Full(Full),
        }
        Ok(match Either::deserialize(d)? {
            Either::Outfit(o) => RulePatch {
                outfit: Some(o),
                hair: None,
            },
            Either::Full(f) => RulePatch {
                outfit: f.outfit,
                hair: f.hair,
            },
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesPatch {
    pub gym: Option<RulePatch>,
    pub shopping: Option<RulePatch>,
    pub default: Option<RulePatch>,
}

/// The `PUT /v1/avatar` body.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AvatarPatch {
    #[serde(default, deserialize_with = "present")]
    pub outfit: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub hair: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub hair_color: Option<Option<String>>,
    pub auto: Option<bool>,
    pub rules: Option<RulesPatch>,
}

impl AvatarPatch {
    pub fn apply(self, mut s: AvatarSettings) -> AvatarSettings {
        if let Some(o) = self.outfit {
            s.outfit = o;
        }
        if let Some(h) = self.hair {
            s.hair = h;
        }
        if let Some(c) = self.hair_color {
            s.hair_color = c;
        }
        if let Some(a) = self.auto {
            s.auto = a;
        }
        if let Some(r) = self.rules {
            for (patch, rule) in [
                (r.gym, &mut s.rules.gym),
                (r.shopping, &mut s.rules.shopping),
                (r.default, &mut s.rules.default),
            ] {
                let Some(p) = patch else { continue };
                if let Some(o) = p.outfit {
                    rule.outfit = o;
                }
                if let Some(h) = p.hair {
                    rule.hair = h;
                }
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

/// The installed wardrobe (`GET /v1/avatar/wardrobe`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Wardrobe {
    pub outfits: Vec<Value>,
    pub hairstyles: Vec<Value>,
    pub colors: Vec<Value>,
}

fn ids(list: &[Value]) -> Vec<&str> {
    list.iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str))
        .collect()
}

fn unknown(code: &str, what: &str, id: &str, known: &[&str]) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        code,
        format!(
            "no {what} {id:?} in the wardrobe; known: {}",
            known.join(", ")
        ),
    )
}

/// Shape checks on new settings (ids well formed, and known when the
/// wardrobe lists that kind of item).
pub fn validate(s: &AvatarSettings, w: &Wardrobe) -> Result<(), ApiError> {
    let mut outfits: Vec<(String, &str)> = Vec::new();
    if let Some(o) = &s.outfit {
        outfits.push(("outfit".into(), o));
    }
    let mut hairs: Vec<(String, &str)> = Vec::new();
    if let Some(h) = &s.hair {
        hairs.push(("hair".into(), h));
    }
    for (name, rule) in s.rules() {
        outfits.push((format!("rules.{name}.outfit"), &rule.outfit));
        hairs.push((format!("rules.{name}.hair"), &rule.hair));
    }
    let colors: Vec<(String, &str)> = s
        .hair_color
        .as_deref()
        .map(|c| vec![("hairColor".to_owned(), c)])
        .unwrap_or_default();
    for (field, id) in outfits.iter().chain(&hairs).chain(&colors) {
        if !valid_id(id) {
            return Err(bad_request(format!(
                "{field}: must match [a-z0-9][a-z0-9_-]* (at most {MAX_ID_LEN} characters)"
            )));
        }
    }
    let known = ids(&w.outfits);
    if !known.is_empty() {
        if let Some((_, bad)) = outfits.iter().find(|(_, id)| !known.contains(id)) {
            return Err(unknown("unknown_outfit", "outfit", bad, &known));
        }
    }
    let mut known = ids(&w.hairstyles);
    if !known.is_empty() {
        if !known.contains(&NATURAL_HAIR) {
            known.insert(0, NATURAL_HAIR);
        }
        if let Some((_, bad)) = hairs.iter().find(|(_, id)| !known.contains(id)) {
            return Err(unknown("unknown_hairstyle", "hairstyle", bad, &known));
        }
    }
    let known = ids(&w.colors);
    if !known.is_empty() {
        if let Some((_, bad)) = colors.iter().find(|(_, id)| !known.contains(id)) {
            return Err(unknown("unknown_hair_color", "hair colour", bad, &known));
        }
    }
    Ok(())
}

fn asset_ok(v: Option<&Value>, exts: &[&str], required: bool) -> bool {
    match v {
        None | Some(Value::Null) => !required,
        Some(Value::String(t)) => {
            exts.iter().any(|e| t.ends_with(e)) && crate::assets::content_type(t).is_some()
        }
        _ => false,
    }
}

fn name_ok(v: Option<&Value>, required: bool) -> bool {
    match v {
        None => !required,
        Some(Value::Object(n)) => {
            !n.is_empty()
                && n.iter().all(|(k, v)| {
                    NAME_LANGS.contains(&k.as_str())
                        && v.as_str().is_some_and(|s| s.chars().count() <= 80)
                })
        }
        Some(Value::String(s)) => s.chars().count() <= 80,
        _ => false,
    }
}

fn strings_ok(v: Option<&Value>, max: usize, len: usize) -> bool {
    match v {
        None => true,
        Some(Value::Array(t)) => {
            t.len() <= max
                && t.iter()
                    .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= len))
        }
        _ => false,
    }
}

fn id_ok(o: &serde_json::Map<String, Value>) -> bool {
    o.get("id").and_then(Value::as_str).is_some_and(valid_id)
}

/// An outfit worth serving: a valid `id`, a `.glb` asset name, an optional
/// `.png`/`.webp` thumbnail, `name` strings per language, string `tags`.
/// Other fields pass through unchanged.
fn valid_outfit(e: &Value) -> bool {
    let Some(o) = e.as_object() else { return false };
    id_ok(o)
        && asset_ok(o.get("glb"), &[".glb"], true)
        && asset_ok(o.get("thumbnail"), &[".png", ".webp"], false)
        && name_ok(o.get("name"), true)
        && strings_ok(o.get("tags"), 16, 32)
}

/// A hairstyle: like an outfit (`natural` needs no glb and no name), plus
/// `hideTiara` (bool) and `hideMaterials` (strings).
fn valid_hairstyle(e: &Value) -> bool {
    let Some(o) = e.as_object() else { return false };
    let natural = o.get("id").and_then(Value::as_str) == Some(NATURAL_HAIR);
    id_ok(o)
        && asset_ok(o.get("glb"), &[".glb"], !natural)
        && asset_ok(o.get("thumbnail"), &[".png", ".webp"], false)
        && name_ok(o.get("name"), !natural)
        && strings_ok(o.get("tags"), 16, 32)
        && matches!(o.get("hideTiara"), None | Some(Value::Bool(_)))
        && strings_ok(o.get("hideMaterials"), 32, 64)
}

/// A hair colour: `id`, `hex` (`#rrggbb`), optional `name`.
fn valid_color(e: &Value) -> bool {
    let Some(o) = e.as_object() else { return false };
    let hex_ok = o.get("hex").and_then(Value::as_str).is_some_and(|h| {
        h.len() == 7 && h.starts_with('#') && h[1..].bytes().all(|b| b.is_ascii_hexdigit())
    });
    id_ok(o) && hex_ok && name_ok(o.get("name"), false)
}

/// The valid entries of one list (unique ids, file order, at most [`MAX_OUTFITS`]).
fn valid_list(v: Option<&Value>, ok: fn(&Value) -> bool, what: &str) -> Vec<Value> {
    let Some(Value::Array(entries)) = v else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for e in entries {
        if out.len() >= MAX_OUTFITS {
            break;
        }
        if !ok(e) {
            tracing::warn!(id = ?e.get("id"), what, "wardrobe entry skipped (invalid)");
            continue;
        }
        let id = e["id"].as_str().unwrap_or_default().to_owned();
        if seen.insert(id) {
            out.push(e.clone());
        }
    }
    out
}

/// The valid entries of `dir/wardrobe.json` (`{outfits, hairstyles, colors}`
/// or a bare array of outfits). Missing file: empty. Unreadable or not
/// JSON: empty, logged.
pub fn load_wardrobe(dir: &Path) -> Wardrobe {
    let path = dir.join(WARDROBE_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Wardrobe::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "wardrobe unreadable");
            return Wardrobe::default();
        }
    };
    let root: Value = match serde_json::from_slice(&bytes) {
        Ok(v @ (Value::Array(_) | Value::Object(_))) => v,
        _ => {
            tracing::warn!(path = %path.display(), "wardrobe.json is not a JSON array or object");
            return Wardrobe::default();
        }
    };
    if root.is_array() {
        return Wardrobe {
            outfits: valid_list(Some(&root), valid_outfit, "outfit"),
            ..Wardrobe::default()
        };
    }
    Wardrobe {
        outfits: valid_list(root.get("outfits"), valid_outfit, "outfit"),
        hairstyles: valid_list(root.get("hairstyles"), valid_hairstyle, "hairstyle"),
        colors: valid_list(root.get("colors"), valid_color, "color"),
    }
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
                Ok(s) if validate(&s, &Wardrobe::default()).is_ok() => s,
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
        wardrobe: &Wardrobe,
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
    tracing::info!(
        outfit = ?saved.outfit,
        hair = ?saved.hair,
        hair_color = ?saved.hair_color,
        auto = saved.auto,
        by = saved.updated_by,
        "avatar settings changed"
    );
    Ok(Json(saved))
}

async fn wardrobe(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let assets = st.private_assets_dir.clone();
    let w = blocking(move || load_wardrobe(&assets)).await?;
    Ok(Json(json!({
        "outfits": w.outfits,
        "hairstyles": w.hairstyles,
        "colors": w.colors,
    })))
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

    fn wardrobe() -> Wardrobe {
        Wardrobe {
            outfits: ["uniform", "gym", "chef", "maid"]
                .iter()
                .map(|id| json!({"id": id}))
                .collect(),
            hairstyles: ["ponytail", "bun"]
                .iter()
                .map(|id| json!({"id": id}))
                .collect(),
            colors: vec![json!({"id": "pink", "hex": "#ff88cc"})],
        }
    }

    #[test]
    fn ids_and_resolution() {
        for ok in ["uniform", "gym", "maid-2", "a", "x_1"] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", "-a", "_a", "A", "a b", "a/b", "a.b", &"a".repeat(41)] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        let mut s = AvatarSettings::default();
        assert_eq!(s.resolve("gym"), ("gym", "ponytail"));
        assert_eq!(s.resolve("shopping"), ("chef", "natural"));
        assert_eq!(s.resolve("chat"), ("uniform", "natural"));
        s.auto = false;
        assert_eq!(s.resolve("gym"), ("uniform", "natural"));
        s.outfit = Some("maid".into());
        s.hair = Some("bun".into());
        assert_eq!(s.resolve("shopping"), ("maid", "bun"));
    }

    #[test]
    fn patch_distinguishes_null_and_absent_and_takes_old_rules() {
        let pinned = AvatarSettings {
            outfit: Some("maid".into()),
            hair: Some("bun".into()),
            hair_color: Some("pink".into()),
            ..AvatarSettings::default()
        };
        let p: AvatarPatch = serde_json::from_value(json!({"auto": false})).unwrap();
        let s = p.apply(pinned.clone());
        assert_eq!(
            (s.outfit.as_deref(), s.hair.as_deref(), s.auto),
            (Some("maid"), Some("bun"), false)
        );
        let p: AvatarPatch =
            serde_json::from_value(json!({"outfit": null, "hair": null, "hairColor": null}))
                .unwrap();
        let s = p.apply(pinned);
        assert_eq!((s.outfit, s.hair, s.hair_color), (None, None, None));
        let p: AvatarPatch =
            serde_json::from_value(json!({"rules": {"gym": {"hair": "bun"}, "shopping": "maid"}}))
                .unwrap();
        let s = p.apply(AvatarSettings::default());
        assert_eq!(s.rules.gym, Rule::new("gym", "bun"));
        assert_eq!(s.rules.shopping, Rule::new("maid", "natural"));
        assert_eq!(s.rules.default, Rule::new("uniform", "natural"));
        for bad in [
            json!({"colour": "red"}),
            json!({"rules": {"chat": "x"}}),
            json!({"rules": {"gym": {"shoes": "x"}}}),
            json!({"auto": "yes"}),
            json!({"hair_color": "pink"}),
        ] {
            assert!(
                serde_json::from_value::<AvatarPatch>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        // avatar.json from before hairstyles.
        let old: AvatarSettings = serde_json::from_value(json!({
            "outfit": null, "auto": true,
            "rules": {"gym": "gym", "shopping": "maid", "default": "uniform"},
            "updatedMs": 5, "updatedBy": "web"
        }))
        .unwrap();
        assert_eq!(old.rules.shopping, Rule::new("maid", "natural"));
        assert_eq!(old.hair, None);
    }

    #[test]
    fn validation_against_the_wardrobe() {
        let w = wardrobe();
        let mut s = AvatarSettings::default();
        assert!(validate(&s, &w).is_ok());
        s.hair = Some("natural".into());
        assert!(validate(&s, &w).is_ok(), "natural is always there");
        s.hair = Some("mohawk".into());
        assert_eq!(validate(&s, &w).unwrap_err().code, "unknown_hairstyle");
        s.hair = None;
        s.rules.gym.outfit = "pirate".into();
        assert_eq!(validate(&s, &w).unwrap_err().code, "unknown_outfit");
        s.rules.gym.outfit = "gym".into();
        s.hair_color = Some("green".into());
        assert_eq!(validate(&s, &w).unwrap_err().code, "unknown_hair_color");
        s.hair_color = Some("Bad Id".into());
        assert_eq!(validate(&s, &w).unwrap_err().code, "bad_request");
        s.hair_color = None;
        // Without hairstyles installed, any well-formed hairstyle id is fine.
        let no_hair = Wardrobe {
            hairstyles: vec![],
            ..wardrobe()
        };
        s.hair = Some("mohawk".into());
        assert!(validate(&s, &no_hair).is_ok());
    }

    #[test]
    fn wardrobe_files_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WARDROBE_FILE),
            json!({
                "outfits": [
                    {"id": "uniform", "name": {"it": "Divisa", "en": "Uniform"}, "glb": "lucina.glb", "thumbnail": "outfit-uniform.webp", "tags": ["default"]},
                    {"id": "gym", "name": {"en": "Gym"}, "glb": "lucina-gym.glb"},
                    {"id": "gym", "name": {"en": "Dup"}, "glb": "lucina-gym2.glb"},
                    {"id": "Bad", "name": {"en": "x"}, "glb": "x.glb"},
                    {"id": "evil", "name": {"en": "x"}, "glb": "../x.glb"},
                    {"id": "badname", "name": {"fr": "x"}, "glb": "x.glb"}
                ],
                "hairstyles": [
                    {"id": "natural"},
                    {"id": "ponytail", "name": {"en": "Ponytail"}, "glb": "lucina-hair-ponytail.glb", "hideTiara": true},
                    {"id": "noglb", "name": {"en": "x"}},
                    {"id": "badtiara", "name": {"en": "x"}, "glb": "x.glb", "hideTiara": "yes"}
                ],
                "colors": [
                    {"id": "pink", "hex": "#ff88cc", "name": {"en": "Pink"}},
                    {"id": "red", "hex": "red"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let w = load_wardrobe(dir.path());
        assert_eq!(ids(&w.outfits), ["uniform", "gym"]);
        assert_eq!(ids(&w.hairstyles), ["natural", "ponytail"]);
        assert_eq!(ids(&w.colors), ["pink"]);
        assert_eq!(w.hairstyles[1]["hideTiara"], true);
        // The older bare array is the outfits.
        std::fs::write(
            dir.path().join(WARDROBE_FILE),
            json!([{"id": "gym", "name": "Gym", "glb": "lucina-gym.glb"}]).to_string(),
        )
        .unwrap();
        let w = load_wardrobe(dir.path());
        assert_eq!((ids(&w.outfits), w.hairstyles.len()), (vec!["gym"], 0));
        std::fs::write(dir.path().join(WARDROBE_FILE), "3").unwrap();
        assert_eq!(load_wardrobe(dir.path()), Wardrobe::default());
        assert_eq!(load_wardrobe(&dir.path().join("nope")), Wardrobe::default());
    }

    #[test]
    fn store_round_trip_is_0600_and_validated() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = AvatarStore::new(&dir.path().join("state"));
        assert_eq!(store.load(), AvatarSettings::default());
        let w = wardrobe();
        let p: AvatarPatch =
            serde_json::from_value(json!({"outfit": "maid", "hair": "bun", "hairColor": "pink"}))
                .unwrap();
        let s = store.update(p, &w, "web", 42).unwrap();
        assert_eq!(
            (
                s.outfit.as_deref(),
                s.hair.as_deref(),
                s.hair_color.as_deref(),
                s.updated_ms,
                s.updated_by.as_str()
            ),
            (Some("maid"), Some("bun"), Some("pink"), 42, "web")
        );
        assert_eq!(store.load(), s);
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let p: AvatarPatch = serde_json::from_value(json!({"hair": "mohawk"})).unwrap();
        let e = store.update(p, &w, "web", 43).unwrap_err();
        assert_eq!(
            (e.status, e.code.as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "unknown_hairstyle")
        );
        assert_eq!(store.load(), s, "a refused patch changes nothing");
        std::fs::write(store.path(), "{not json").unwrap();
        assert_eq!(store.load(), AvatarSettings::default());
    }
}
