//! The owner's Instagram and Pixiv plugins as tools (`secretary-instagram` on
//! `127.0.0.1:8871`, `secretary-pixiv` on `127.0.0.1:8872`, both on nezopt).
//!
//! - `instagram_profile`, `instagram_stories`: read-only, anonymous upstream.
//! - `instagram_watchlist`, `instagram_watch`, `instagram_unwatch`, `instagram_new`: the
//!   owner's watchlist kept by the Instagram service (`/v1/instagram/watch…`): who is
//!   watched, what is new since he last opened it, watch / unwatch (his own list only).
//! - `instagram_save_latest`: the newest post or story of an account into the Gallery.
//! - `pixiv_search` (always `mode=safe`), `pixiv_ranking` (never an `*_r18`
//!   mode): the `adult` switch of the service is never sent, and an R-18 item
//!   that still shows up in a list is dropped here too.
//! - `save_to_gallery`: an Instagram item (a signed `url` from an earlier
//!   result) or a Pixiv work (illustration id) goes into the owner's Gallery
//!   (`POST /save`, source `instagram` / `pixiv`).
//!
//! Owner only: all five are in [`crate::OWNER_ONLY_TOOLS`] (never on a Lucina
//! MCP turn) and each call refuses [`crate::Origin::Mcp`] itself. Every result
//! marks the turn private ([`Effect::PrivateTurn`]): the server keeps it out of
//! Honcho. The timeline line of a call is the tool name only (no username,
//! query or id).
//!
//! Auth to the services is the gateway secret in `X-Lucina-Gateway`, read from
//! `~/.config/secretary-<svc>/gateway.secret` per call (overridable with
//! `LUCINA_INSTAGRAM_GATEWAY_SECRET_FILE` / `LUCINA_PIXIV_GATEWAY_SECRET_FILE`;
//! URLs with `LUCINA_INSTAGRAM_URL` / `LUCINA_PIXIV_URL`). A missing secret
//! file or a service that is down answers "not available", not a stack trace.
//!
//! Media urls in the services' answers are signed relative paths
//! (`/v1/<svc>/s/…`, about 6 h); they are returned as absolute
//! `LUCINA_PUBLIC_URL` (default `https://getsecretary.tech`) urls so a reply
//! can link them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::{schema_for, Ctx, Effect, Origin, SideEffect, Tool, ToolError, ToolResult};

/// Items of a list a result keeps (the model sees no more).
const MAX_ITEMS: usize = 24;
/// Instagram items one `save_to_gallery` call may save.
const MAX_SAVE: usize = 30;
/// Instagram items remembered per chat for "save it to the gallery".
const MAX_REMEMBERED: usize = 200;
const CAPTION_CHARS: usize = 300;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

pub const INSTAGRAM_PREFIX: &str = "/v1/instagram/";
pub const PIXIV_PREFIX: &str = "/v1/pixiv/";

/// Names of the tools in this module (the server adds them to the owner-only list).
pub const NAMES: &[&str] = &[
    "instagram_profile",
    "instagram_stories",
    "instagram_watchlist",
    "instagram_watch",
    "instagram_unwatch",
    "instagram_new",
    "instagram_save_latest",
    "pixiv_search",
    "pixiv_ranking",
    "save_to_gallery",
];

/// One plugin service on this host.
#[derive(Debug, Clone)]
pub struct Service {
    pub label: &'static str,
    pub url: String,
    pub secret_file: PathBuf,
}

/// An Instagram item of the latest result in a chat, for `save_to_gallery`.
#[derive(Debug, Clone, PartialEq)]
pub struct Remembered {
    /// The signed path as the service gave it (`/v1/instagram/s/…`).
    pub url: String,
    pub kind: String,
    pub username: String,
    pub taken_at: Option<Value>,
    pub caption: Option<String>,
}

/// Where the plugin services are and what the last Instagram result held.
#[derive(Debug)]
pub struct Plugins {
    pub instagram: Service,
    pub pixiv: Service,
    /// Absolute prefix for the signed media paths, no trailing slash.
    pub public_url: String,
    last_instagram: Mutex<HashMap<String, Vec<Remembered>>>,
    /// When a plugin tool last answered in a chat (so "save it to the gallery" can be routed).
    touched: Mutex<HashMap<String, std::time::Instant>>,
}

