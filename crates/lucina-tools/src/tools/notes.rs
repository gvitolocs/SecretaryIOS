//! `notes`: the owner's notes, briefings and documents.
//!
//! With secretary-server configured (`LUCINA_SECRETARY_SERVER_URL` and
//! `LUCINA_SECRETARY_SERVICE_TOKEN_FILE`) they live there (`/v1/notes`,
//! SecretaryIOS `docs/server-api.md`, "Notes"), so the owner sees them in the
//! app and ChatGPT, Grok or Claude Code (Lucina MCP) share them. When the
//! server is not configured or not reachable, markdown files under
//! `<data dir>/notes/` (atomic 0600 writes) are the fallback; a server that
//! answers but refuses (4xx) is reported, not bypassed.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use super::calendar::server_of;
use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolError, ToolResult};

const MAX_NOTE_CHARS: usize = 20_000;
/// Characters of a note's content a `read` returns.
const READ_CHARS: usize = 12_000;
/// Content limit on the server (UTF-8 bytes).
const SERVER_MAX_BYTES: usize = 256 * 1024;

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Read,
    Create,
    Append,
    Search,
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// Note title (read, create, append).
    title: Option<String>,
    /// Short name of the note, e.g. "craftit-meeting" (read, append; create: optional).
    slug: Option<String>,
    /// Markdown text (create, append).
    content: Option<String>,
    /// Text to look for (search).
    query: Option<String>,
    /// Tags for a new note (create), e.g. ["work"].
    tags: Option<Vec<String>>,
}

/// Lowercase ASCII slug, `-` separated, at most 80 characters.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().to_lowercase().chars() {
        let c = match c {
            'à' | 'á' | 'â' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'æ' => 'a',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let s: String = out.trim_matches('-').chars().take(80).collect();
    s.trim_end_matches('-').to_owned()
}

// ---------------------------------------------------------------- server

enum Remote {
    /// The server answered.
    Done(Value),
    /// Not configured or not reachable: use the local files.
    Fallback(String),
}

