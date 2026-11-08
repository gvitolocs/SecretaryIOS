//! `notebook_*`: Lucina Notebooks, the owner's NotebookLM-style workspace (secretary-server
//! `/v1/notebooks/*` → Lucina's `/api/secretary/notebooks/*`; the contract is
//! `docs/design/notebooks-api.md`).
//!
//! A notebook groups sources (PDFs, videos, web pages, audio, text, files), notes (the
//! owner's Keep notes, linked) and chats that answer only from those sources and cite
//! them as `[source:<source id>-<n>]`. All eight tools are scope `notebooks`, which only
//! the owner grants (consent page or `secretary-mcp token|oauth add-scope`): the sources
//! are his documents. The MCP names its client in `X-Secretary-Caller` (`mcp:claude`),
//! which Lucina keeps in her log. Nothing here deletes anything.
//!
//! Ids are checked here (`nb`, `s`, `c`, `p` + hex digits), so a citation id such as
//! `s0a9be3c112-14` passed as a source id gets a precise error instead of a 404.

use reqwest::Method;
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::backend::{seg, CHAT_TIMEOUT};
use crate::computer::caller_headers;
use crate::format;
use crate::tools::{ToolError, ToolResult, Tools};

/// Characters of a pasted text source (Lucina's own limit applies too).
const MAX_TEXT: usize = 1_000_000;
const MAX_QUESTION: usize = 12_000;
const MAX_BRIEFING: usize = 4_000;

/// Which notebooks `notebook_list` shows.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Archived {
    Exclude,
    Include,
    Only,
}

impl Archived {
    fn as_str(self) -> &'static str {
        match self {
            Archived::Exclude => "exclude",
            Archived::Include => "include",
            Archived::Only => "only",
        }
    }
}

/// The model that answers.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Model {
    /// DeepSeek with Lucina's own key (default).
    Deepseek,
    /// The local Qwen 3.8 27B on nezopt: slower, never leaves the machine.
    Qwen,
}

impl Model {
    fn as_str(self) -> &'static str {
        match self {
            Model::Deepseek => "deepseek",
            Model::Qwen => "qwen",
        }
    }
}

/// Arguments of `notebook_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// Archived notebooks: exclude (default), include or only.
    #[serde(default)]
    pub archived: Option<Archived>,
}

/// Arguments of `notebook_get`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    /// The notebook id (`nb` + hex digits) that notebook_list or notebook_create returned.
    pub notebook_id: String,
}

/// Arguments of `notebook_create`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    /// The notebook's name, up to 200 characters, e.g. "CNS".
    pub name: String,
    /// What it is for, up to 2000 characters.
    #[serde(default)]
    pub description: Option<String>,
}

/// Arguments of `notebook_add_source`: exactly one of `url`, `text`, `sandbox_path`, `note_slug`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddSourceArgs {
    /// The notebook id (`nb` + hex digits).
    pub notebook_id: String,
    /// A web page, YouTube video, PDF or audio file by its http(s) address.
    #[serde(default)]
    pub url: Option<String>,
    /// Text to add as a source (up to 1,000,000 characters), with `title`.
    #[serde(default)]
    pub text: Option<String>,
    /// The title of a `text` source (default: its first line).
    #[serde(default)]
    pub title: Option<String>,
    /// A file on Lucina's own Linux desktop sandbox, e.g. /home/cua/Downloads/paper.pdf (see computer_bash).
    #[serde(default)]
    pub sandbox_path: Option<String>,
    /// A Keep note by its slug (note_get shows it): linked, not copied.
    #[serde(default)]
    pub note_slug: Option<String>,
    /// Transformation ids to run on the new source besides the defaults (notebook_transform lists them).
    #[serde(default)]
    pub transformations: Option<Vec<String>>,
}

/// Arguments of `notebook_ask`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskArgs {
    /// The notebook id (`nb` + hex digits).
    pub notebook_id: String,
    /// The question, up to 12000 characters. It is answered only from the notebook's sources.
    pub question: String,
    /// Continue this chat (`c` + hex digits, from an earlier notebook_ask or notebook_get). Without it a new chat starts.
    #[serde(default)]
    pub chat_id: Option<String>,
    /// deepseek (default) or qwen (local, slower).
    #[serde(default)]
    pub model: Option<Model>,
}