impl Plugins {
    /// Defaults: loopback ports 8871 / 8872, secrets under `$HOME/.config/secretary-<svc>/`.
    pub fn new(home: Option<&std::path::Path>) -> Self {
        let secret = |svc: &str| {
            home.map(|h| h.join(format!(".config/secretary-{svc}/gateway.secret")))
                .unwrap_or_else(|| PathBuf::from(format!("/nonexistent/secretary-{svc}.secret")))
        };
        Self {
            instagram: Service {
                label: "Instagram",
                url: "http://127.0.0.1:8871".into(),
                secret_file: secret("instagram"),
            },
            pixiv: Service {
                label: "Pixiv",
                url: "http://127.0.0.1:8872".into(),
                secret_file: secret("pixiv"),
            },
            public_url: "https://getsecretary.tech".into(),
            last_instagram: Mutex::new(HashMap::new()),
            touched: Mutex::new(HashMap::new()),
        }
    }

    fn touch(&self, session: &str) {
        let mut map = self.touched.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 64 && !map.contains_key(session) {
            map.clear();
        }
        map.insert(session.to_owned(), std::time::Instant::now());
    }

    /// A plugin tool answered in `session` within the last 30 minutes.
    pub fn recently_used(&self, session: &str) -> bool {
        self.touched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(30 * 60))
    }

    fn remember(&self, session: &str, items: Vec<Remembered>) {
        let mut map = self
            .last_instagram
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if map.len() > 64 && !map.contains_key(session) {
            map.clear();
        }
        map.insert(session.to_owned(), items);
    }

    /// The items of the latest Instagram result in `session`.
    pub fn remembered(&self, session: &str) -> Vec<Remembered> {
        self.last_instagram
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned()
            .unwrap_or_default()
    }
}

impl Default for Plugins {
    fn default() -> Self {
        Self::new(std::env::var_os("HOME").map(PathBuf::from).as_deref())
    }
}

// ---------------------------------------------------------------- plumbing

fn owner_only(ctx: &Ctx) -> Result<(), ToolError> {
    if ctx.origin == Origin::Mcp {
        return Err(ToolError::new(
            "Instagram and Pixiv are for the owner's own chat only",
        ));
    }
    Ok(())
}

fn unavailable(svc: &Service, why: &str) -> ToolError {
    ToolError::new(format!("{} is not available: {why}", svc.label))
}

fn secret(svc: &Service) -> Result<String, ToolError> {
    let s = std::fs::read_to_string(&svc.secret_file)
        .map_err(|_| unavailable(svc, "its gateway secret is missing on this host"))?;
    let s = s.trim().to_owned();
    if s.is_empty() || s.contains(['\r', '\n']) {
        return Err(unavailable(svc, "its gateway secret is unusable"));
    }
    Ok(s)
}

async fn send(
    ctx: &Ctx,
    svc: &Service,
    method: reqwest::Method,
    path: &str,
    query: &[(&str, String)],
    body: Option<Value>,
) -> Result<Value, ToolError> {
    let key = secret(svc)?;
    let url = format!("{}{path}", svc.url.trim_end_matches('/'));
    let mut req = ctx
        .env
        .http
        .request(method, url)
        .header("X-Lucina-Gateway", key)
        .timeout(REQUEST_TIMEOUT)
        .query(query);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            ToolError::new(format!("{} did not answer in time, try again", svc.label))
        } else {
            unavailable(svc, "the service is not running on this host")
        }
    })?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    let code = body["error"]
        .as_str()
        .or_else(|| body["error"]["code"].as_str())
        .unwrap_or_default();
    let message = body["message"]
        .as_str()
        .or_else(|| body["error"]["message"].as_str())
        .unwrap_or_default();
    Err(ToolError::new(match status.as_u16() {
        401 | 403 if code == "adult_off" => {
            format!(
                "{}: adult content is switched off and stays off here",
                svc.label
            )
        }
        401 | 403 => format!(
            "{} is not available: the service refused this host",
            svc.label
        ),
        404 => format!("{}: not found", svc.label),
        429 => match body["retryAfter"].as_u64() {
            Some(s) => format!("{} is rate limited, try again in about {s} s", svc.label),
            None => format!("{} is rate limited, try again in a minute", svc.label),
        },
        s => format!("{} answered {s}: {code} {message}", svc.label)
            .trim()
            .to_owned(),
    }))
}