/// `(status, body)` from the server, or why it could not be asked.
async fn send(req: reqwest::RequestBuilder) -> Result<(u16, Value), String> {
    let resp = req
        .send()
        .await
        .map_err(|e| format!("secretary-server unreachable: {}", e.without_url()))?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

fn server_err(status: u16, body: &Value) -> ToolError {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let message = body["error"]["message"].as_str().unwrap_or_default();
    ToolError::new(format!(
        "secretary-server answered {status} {code}: {message}"
    ))
}

/// Key for `/v1/notes/{key}`: the slug, else one made from the title.
fn key_of(a: &Args) -> Option<String> {
    let s = a
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .or_else(|| a.title.as_deref().map(slug).filter(|s| !s.is_empty()))?;
    let ok = s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    ok.then_some(s)
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn remote(
    ctx: &Ctx,
    a: &Args,
    req_title: Option<&str>,
    content: Option<&str>,
) -> Result<Remote, ToolError> {
    let Some((base, token)) = server_of(&ctx.env) else {
        return Ok(Remote::Fallback("not configured".into()));
    };
    let http = &ctx.env.http;
    let get = |path: String| http.get(format!("{base}{path}")).bearer_auth(&token);
    let post = |path: String, body: Value| {
        http.post(format!("{base}{path}"))
            .bearer_auth(&token)
            .json(&body)
    };
    // Unreachable → local files; any HTTP answer is final.
    macro_rules! ask {
        ($req:expr) => {
            match send($req).await {
                Ok(r) => r,
                Err(why) => return Ok(Remote::Fallback(why)),
            }
        };
    }
    let out = match a.action {
        Action::List => {
            let (s, v) = ask!(get("/v1/notes?limit=100".into()));
            if s != 200 {
                return Err(server_err(s, &v));
            }
            let notes: Vec<Value> = v["notes"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|n| {
                            json!({"title": n["title"], "slug": n["slug"], "modified": n["updatedAt"],
                                   "source": n["source"], "pinned": n["pinned"], "tags": n["tags"]})
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({"notes": notes, "stored": "secretary"})
        }
        Action::Search => {
            let q = a.query.as_deref().unwrap_or_default().trim().to_owned();
            let (s, v) = ask!(get(format!("/v1/notes?limit=20&q={}", enc(&q))));
            if s != 200 {
                return Err(server_err(s, &v));
            }
            let results: Vec<Value> = v["notes"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|n| json!({"title": n["title"], "slug": n["slug"], "lines": [n["excerpt"]]}))
                        .collect()
                })
                .unwrap_or_default();
            json!({"query": q.to_lowercase(), "results": results, "stored": "secretary"})
        }
        Action::Read => {
            let key = key_of(a).ok_or_else(|| ToolError::new("title or slug is required"))?;
            let (mut s, mut v) = ask!(get(format!("/v1/notes/{}", enc(&key))));
            if s == 404 {
                // A title that is not the slug: the best search hit.
                let q = req_title.unwrap_or(&key);
                let (s2, hits) = ask!(get(format!("/v1/notes?limit=5&q={}", enc(q))));
                let first = (s2 == 200)
                    .then(|| hits["notes"].as_array().and_then(|l| l.first().cloned()))
                    .flatten();
                if let Some(h) = first.and_then(|h| h["slug"].as_str().map(str::to_owned)) {
                    (s, v) = ask!(get(format!("/v1/notes/{}", enc(&h))));
                }
            }
            if s == 404 {
                return Err(ToolError::new(format!(
                    "no note titled {:?}",
                    req_title.unwrap_or(&key)
                )));
            }
            if s != 200 {
                return Err(server_err(s, &v));
            }
            let text = v["content"].as_str().unwrap_or_default();
            let total = text.chars().count();
            let mut out = json!({"title": v["title"], "slug": v["slug"], "source": v["source"],
                "updated": v["updatedAt"], "tags": v["tags"],
                "content": text.chars().take(READ_CHARS).collect::<String>(), "stored": "secretary"});
            if total > READ_CHARS {
                out["truncated"] = json!(format!(
                    "{total} characters in total; the whole note is in the Secretary app"
                ));
            }
            out
        }
        Action::Create | Action::Append => {
            let content = content.unwrap_or_default();
            if content.len() > SERVER_MAX_BYTES {
                return Err(ToolError::new("note too long"));
            }
            let title = req_title.unwrap_or_default().to_owned();
            let explicit = a
                .slug
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_lowercase);
            let create = |slug: Option<String>| {
                let mut body = json!({"title": title, "content": content, "source": "lucina"});
                if let Some(s) = slug {
                    body["slug"] = json!(s);
                }
                if let Some(t) = &a.tags {
                    body["tags"] = json!(t);
                }
                post("/v1/notes".into(), body)
            };
            let (s, v) = if a.action == Action::Create {
                let (s, v) = ask!(create(explicit.clone()));
                if s == 409 && v["error"]["code"] == "slug_taken" {
                    return Err(ToolError::new(format!(
                        "a note titled {title:?} exists; use append"
                    )));
                }
                (s, v)
            } else {
                let key = key_of(a).ok_or_else(|| ToolError::new("title or slug is required"))?;
                let (s, v) = ask!(post(
                    format!("/v1/notes/{}/append", enc(&key)),
                    json!({"content": content, "separator": "\n", "source": "lucina"})
                ));
                if s == 404 {
                    // Appending to a missing note starts it (as with local files).
                    ask!(create(explicit.clone().or(Some(key))))
                } else {
                    (s, v)
                }
            };
            if !(200..300).contains(&s) {
                if v["error"]["code"] == "note_too_large" {
                    return Err(ToolError::new("note too long"));
                }
                return Err(server_err(s, &v));
            }
            let saved_title = v["title"].as_str().unwrap_or(&title).to_owned();
            ctx.push_effect(Effect::Note {
                title: saved_title.clone(),
            });
            tracing::info!(
                slug = v["slug"].as_str().unwrap_or_default(),
                "note saved on secretary-server"
            );
            json!({"saved": true, "title": saved_title, "slug": v["slug"],
                   "chars": v["content"].as_str().map_or(0, |c| c.chars().count()), "stored": "secretary"})
        }
    };
    Ok(Remote::Done(out))
}

// ---------------------------------------------------------------- local files

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.env.data_dir.join("notes")
}

fn path_for(ctx: &Ctx, title: &str) -> Result<PathBuf, ToolError> {
    let s = slug(title);
    if s.is_empty() {
        return Err(ToolError::new("title is required"));
    }
    Ok(dir(ctx).join(format!("{s}.md")))
}

fn list(ctx: &Ctx) -> Vec<(String, std::time::SystemTime, String)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir(ctx)) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let title = text
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("# "))
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    p.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((title, mtime, text));
        }
    }
    out.sort_by_key(|n| std::cmp::Reverse(n.1));
    out
}