/// Arguments of `notebook_search`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    /// Words or a question to look for in the sources and linked notes (up to 500 characters).
    pub query: String,
    /// Only this notebook (`nb` + hex digits). Without it every notebook is searched.
    #[serde(default)]
    pub notebook_id: Option<String>,
    /// How many hits, 1-50 (default: Lucina's).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments of `notebook_transform`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransformArgs {
    /// The source id (`s` + hex digits, from notebook_get; not a citation id with a -<n> suffix).
    pub source_id: String,
    /// The transformation to run (`t-` + a name, e.g. t-dense-summary). Without it nothing is started: the available transformations and this source's insights are listed.
    #[serde(default)]
    pub transformation_id: Option<String>,
}

/// Arguments of `notebook_podcast`: `notebook_id` starts one, `podcast_id` reads one.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PodcastArgs {
    /// Start a podcast from this notebook's sources (`nb` + hex digits).
    #[serde(default)]
    pub notebook_id: Option<String>,
    /// Get the status of this podcast instead (`p` + hex digits, from the start answer or notebook_get).
    #[serde(default)]
    pub podcast_id: Option<String>,
    /// What the episode should focus on, up to 4000 characters.
    #[serde(default)]
    pub briefing: Option<String>,
    /// Episode profile (default: Lucina's two-hosts).
    #[serde(default)]
    pub episode_profile: Option<String>,
    /// Speaker profile (default: Lucina's).
    #[serde(default)]
    pub speaker_profile: Option<String>,
    /// Target length in minutes, 2-60.
    #[serde(default)]
    pub minutes: Option<u32>,
}

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| bad(format!("Invalid arguments: {e}")))
}

/// `<prefix>` + 6-32 lowercase hex digits.
fn hex_id(field: &str, s: &str, prefix: &str, example: &str, from: &str) -> Result<(), ToolError> {
    let ok = s.strip_prefix(prefix).is_some_and(|h| {
        (6..=32).contains(&h.len())
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "`{field}` is `{prefix}` + hex digits, e.g. {example}, as {from} returned it"
        )))
    }
}

fn notebook_id(s: &str) -> Result<(), ToolError> {
    hex_id("notebook_id", s, "nb", "nb4f1c09a2e7", "notebook_list")
}