/// Instagram handles: letters, digits, `.` and `_`, at most 30; a leading `@` is dropped.
fn username(raw: &str) -> Result<String, ToolError> {
    let u = raw.trim().trim_start_matches('@');
    let ok = !u.is_empty()
        && u.len() <= 30
        && u.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_');
    if !ok {
        return Err(ToolError::new(
            "that is not an Instagram username (letters, digits, . and _ only)",
        ));
    }
    Ok(u.to_owned())
}

/// Make the signed media paths absolute, shorten long lists and captions.
/// Returns true when something was cut.
fn tidy(v: &mut Value, public: &str) -> bool {
    let mut cut = false;
    match v {
        Value::String(s) => {
            if s.starts_with(INSTAGRAM_PREFIX) || s.starts_with(PIXIV_PREFIX) {
                *s = format!("{public}{s}");
            } else if s.chars().count() > CAPTION_CHARS {
                *s = s.chars().take(CAPTION_CHARS - 1).collect::<String>() + "…";
            }
        }
        Value::Array(a) => {
            if a.len() > MAX_ITEMS {
                a.truncate(MAX_ITEMS);
                cut = true;
            }
            for x in a {
                cut |= tidy(x, public);
            }
        }
        Value::Object(o) => {
            for x in o.values_mut() {
                cut |= tidy(x, public);
            }
        }
        _ => {}
    }
    cut
}

/// Defence in depth: the service already filters, an R-18 work in a list is dropped here too.
fn is_adult(o: &Map<String, Value>) -> bool {
    ["xRestrict", "x_restrict", "xrestrict"]
        .iter()
        .any(|k| o.get(*k).and_then(Value::as_u64).is_some_and(|n| n > 0))
        || ["adult", "isAdult", "r18"]
            .iter()
            .any(|k| o.get(*k).and_then(Value::as_bool) == Some(true))
}

fn drop_adult(v: &mut Value) -> usize {
    let mut dropped = 0;
    match v {
        Value::Array(a) => {
            let before = a.len();
            a.retain(|x| !x.as_object().is_some_and(is_adult));
            dropped += before - a.len();
            for x in a {
                dropped += drop_adult(x);
            }
        }
        Value::Object(o) => {
            for x in o.values_mut() {
                dropped += drop_adult(x);
            }
        }
        _ => {}
    }
    dropped
}

/// Instagram media items anywhere in a result: objects with a signed `url` and a `kind`.
fn collect_items(v: &Value, user: &str, out: &mut Vec<Remembered>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| collect_items(x, user, out)),
        Value::Object(o) => {
            if let (Some(url), Some(kind)) = (
                o.get("url").and_then(Value::as_str),
                o.get("kind").and_then(Value::as_str),
            ) {
                if url.starts_with(INSTAGRAM_PREFIX)
                    && matches!(kind, "image" | "video")
                    && out.len() < MAX_REMEMBERED
                {
                    out.push(Remembered {
                        url: url.to_owned(),
                        kind: kind.to_owned(),
                        username: user.to_owned(),
                        taken_at: o.get("takenAt").filter(|t| !t.is_null()).cloned(),
                        caption: o
                            .get("caption")
                            .and_then(Value::as_str)
                            .map(|c| c.chars().take(500).collect()),
                    });
                }
            }
            o.values().for_each(|x| collect_items(x, user, out));
        }
        _ => {}
    }
}

fn private(ctx: &Ctx, browse_path: Option<(&str, usize)>) {
    ctx.env.plugins.touch(&ctx.session_id);
    ctx.push_effect(Effect::PrivateTurn);
    if let Some((path, count)) = browse_path {
        if count > 0 {
            ctx.push_effect(Effect::Gallery {
                path: path.to_owned(),
                count,
            });
        }
    }
}

// ---------------------------------------------------------------- instagram

#[derive(Deserialize, JsonSchema)]
struct UserArgs {
    /// The Instagram username, with or without the leading @.
    username: String,
}