fn local(a: &Args, ctx: &Ctx) -> ToolResult {
    let req = |v: &Option<String>, what: &str| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ToolError::new(format!("{what} is required")))
    };
    let title_or_slug = || req(&a.title, "title").or_else(|_| req(&a.slug, "title"));
    match a.action {
        Action::List => Ok(
            json!({"notes": list(ctx).into_iter().take(100).map(|(t, m, _)| json!({
            "title": t,
            "modified": chrono::DateTime::<chrono::Utc>::from(m).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        })).collect::<Vec<_>>()}),
        ),
        Action::Read => {
            let title = title_or_slug()?;
            let p = path_for(ctx, &title)?;
            let text = std::fs::read_to_string(&p)
                .map_err(|_| ToolError::new(format!("no note titled {title:?}")))?;
            Ok(json!({"title": title, "content": text.chars().take(8000).collect::<String>()}))
        }
        Action::Create | Action::Append => {
            let title = title_or_slug()?;
            let content = req(&a.content, "content")?;
            let p = path_for(ctx, &title)?;
            let existing = std::fs::read_to_string(&p).ok();
            let text = match (a.action, existing) {
                (Action::Create, Some(_)) => {
                    return Err(ToolError::new(format!(
                        "a note titled {title:?} exists; use append"
                    )))
                }
                (Action::Create, None) => format!("# {title}\n\n{content}\n"),
                (_, Some(old)) => format!("{}\n{content}\n", old.trim_end()),
                (_, None) => format!("# {title}\n\n{content}\n"),
            };
            if text.chars().count() > MAX_NOTE_CHARS {
                return Err(ToolError::new("note too long"));
            }
            lucina_store::write_atomic(&p, text.as_bytes())
                .map_err(|e| ToolError::new(format!("write failed: {e}")))?;
            ctx.push_effect(Effect::Note {
                title: title.clone(),
            });
            Ok(json!({"saved": true, "title": title, "chars": text.chars().count()}))
        }
        Action::Search => {
            let q = req(&a.query, "query")?.to_lowercase();
            let hits: Vec<Value> = list(ctx)
                .into_iter()
                .filter_map(|(t, _, text)| {
                    let lines: Vec<String> = text
                        .lines()
                        .filter(|l| l.to_lowercase().contains(&q))
                        .take(5)
                        .map(str::to_owned)
                        .collect();
                    (t.to_lowercase().contains(&q) || !lines.is_empty())
                        .then(|| json!({"title": t, "lines": lines}))
                })
                .take(20)
                .collect();
            Ok(json!({"query": q, "results": hits}))
        }
    }
}

pub struct NotesTool;

#[async_trait::async_trait]
impl Tool for NotesTool {
    fn name(&self) -> &'static str {
        "notes"
    }
    fn description(&self) -> &'static str {
        "The owner's notes, briefings and documents, shared with the Secretary app (Notes) and with \
         ChatGPT, Grok and Claude Code: list, read (by title or slug, e.g. \"craftit-meeting\"), create, \
         append or search. Read a note whenever Giuseppe names one or asks about a briefing."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "notes {} {}",
            args["action"].as_str().unwrap_or(""),
            args["title"]
                .as_str()
                .or(args["slug"].as_str())
                .or(args["query"].as_str())
                .unwrap_or("")
        )
        .trim()
        .to_owned()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let trimmed = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let title = trimmed(&a.title);
        let content = trimmed(&a.content);
        match a.action {
            Action::Read | Action::Append if title.is_none() && trimmed(&a.slug).is_none() => {
                return Err(ToolError::new("title is required"))
            }
            Action::Create if title.is_none() => return Err(ToolError::new("title is required")),
            Action::Create | Action::Append if content.is_none() => {
                return Err(ToolError::new("content is required"))
            }
            Action::Search if trimmed(&a.query).is_none() => {
                return Err(ToolError::new("query is required"))
            }
            _ => {}
        }
        let req_title = title.clone().or_else(|| trimmed(&a.slug));
        match remote(ctx, &a, req_title.as_deref(), content.as_deref()).await? {
            Remote::Done(v) => Ok(v),
            Remote::Fallback(why) => {
                if why != "not configured" {
                    tracing::warn!(reason = %why, "notes: secretary-server unavailable, using local files");
                }
                local(&a, ctx)
            }
        }
    }
}