/// `t-` + lowercase letters, digits and '-'.
fn transformation_id(s: &str) -> Result<(), ToolError> {
    let ok = s.strip_prefix("t-").is_some_and(|n| {
        (1..=64).contains(&n.len())
            && n.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    if ok {
        Ok(())
    } else {
        Err(bad(
            "`transformation_id` is `t-` + a name, e.g. t-dense-summary (call notebook_transform without it to list them)",
        ))
    }
}

/// A slug (`[a-z0-9-]`, at most 64).
fn slug_ok(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A non-empty text of at most `max` characters, without control characters other than newlines and tabs.
fn text(field: &str, s: &str, max: usize) -> Result<String, ToolError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(bad(format!("`{field}` must not be empty")));
    }
    if t.chars().count() > max {
        return Err(bad(format!("`{field}` is longer than {max} characters")));
    }
    if t.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
    {
        return Err(bad(format!("`{field}` contains control characters")));
    }
    Ok(t.to_owned())
}

/// The first `max` characters of a question, cut at a word, as the new chat's title.
fn chat_title(q: &str) -> String {
    let one_line = q.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = one_line.chars().take(60).collect();
    if t.len() < one_line.len() {
        format!("{}…", t.trim_end())
    } else {
        t
    }
}

/// A refusal the model can act on.
fn refusal(e: crate::backend::BackendError) -> ToolError {
    match (e.status, e.code.as_str()) {
        (404, "not_found") => bad(format!("Not found: {}", e.message)),
        (_, "unavailable") => bad(
            "Lucina's notebooks are not available right now (the model or the embedder is busy or off). Try again later.",
        ),
        (_, "conflict") => bad(format!("Conflict: {}", e.message)),
        (_, "bad_request") => bad(format!(
            "Lucina's notebooks refused the request: {}",
            e.message
        )),
        _ => e.into(),
    }
}

impl Tools<'_> {
    async fn nb(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, ToolError> {
        self.app
            .backend
            .send_device_query(
                method,
                &format!("/v1/notebooks{path}"),
                query,
                body,
                caller_headers(&self.caller.name),
            )
            .await
            .map_err(refusal)
    }

    /// `notebook_list`, `notebook_get`, `notebook_create`, `notebook_add_source`, `notebook_search`,
    /// `notebook_transform`, `notebook_podcast`. (`notebook_ask` is [`Self::notebook_ask`].)
    pub async fn notebooks(&self, name: &str, args: &JsonObject) -> ToolResult {
        let tz = self.app.cfg.timezone;
        match name {
            "notebook_list" => {
                let a: ListArgs = parse(args)?;
                let q: Vec<(&str, String)> = a
                    .archived
                    .map(|x| ("archived", x.as_str().to_owned()))
                    .into_iter()
                    .collect();
                Ok(format::notebooks(
                    &self.nb(Method::GET, "", &q, None).await?,
                ))
            }
            "notebook_get" => {
                let a: GetArgs = parse(args)?;
                notebook_id(&a.notebook_id)?;
                Ok(format::notebook(
                    &self
                        .nb(Method::GET, &format!("/{}", seg(&a.notebook_id)), &[], None)
                        .await?,
                    tz,
                ))
            }
            "notebook_create" => {
                let a: CreateArgs = parse(args)?;
                let mut body = json!({"name": text("name", &a.name, 200)?});
                if let Some(d) = a
                    .description
                    .as_deref()
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                {
                    body["description"] = json!(text("description", d, 2000)?);
                }
                let v = self.nb(Method::POST, "", &[], Some(&body)).await?;
                Ok(format!(
                    "Created notebook {} ({}). Add sources with notebook_add_source.",
                    v["id"].as_str().unwrap_or("?"),
                    v["name"].as_str().unwrap_or("?")
                ))
            }
            "notebook_add_source" => self.add_source(parse(args)?).await,
            "notebook_search" => {
                let a: SearchArgs = parse(args)?;
                let q = text("query", &a.query, 500)?;
                let mut query = vec![("q", q.clone())];
                if let Some(n) = a.limit {
                    if !(1..=50).contains(&n) {
                        return Err(bad("`limit` must be 1-50"));
                    }
                    query.push(("limit", n.to_string()));
                }
                let path = match a.notebook_id.as_deref() {
                    Some(nb) => {
                        notebook_id(nb)?;
                        format!("/{}/search", seg(nb))
                    }
                    None => "/search".to_owned(),
                };
                Ok(format::notebook_hits(
                    &self.nb(Method::GET, &path, &query, None).await?,
                    &q,
                ))
            }
            "notebook_transform" => self.transform(parse(args)?).await,
            "notebook_podcast" => self.podcast(parse(args)?).await,
            _ => Err(bad(format!("unknown tool {name}"))),
        }
    }

    async fn add_source(&self, a: AddSourceArgs) -> ToolResult {
        notebook_id(&a.notebook_id)?;
        let given = [
            a.url.is_some(),
            a.text.is_some(),
            a.sandbox_path.is_some(),
            a.note_slug.is_some(),
        ];
        if given.iter().filter(|g| **g).count() != 1 {
            return Err(bad(
                "Give exactly one of `url`, `text`, `sandbox_path` or `note_slug`",
            ));
        }
        if a.title.is_some() && a.text.is_none() {
            return Err(bad("`title` goes with `text`"));
        }
        let mut body = if let Some(u) = &a.url {
            let u = u.trim();
            if !(u.starts_with("http://") || u.starts_with("https://"))
                || u.len() > 2048
                || u.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(bad(
                    "`url` must be an http(s) address without spaces, up to 2048 characters",
                ));
            }
            json!({"kind": "url", "url": u})
        } else if let Some(t) = &a.text {
            let mut b = json!({"kind": "text", "text": text("text", t, MAX_TEXT)?});
            if let Some(title) = a.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                b["title"] = json!(text("title", title, 200)?);
            }
            b
        } else if let Some(p) = &a.sandbox_path {
            let p = p.trim();
            if !p.starts_with('/')
                || p.len() > 1024
                || p.split('/').any(|s| s == "..")
                || p.chars().any(char::is_control)
            {
                return Err(bad(
                    "`sandbox_path` is an absolute path on Lucina's desktop sandbox, without `..`",
                ));
            }
            json!({"kind": "sandbox", "path": p})
        } else {
            let slug = a.note_slug.as_deref().unwrap_or("").trim();
            if !slug_ok(slug) {
                return Err(bad(
                    "`note_slug` is a note's slug (lowercase letters, digits and '-'), as notes_search returned it",
                ));
            }
            json!({"kind": "note", "slug": slug})
        };
        if let Some(ts) = &a.transformations {
            if ts.len() > 10 {
                return Err(bad("`transformations` takes at most 10 ids"));
            }
            ts.iter().try_for_each(|t| transformation_id(t))?;
            body["transformations"] = json!(ts);
        }
        let v = self
            .nb(
                Method::POST,
                &format!("/{}/sources", seg(&a.notebook_id)),
                &[],
                Some(&body),
            )
            .await?;
        Ok(format::notebook_source_added(&v))
    }

    /// `notebook_ask`: a chat is made when none is given, the question posted, and the
    /// answer returned with its numbered citations. Takes up to two minutes.
    pub async fn notebook_ask(&self, args: &JsonObject) -> ToolResult {
        let a: AskArgs = parse(args)?;
        notebook_id(&a.notebook_id)?;
        let question = text("question", &a.question, MAX_QUESTION)?;
        if let Some(c) = &a.chat_id {
            hex_id(
                "chat_id",
                c,
                "c",
                "c4f1c09a2e",
                "notebook_ask or notebook_get",
            )?;
        }
        let chat = match &a.chat_id {
            Some(c) => c.clone(),
            None => {
                let body = json!({"title": chat_title(&question)});
                let v = self
                    .nb(
                        Method::POST,
                        &format!("/{}/chats", seg(&a.notebook_id)),
                        &[],
                        Some(&body),
                    )
                    .await?;
                v["id"].as_str().map(str::to_owned).ok_or_else(|| {
                    bad("Lucina's notebooks made a chat but sent no id; nothing was asked.")
                })?
            }
        };
        let mut body = json!({"text": question});
        if let Some(m) = a.model {
            body["model"] = json!(m.as_str());
        }
        let v = self
            .app
            .backend
            .send_device(Method::POST, &format!("/v1/notebooks/chats/{}/messages", seg(&chat)), Some(&body), CHAT_TIMEOUT, caller_headers(&self.caller.name))
            .await
            .map_err(|e| match e.code.as_str() {
                "backend_timeout" => bad(format!(
                    "The answer did not arrive in time. Chat {chat} may still receive it; ask again with chat_id={chat}, or try model=deepseek."
                )),
                _ => refusal(e),
            })?;
        Ok(format::notebook_answer(&chat, &v["reply"]))
    }

    async fn transform(&self, a: TransformArgs) -> ToolResult {
        hex_id(
            "source_id",
            &a.source_id,
            "s",
            "s0a9be3c112",
            "notebook_get",
        )?;
        let src = seg(&a.source_id);
        match a.transformation_id.as_deref() {
            Some(t) => {
                transformation_id(t)?;
                let v = self
                    .nb(
                        Method::POST,
                        &format!("/sources/{src}/insights"),
                        &[],
                        Some(&json!({"transformation_id": t})),
                    )
                    .await?;
                Ok(format::notebook_insight(&v))
            }
            None => {
                let ts = self.nb(Method::GET, "/transformations", &[], None).await?;
                let ins = self
                    .nb(Method::GET, &format!("/sources/{src}/insights"), &[], None)
                    .await?;
                Ok(format!(
                    "{}\n\nInsights on {}:\n{}",
                    format::notebook_transformations(&ts),
                    a.source_id,
                    format::notebook_insights(&ins)
                ))
            }
        }
    }

    async fn podcast(&self, a: PodcastArgs) -> ToolResult {
        match (&a.notebook_id, &a.podcast_id) {
            (None, Some(p)) => {
                hex_id(
                    "podcast_id",
                    p,
                    "p",
                    "p4f1c09a2e7",
                    "notebook_get or the start answer",
                )?;
                Ok(format::notebook_podcast(
                    &self
                        .nb(Method::GET, &format!("/podcasts/{}", seg(p)), &[], None)
                        .await?,
                ))
            }
            (Some(nb), None) => {
                notebook_id(nb)?;
                let mut body = json!({});
                if let Some(b) = a
                    .briefing
                    .as_deref()
                    .map(str::trim)
                    .filter(|b| !b.is_empty())
                {
                    body["briefing"] = json!(text("briefing", b, MAX_BRIEFING)?);
                }
                for (field, v) in [
                    ("episode_profile", &a.episode_profile),
                    ("speaker_profile", &a.speaker_profile),
                ] {
                    if let Some(p) = v.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                        if p.len() > 64
                            || !p
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                        {
                            return Err(bad(format!(
                                "`{field}` is a profile name (letters, digits, '-' and '_')"
                            )));
                        }
                        body[field] = json!(p);
                    }
                }
                if let Some(m) = a.minutes {
                    if !(2..=60).contains(&m) {
                        return Err(bad("`minutes` must be 2-60"));
                    }
                    body["minutes"] = json!(m);
                }
                let v = self
                    .nb(
                        Method::POST,
                        &format!("/{}/podcasts", seg(nb)),
                        &[],
                        Some(&body),
                    )
                    .await?;
                Ok(format!(
                    "Started. {}\nIt takes a few minutes (outline, script, voices, mix).",
                    format::notebook_podcast(&v)
                ))
            }
            _ => Err(bad(
                "Give `notebook_id` to start a podcast, or `podcast_id` to read one's status (not both)",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_checked() {
        assert!(notebook_id("nb4f1c09a2e7").is_ok());
        for bad in [
            "",
            "4f1c09a2e7",
            "nb",
            "nbZZZZZZZZ",
            "nb4F1C09A2E7",
            "s0a9be3c112",
            "nb4f1c09a2e7/x",
            "nb4f1c09a2e7-14",
        ] {
            assert!(notebook_id(bad).is_err(), "{bad}");
        }
        assert!(hex_id("source_id", "s0a9be3c112", "s", "s0a9be3c112", "x").is_ok());
        let e = hex_id(
            "source_id",
            "s0a9be3c112-14",
            "s",
            "s0a9be3c112",
            "notebook_get",
        )
        .unwrap_err();
        assert!(
            e.0.contains("source_id") && e.0.contains("notebook_get"),
            "{}",
            e.0
        );
        assert!(transformation_id("t-dense-summary").is_ok());
        for bad in ["dense-summary", "t-", "t-Dense", "t-a b", "t-../x"] {
            assert!(transformation_id(bad).is_err(), "{bad}");
        }
        assert!(slug_ok("rm-class-2-summary") && !slug_ok("Rm") && !slug_ok("") && !slug_ok("a/b"));
    }

    #[test]
    fn chat_titles_are_short() {
        assert_eq!(chat_title("What is\na ring?"), "What is a ring?");
        let long = "word ".repeat(40);
        let t = chat_title(&long);
        assert!(t.chars().count() <= 61 && t.ends_with('…'), "{t}");
    }

    #[test]
    fn texts_are_checked() {
        assert_eq!(text("q", "  hi \n", 10).unwrap(), "hi");
        assert!(text("q", "   ", 10).is_err());
        assert!(text("q", &"x".repeat(11), 10).is_err());
        assert!(text("q", "a\u{0}b", 10).is_err());
        assert!(text("q", "line\nnext\tcol", 20).is_ok());
    }
}