pub struct InstagramProfileTool;

#[async_trait::async_trait]
impl Tool for InstagramProfileTool {
    fn name(&self) -> &'static str {
        "instagram_profile"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's Instagram plugin (owner only): a public profile with its bio, counts, the first page of \
         posts and the highlights. Media comes as signed `url` / `thumbUrl` links you can give him. Reads only; \
         an account that is private or missing answers not found."
    }
    fn parameters(&self) -> Value {
        schema_for::<UserArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: UserArgs = super::args(args)?;
        let user = username(&a.username)?;
        let p = &ctx.env.plugins;
        let mut out = send(
            ctx,
            &p.instagram,
            reqwest::Method::GET,
            &format!("/v1/instagram/profile/{user}"),
            &[],
            None,
        )
        .await?;
        finish_instagram(ctx, &user, &mut out, "/instagram");
        Ok(out)
    }
}

pub struct InstagramStoriesTool;

#[async_trait::async_trait]
impl Tool for InstagramStoriesTool {
    fn name(&self) -> &'static str {
        "instagram_stories"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's Instagram plugin (owner only): the current stories of a public account (they expire after \
         24 hours; none is a normal answer). Each item has a kind (image or video), a signed `url`, a \
         `thumbUrl` and its time. To keep them, use save_to_gallery."
    }
    fn parameters(&self) -> Value {
        schema_for::<UserArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: UserArgs = super::args(args)?;
        let user = username(&a.username)?;
        let p = &ctx.env.plugins;
        let mut out = send(
            ctx,
            &p.instagram,
            reqwest::Method::GET,
            &format!("/v1/instagram/stories/{user}"),
            &[],
            None,
        )
        .await?;
        finish_instagram(ctx, &user, &mut out, "/instagram");
        Ok(out)
    }
}

fn finish_instagram(ctx: &Ctx, user: &str, out: &mut Value, page: &str) {
    let p = &ctx.env.plugins;
    let mut items = Vec::new();
    collect_items(out, user, &mut items);
    let n = items.len();
    p.remember(&ctx.session_id, items);
    let cut = tidy(out, &p.public_url);
    if cut {
        if let Some(o) = out.as_object_mut() {
            o.insert("truncated".into(), json!(true));
        }
    }
    private(ctx, Some((page, n)));
}

// ---------------------------------------------------------------- watchlist

pub struct InstagramWatchlistTool;

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

#[async_trait::async_trait]
impl Tool for InstagramWatchlistTool {
    fn name(&self) -> &'static str {
        "instagram_watchlist"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's Instagram watchlist (owner only): the accounts he follows in the plugin, each with whether \
         new items are saved automatically (autoSave), when it was last checked, how many new stories and posts \
         there are, and whether the account is private. For what exactly is new on one account, use instagram_new."
    }
    fn parameters(&self) -> Value {
        schema_for::<NoArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let _: NoArgs = super::args(args)?;
        let mut out = send(
            ctx,
            &ctx.env.plugins.instagram,
            reqwest::Method::GET,
            "/v1/instagram/watch",
            &[],
            None,
        )
        .await?;
        let cut = tidy(&mut out, &ctx.env.plugins.public_url);
        let mut out = if out.is_array() {
            json!({"watching": out})
        } else {
            out
        };
        if cut {
            out["truncated"] = json!(true);
        }
        private(ctx, Some(("/instagram", 0)));
        Ok(out)
    }
}

#[derive(Deserialize, JsonSchema)]
struct WatchArgs {
    /// The Instagram username, with or without the leading @.
    username: String,
    /// true: save the account's new posts and stories to the Gallery automatically. Omit to keep the current setting.
    #[serde(rename = "autoSave")]
    #[schemars(rename = "autoSave")]
    auto_save: Option<bool>,
}

pub struct InstagramWatchTool;

#[async_trait::async_trait]
impl Tool for InstagramWatchTool {
    fn name(&self) -> &'static str {
        "instagram_watch"
    }
    fn description(&self) -> &'static str {
        "Put an Instagram account on Giuseppe's watchlist (owner only; it changes only his plugin's list, \
         nothing on Instagram). autoSave=true also keeps its new posts and stories in the Gallery; only set it \
         when he asks for that."
    }
    fn parameters(&self) -> Value {
        schema_for::<WatchArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: WatchArgs = super::args(args)?;
        let user = username(&a.username)?;
        let body = match a.auto_save {
            Some(v) => json!({"autoSave": v}),
            None => json!({}),
        };
        let mut out = send(
            ctx,
            &ctx.env.plugins.instagram,
            reqwest::Method::PUT,
            &format!("/v1/instagram/watch/{user}"),
            &[],
            Some(body),
        )
        .await?;
        tidy(&mut out, &ctx.env.plugins.public_url);
        private(ctx, None);
        Ok(json!({"watching": true, "result": out}))
    }
}

pub struct InstagramUnwatchTool;

#[async_trait::async_trait]
impl Tool for InstagramUnwatchTool {
    fn name(&self) -> &'static str {
        "instagram_unwatch"
    }
    fn description(&self) -> &'static str {
        "Take an Instagram account off Giuseppe's watchlist (owner only; only his plugin's list changes). \
         Items already saved to the Gallery stay."
    }
    fn parameters(&self) -> Value {
        schema_for::<UserArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: UserArgs = super::args(args)?;
        let user = username(&a.username)?;
        send(
            ctx,
            &ctx.env.plugins.instagram,
            reqwest::Method::DELETE,
            &format!("/v1/instagram/watch/{user}"),
            &[],
            None,
        )
        .await?;
        private(ctx, None);
        Ok(json!({"watching": false}))
    }
}

pub struct InstagramNewTool;

#[async_trait::async_trait]
impl Tool for InstagramNewTool {
    fn name(&self) -> &'static str {
        "instagram_new"
    }
    fn description(&self) -> &'static str {
        "What is new on a watched Instagram account since Giuseppe last opened it (owner only): `stories` and \
         `posts`, each a media item with a signed `url` / `thumbUrl`. Empty lists mean nothing new. To keep \
         them, use save_to_gallery (all=true) or instagram_save_latest."
    }
    fn parameters(&self) -> Value {
        schema_for::<UserArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: UserArgs = super::args(args)?;
        let user = username(&a.username)?;
        let mut out = send(
            ctx,
            &ctx.env.plugins.instagram,
            reqwest::Method::GET,
            &format!("/v1/instagram/watch/{user}/new"),
            &[],
            None,
        )
        .await?;
        finish_instagram(ctx, &user, &mut out, "/instagram");
        Ok(out)
    }
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LatestKind {
    Post,
    Story,
}

#[derive(Deserialize, JsonSchema)]
struct LatestArgs {
    /// The Instagram username, with or without the leading @.
    username: String,
    /// post (default) or story.
    kind: Option<LatestKind>,
}

/// The newest of `items` (by `takenAt` when all have one, else the first).
fn newest(mut items: Vec<Remembered>) -> Option<Remembered> {
    if items
        .iter()
        .all(|i| i.taken_at.as_ref().is_some_and(Value::is_string))
    {
        items.sort_by(|a, b| {
            b.taken_at
                .as_ref()
                .and_then(Value::as_str)
                .cmp(&a.taken_at.as_ref().and_then(Value::as_str))
        });
    }
    items.into_iter().next()
}

pub struct InstagramSaveLatestTool;

#[async_trait::async_trait]
impl Tool for InstagramSaveLatestTool {
    fn name(&self) -> &'static str {
        "instagram_save_latest"
    }
    fn description(&self) -> &'static str {
        "Save an account's newest post (default) or newest story to Giuseppe's Gallery in one step (owner only; \
         \"save X's latest post\"). Tell him what was saved or that it was already there."
    }
    fn parameters(&self) -> Value {
        schema_for::<LatestArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: LatestArgs = super::args(args)?;
        let user = username(&a.username)?;
        let story = a.kind == Some(LatestKind::Story);
        let p = &ctx.env.plugins;
        let path = if story {
            format!("/v1/instagram/stories/{user}")
        } else {
            format!("/v1/instagram/profile/{user}")
        };
        let got = send(ctx, &p.instagram, reqwest::Method::GET, &path, &[], None).await?;
        // A profile also lists highlights: only its posts count.
        let scope = if story {
            &got
        } else {
            got.get("posts").unwrap_or(&got)
        };
        let mut items = Vec::new();
        collect_items(scope, &user, &mut items);
        let item = newest(items).ok_or_else(|| {
            ToolError::new(if story {
                "that account has no stories right now"
            } else {
                "no posts found on that account"
            })
        })?;
        let out = send(
            ctx,
            &p.instagram,
            reqwest::Method::POST,
            "/v1/instagram/save",
            &[],
            Some(json!({"items": [item_body(&item)]})),
        )
        .await?;
        let saved = out["saved"].as_array().map_or(0, Vec::len);
        let dup = out["saved"]
            .as_array()
            .map_or(0, |a| a.iter().filter(|s| s["duplicate"] == true).count());
        let failed = out["failed"].as_array().map_or(0, Vec::len);
        private(ctx, Some(("/gallery", saved.saturating_sub(dup))));
        Ok(json!({
            "kind": item.kind,
            "saved": saved - dup,
            "alreadyThere": dup,
            "failed": failed,
            "takenAt": item.taken_at,
            "details": out,
        }))
    }
}

// ---------------------------------------------------------------- pixiv

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum WorkType {
    All,
    Illust,
    Manga,
    Ugoira,
}

impl WorkType {
    fn as_str(self) -> &'static str {
        match self {
            WorkType::All => "all",
            WorkType::Illust => "illust",
            WorkType::Manga => "manga",
            WorkType::Ugoira => "ugoira",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    /// What to look for: a character, an artist's name, a tag (Japanese tags find the most).
    query: String,
    /// all (default), illust, manga or ugoira.
    r#type: Option<WorkType>,
}

pub struct PixivSearchTool;

#[async_trait::async_trait]
impl Tool for PixivSearchTool {
    fn name(&self) -> &'static str {
        "pixiv_search"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's Pixiv plugin (owner only): search artwork by words or tag, newest first. Safe works only; \
         you cannot search adult content and must not offer to. Each work has an id, title, artist, tags and \
         signed `thumbUrl` / `url` links; save one with save_to_gallery (its id)."
    }
    fn parameters(&self) -> Value {
        schema_for::<SearchArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: SearchArgs = super::args(args)?;
        let q = a.query.trim();
        if q.is_empty() || q.chars().count() > 200 {
            return Err(ToolError::new("the search words must be 1-200 characters"));
        }
        let mut out = send(
            ctx,
            &ctx.env.plugins.pixiv,
            reqwest::Method::GET,
            "/v1/pixiv/search",
            &[
                ("q", q.to_owned()),
                ("mode", "safe".into()),
                ("order", "date_d".into()),
                ("type", a.r#type.unwrap_or(WorkType::All).as_str().into()),
            ],
            None,
        )
        .await?;
        finish_pixiv(ctx, &mut out);
        Ok(out)
    }
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum RankingMode {
    Day,
    Week,
    Month,
    DayMale,
    DayFemale,
    Original,
    Rookie,
}

impl RankingMode {
    fn as_str(self) -> &'static str {
        match self {
            RankingMode::Day => "day",
            RankingMode::Week => "week",
            RankingMode::Month => "month",
            RankingMode::DayMale => "day_male",
            RankingMode::DayFemale => "day_female",
            RankingMode::Original => "original",
            RankingMode::Rookie => "rookie",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct RankingArgs {
    /// day (default), week, month, day_male, day_female, original or rookie.
    mode: Option<RankingMode>,
}

pub struct PixivRankingTool;

#[async_trait::async_trait]
impl Tool for PixivRankingTool {
    fn name(&self) -> &'static str {
        "pixiv_ranking"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's Pixiv plugin (owner only): today's (or this week's, month's) Pixiv ranking of safe \
         artwork. No adult rankings exist here. Works come with id, title, artist and signed links; save \
         one with save_to_gallery (its id)."
    }
    fn parameters(&self) -> Value {
        schema_for::<RankingArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    fn summarize(&self, _: &Value) -> String {
        self.name().into()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: RankingArgs = super::args(args)?;
        let mode = a.mode.unwrap_or(RankingMode::Day).as_str();
        let mut out = send(
            ctx,
            &ctx.env.plugins.pixiv,
            reqwest::Method::GET,
            "/v1/pixiv/ranking",
            &[("mode", mode.into())],
            None,
        )
        .await?;
        finish_pixiv(ctx, &mut out);
        Ok(out)
    }
}

fn finish_pixiv(ctx: &Ctx, out: &mut Value) {
    let dropped = drop_adult(out);
    let cut = tidy(out, &ctx.env.plugins.public_url);
    if let Some(o) = out.as_object_mut() {
        if cut {
            o.insert("truncated".into(), json!(true));
        }
        if dropped > 0 {
            o.insert("adultDropped".into(), json!(dropped));
        }
    }
    let n = ["items", "works", "illusts"]
        .iter()
        .find_map(|k| out.get(*k).and_then(Value::as_array).map(Vec::len))
        .unwrap_or(0);
    private(ctx, Some(("/pixiv", n)));
}

// ---------------------------------------------------------------- save

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Source {
    Instagram,
    Pixiv,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Image,
    Video,
}

#[derive(Deserialize, JsonSchema)]
struct SaveArgs {
    /// instagram or pixiv.
    source: Source,
    /// instagram: the `url` of one item exactly as an earlier instagram_profile / instagram_stories result gave it.
    url: Option<String>,
    /// instagram: several item urls.
    urls: Option<Vec<String>>,
    /// instagram: true saves every item of the latest Instagram result in this chat.
    all: Option<bool>,
    /// instagram: image or video; only when the url is not from a result in this chat.
    kind: Option<Kind>,
    /// instagram: the account the item belongs to; only when the url is not from a result in this chat.
    username: Option<String>,
    /// pixiv: the work's id (digits) from pixiv_search / pixiv_ranking.
    id: Option<String>,
    /// pixiv: which pages of a multi-page work (0 is the first); default all pages.
    pages: Option<Vec<u32>>,
}

/// The signed path of an item url (relative, or absolute under the public prefix).
fn relative(url: &str, public: &str) -> Option<String> {
    let u = url.trim();
    let u = u.strip_prefix(public).unwrap_or(u);
    u.starts_with(INSTAGRAM_PREFIX).then(|| u.to_owned())
}

pub struct SaveToGalleryTool;

#[async_trait::async_trait]
impl Tool for SaveToGalleryTool {
    fn name(&self) -> &'static str {
        "save_to_gallery"
    }
    fn description(&self) -> &'static str {
        "Save to Giuseppe's own Gallery (owner only; it only adds to his library): Instagram items from the \
         latest instagram_profile / instagram_stories result (pass `url`, `urls`, or all=true for everything \
         just shown), or a Pixiv work by `id` (optionally `pages`). Duplicates are skipped. Tell him how \
         many were saved and how many already were there."
    }
    fn parameters(&self) -> Value {
        schema_for::<SaveArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!("save_to_gallery {}", args["source"].as_str().unwrap_or(""))
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: SaveArgs = super::args(args)?;
        let p = &ctx.env.plugins;
        let out = match a.source {
            Source::Pixiv => {
                let id: u64 =
                    a.id.as_deref()
                        .map(str::trim)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| {
                            ToolError::new(
                                "a Pixiv work needs its id (digits) from a search or ranking",
                            )
                        })?;
                let mut body = json!({"id": id});
                if let Some(pages) = a.pages.filter(|p| !p.is_empty()) {
                    body["pages"] = json!(pages);
                }
                send(
                    ctx,
                    &p.pixiv,
                    reqwest::Method::POST,
                    "/v1/pixiv/save",
                    &[],
                    Some(body),
                )
                .await?
            }
            Source::Instagram => {
                let known = p.remembered(&ctx.session_id);
                let mut wanted: Vec<String> = a.urls.unwrap_or_default();
                wanted.extend(a.url);
                let mut items: Vec<Value> = Vec::new();
                if a.all == Some(true) {
                    for r in known.iter().take(MAX_SAVE) {
                        items.push(item_body(r));
                    }
                }
                for raw in wanted {
                    let rel = relative(&raw, &p.public_url).ok_or_else(|| {
                        ToolError::new(
                            "only an item url from an Instagram result can be saved (it starts with /v1/instagram/s/)",
                        )
                    })?;
                    if items.iter().any(|i| i["url"] == rel.as_str()) {
                        continue;
                    }
                    if let Some(r) = known.iter().find(|r| r.url == rel) {
                        items.push(item_body(r));
                    } else {
                        let kind = a.kind.ok_or_else(|| {
                            ToolError::new("that url is not from a recent result: say its kind (image or video) and username")
                        })?;
                        let user = username(a.username.as_deref().ok_or_else(|| {
                            ToolError::new("that url is not from a recent result: say its username")
                        })?)?;
                        items.push(json!({
                            "url": rel,
                            "kind": match kind { Kind::Image => "image", Kind::Video => "video" },
                            "username": user,
                        }));
                    }
                }
                if items.is_empty() {
                    return Err(ToolError::new(
                        "nothing to save: no Instagram result in this chat yet, or no url given",
                    ));
                }
                items.truncate(MAX_SAVE);
                send(
                    ctx,
                    &p.instagram,
                    reqwest::Method::POST,
                    "/v1/instagram/save",
                    &[],
                    Some(json!({"items": items})),
                )
                .await?
            }
        };
        let saved = out["saved"].as_array().map_or(0, Vec::len);
        let dup = out["saved"]
            .as_array()
            .map_or(0, |a| a.iter().filter(|s| s["duplicate"] == true).count());
        let failed = out["failed"].as_array().map_or(0, Vec::len);
        private(ctx, Some(("/gallery", saved.saturating_sub(dup))));
        Ok(json!({
            "saved": saved - dup,
            "alreadyThere": dup,
            "failed": failed,
            "details": out,
        }))
    }
}

fn item_body(r: &Remembered) -> Value {
    let mut b = json!({"url": r.url, "kind": r.kind, "username": r.username});
    if let Some(t) = &r.taken_at {
        b["takenAt"] = t.clone();
    }
    if let Some(c) = &r.caption {
        b["caption"] = json!(c);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        assert_eq!(username("@some.user_1").unwrap(), "some.user_1");
        for bad in ["", "a b", "a/b", "../x", "x?y=1", &"a".repeat(31), "naïve"] {
            assert!(username(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn tidy_prefixes_signed_paths_and_cuts_lists() {
        let mut v = json!({
            "posts": (0..40).map(|i| json!({"url": format!("/v1/instagram/s/{i}"), "caption": "x".repeat(400)})).collect::<Vec<_>>(),
            "avatarUrl": "https://other.example/a.jpg",
        });
        assert!(tidy(&mut v, "https://getsecretary.tech"));
        assert_eq!(v["posts"].as_array().unwrap().len(), MAX_ITEMS);
        assert_eq!(
            v["posts"][0]["url"],
            "https://getsecretary.tech/v1/instagram/s/0"
        );
        assert_eq!(
            v["posts"][0]["caption"].as_str().unwrap().chars().count(),
            CAPTION_CHARS
        );
        assert_eq!(v["avatarUrl"], "https://other.example/a.jpg");
    }

    #[test]
    fn adult_works_are_dropped() {
        let mut v = json!({"items": [
            {"id": "1", "xRestrict": 0}, {"id": "2", "xRestrict": 1}, {"id": "3", "adult": true},
            {"id": "4", "x_restrict": 2}, {"id": "5"}
        ]});
        assert_eq!(drop_adult(&mut v), 3);
        let ids: Vec<&str> = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["1", "5"]);
    }

    #[test]
    fn relative_accepts_only_instagram_signed_paths() {
        let p = "https://getsecretary.tech";
        assert_eq!(
            relative("/v1/instagram/s/a/b", p).unwrap(),
            "/v1/instagram/s/a/b"
        );
        assert_eq!(
            relative("https://getsecretary.tech/v1/instagram/s/a/b", p).unwrap(),
            "/v1/instagram/s/a/b"
        );
        assert!(relative("https://evil.example/v1/instagram/s/a", p).is_none());
        assert!(relative("/v1/pixiv/s/a", p).is_none());
        assert!(relative("http://169.254.169.254/", p).is_none());
    }
}
