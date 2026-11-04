//! `juno_*`: Lucina's own tools on Juno, the owner's laptop (Galaxy Book Go,
//! LucinaOS). The same four as secretary-mcp's `android_*` (`juno_tools`,
//! `juno_call`, `juno_screenshot`, `juno_bash`), with the same argument names.
//!
//! A call travels through [`JunoLink`] (lucina-server's `JunoBridge`): it is
//! queued for the laptop, which long-polls for work through secretary-server
//! and posts the result back. Registered by the server in the owner's
//! registry only (`LUCINA_JUNO_TOOLS`, default on), never on MCP turns.
//!
//! - Which laptop: the turn's session (`secretary_juno-<device>` from Juno's
//!   Chat) names it, see [`device_of_session`]; any other turn gets the one
//!   that polled most recently.
//! - `juno_call` asks the owner first for `delete_file`, `write_file` with
//!   `overwrite`, and `wifi_connect`; reloads, installs, power and disk tools
//!   are refused here and never sent.
//! - Every text is OTP-redacted and clipped to [`MAX_TEXT`]. A screenshot's
//!   PNG never reaches the model: the bridge keeps it and the reply shows it
//!   as an image block ([`Effect::Image`]).

use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolError, ToolResult};

/// Longest text a call returns to the model.
pub const MAX_TEXT: usize = 12_000;
/// Default limit of a call (`juno_tools`, `juno_screenshot`, most driver tools).
const DEFAULT_LIMIT: Duration = Duration::from_secs(30);
/// What the bridge leaves of a call's limit for its own answer.
const MARGIN: Duration = Duration::from_secs(2);
/// `juno_bash` without `timeout_seconds`: the device's own cap.
const BASH_MAX_SECS: u64 = 110;
/// `wait` without `timeout_ms`: the device's own cap.
const WAIT_MAX_MS: u64 = 60_000;

/// The laptop end of the bridge, implemented by lucina-server's `JunoBridge`.
#[async_trait::async_trait]
pub trait JunoLink: Send + Sync {
    /// Run device tool `tool` on `device` (`None`: the laptop that polled most
    /// recently) and wait at most `timeout` for its result object. `Err`: a
    /// text for the model (offline, timed out, the device's own error).
    async fn call(
        &self,
        device: Option<String>,
        tool: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, String>;
    /// The laptops and when each was last seen (`{"devices": [{id, name, connected, lastSeenSecs}]}`).
    fn status(&self) -> Value;
}

/// The Juno device a session belongs to: Juno's Chat sends `juno-<device id>`,
/// which Lucina keeps as `secretary_juno-<device id>`. Anything else: `None`.
pub fn device_of_session(session: &str) -> Option<String> {
    let rest = session.strip_prefix("secretary_").unwrap_or(session);
    let id = rest.strip_prefix("juno-")?;
    (!id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then(|| id.to_owned())
}

/// Driver tools never sent from here: the laptop's boot, power and disks.
fn refused(tool: &str) -> bool {
    tool.split('_').any(|w| {
        matches!(w, "reload" | "install" | "poweroff" | "reboot" | "shutdown")
            || w.starts_with("disk")
    })
}

/// A driver tool name: `[a-z0-9_]{1,64}`.
fn tool_name_ok(tool: &str) -> bool {
    (1..=64).contains(&tool.len())
        && tool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Redact OTP-looking codes, then clip to [`MAX_TEXT`].
pub fn clean_text(s: &str) -> String {
    let s = lucina_phone_core::redact_otps(s);
    if s.chars().count() <= MAX_TEXT {
        return s.into_owned();
    }
    s.chars().take(MAX_TEXT).collect::<String>() + "\n[truncated]"
}

fn link(ctx: &Ctx) -> Result<&Arc<dyn JunoLink>, ToolError> {
    ctx.env
        .juno
        .as_ref()
        .ok_or_else(|| ToolError::new("the Juno bridge is not running on this server"))
}

/// What the model reads of a device result: its text and whether it had an
/// image. A kept screenshot (`image.src`, set by the bridge) goes to the reply.
fn answer(ctx: &Ctx, tool: &str, r: &Value, empty: &str) -> Value {
    let text = r["text"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(empty);
    let image = r["image"].is_object();
    if let Some(src) = r["image"]["src"].as_str() {
        ctx.push_effect(Effect::Image {
            src: src.to_owned(),
            alt: "Juno's screen".into(),
            caption: "Juno, the laptop's screen".into(),
        });
    }
    json!({"tool": tool, "text": clean_text(text), "image": image})
}

async fn run(ctx: &Ctx, tool: &str, args: Value, limit: Duration) -> Result<Value, ToolError> {
    let device = device_of_session(&ctx.session_id);
    link(ctx)?
        .call(device, tool, args, limit.saturating_sub(MARGIN))
        .await
        .map_err(|e| ToolError::new(clean_text(&e)))
}

fn s<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn chars(v: &Value) -> usize {
    v.as_str().map_or(0, |t| t.chars().count())
}

/// One line for a driver tool call, without payloads (typed text, file
/// contents, the command's arguments): `type_text (42 chars)`. The tools'
/// `summarize` and the bridge's `juno_audit` log both use it.
pub fn driver_summary(tool: &str, a: &Value) -> String {
    let tool: String = tool.chars().take(64).collect();
    let named = |k: &str| {
        let v: String = s(a, k).chars().take(120).collect();
        (!v.is_empty()).then(|| format!(" {v}"))
    };
    let detail = match tool.as_str() {
        "type_text" => Some(format!(" ({} chars)", chars(&a["text"]))),
        "write_file" => Some(format!(
            "{} ({} chars{})",
            named("path").unwrap_or_default(),
            chars(&a["text"]).max(chars(&a["base64"])),
            if a["overwrite"] == json!(true) {
                ", overwrite"
            } else {
                ""
            }
        )),
        "notify" => Some(format!(
            " ({} chars)",
            chars(&a["title"]) + chars(&a["body"])
        )),
        "click" | "move_pointer" => Some(format!(" {},{}", a["x"], a["y"])),
        "bash" => {
            let cmd = s(a, "command").trim();
            let program: String = cmd
                .split_whitespace()
                .next()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect();
            Some(format!(" {program} ({} chars)", cmd.chars().count()))
        }
        _ => ["path", "id", "ssid", "key", "percent"]
            .into_iter()
            .find_map(|k| named(k).or_else(|| a[k].as_u64().map(|n| format!(" {n}")))),
    };
    format!("{tool}{}", detail.unwrap_or_default())
}

#[derive(Deserialize, JsonSchema)]
struct ToolsArgs {
    /// A substring of a tool name, to get the full input schemas of the matching tools.
    #[serde(default)]
    name: Option<String>,
}

pub struct JunoToolsTool;

#[async_trait::async_trait]
impl Tool for JunoToolsTool {
    fn name(&self) -> &'static str {
        "juno_tools"
    }

    fn description(&self) -> &'static str {
        "Juno, Giuseppe's laptop (Galaxy Book Go, LucinaOS): the driver tools it has right now (screen, input, apps, files, status, settings), from the laptop itself, plus whether it is connected and when it was last seen. Call it before juno_call when unsure which tools or arguments exist; `name` gives the full input schemas of the matching tools."
    }

    fn parameters(&self) -> Value {
        schema_for::<ToolsArgs>()
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Device
    }

    fn summarize(&self, args: &Value) -> String {
        match s(args, "name").trim() {
            "" => "juno_tools".into(),
            n => format!("juno_tools {}", n.chars().take(64).collect::<String>()),
        }
    }

    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: ToolsArgs = super::args(args)?;
        let name = a.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
        if name.is_some_and(|n| n.len() > 64) {
            return Err(ToolError::new("`name` is a short substring"));
        }
        let status = link(ctx)?.status();
        let r = run(ctx, "tools", json!({"name": name}), DEFAULT_LIMIT).await?;
        let mut out = answer(ctx, "tools", &r, "No tools listed.");
        out["devices"] = status["devices"].clone();
        Ok(out)
    }
}

#[derive(Deserialize, JsonSchema)]
struct CallArgs {
    /// A driver tool from juno_tools, e.g. click, type_text, press_key, scroll, list_apps, open_app, close_app, list_dir, read_file, write_file, status, set_volume, notify, wait.
    tool: String,
    /// The tool's arguments (see juno_tools for each tool's schema).
    #[serde(default)]
    arguments: Option<Map<String, Value>>,
}

pub struct JunoCallTool;

#[async_trait::async_trait]
impl Tool for JunoCallTool {
    fn name(&self) -> &'static str {
        "juno_call"
    }

    fn description(&self) -> &'static str {
        "One driver tool on Juno, Giuseppe's laptop (LucinaOS): look at and act on its screen (click, scroll, press_key, type_text, wait), its apps (list_apps, open_app, close_app, focus_app, play_file), files under his home (list_dir, read_file, write_file, delete_file), and status and settings (status: battery, Wi-Fi, CPU, memory; set_volume, wifi_connect, notify, notifications). Coordinates are pixels of the real screen (take juno_screenshot first). Juno shows one app at a time: opening an app closes the one in front. Deleting a file, overwriting one and joining a Wi-Fi network wait for Giuseppe's yes. This is his own laptop, not your computer sandbox."
    }

    fn parameters(&self) -> Value {
        schema_for::<CallArgs>()
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Device
    }

    fn needs_confirmation(&self, args: &Value) -> bool {
        match s(args, "tool").trim() {
            "delete_file" | "wifi_connect" => true,
            "write_file" => args["arguments"]["overwrite"] == json!(true),
            _ => false,
        }
    }

    fn time_limit(&self, args: &Value) -> Duration {
        if s(args, "tool").trim() == "wait" {
            let ms = args["arguments"]["timeout_ms"]
                .as_u64()
                .unwrap_or(WAIT_MAX_MS)
                .min(WAIT_MAX_MS);
            return Duration::from_millis(ms) + Duration::from_secs(10);
        }
        DEFAULT_LIMIT
    }

    fn summarize(&self, args: &Value) -> String {
        let empty = json!({});
        let a = args.get("arguments").unwrap_or(&empty);
        format!("juno_call {}", driver_summary(s(args, "tool").trim(), a))
    }

    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let limit = self.time_limit(&args);
        let a: CallArgs = super::args(args)?;
        let tool = a.tool.trim();
        if !tool_name_ok(tool) {
            return Err(ToolError::new(
                "`tool` is a driver tool name from juno_tools",
            ));
        }
        if refused(tool) {
            return Err(ToolError::new(format!(
                "{tool} is not available to you: reloads, installs, power and disk tools stay with Giuseppe"
            )));
        }
        let r = run(
            ctx,
            tool,
            Value::Object(a.arguments.unwrap_or_default()),
            limit,
        )
        .await?;
        Ok(answer(ctx, tool, &r, "done"))
    }
}

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

pub struct JunoScreenshotTool;

#[async_trait::async_trait]
impl Tool for JunoScreenshotTool {
    fn name(&self) -> &'static str {
        "juno_screenshot"
    }

    fn description(&self) -> &'static str {
        "What is on Juno's screen now (Giuseppe's laptop): the app in front and the screen size as text; the picture itself is shown to Giuseppe with your reply. Take one before clicking on the laptop."
    }

    fn parameters(&self) -> Value {
        schema_for::<NoArgs>()
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Device
    }

    fn summarize(&self, _args: &Value) -> String {
        "juno_screenshot".into()
    }

    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let _: NoArgs = super::args(args)?;
        let r = run(ctx, "screenshot", json!({}), DEFAULT_LIMIT).await?;
        Ok(answer(ctx, "screenshot", &r, "No screen description."))
    }
}

#[derive(Deserialize, JsonSchema)]
struct BashArgs {
    /// One shell command (`/bin/sh -c`) on the laptop, in Giuseppe's home folder.
    command: String,
    /// Seconds before the command is stopped (1-110, default 110).
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

pub struct JunoBashTool;

/// `timeout_seconds` clamped to 1..=110 (none: 110).
fn bash_secs(v: Option<u64>) -> u64 {
    v.unwrap_or(BASH_MAX_SECS).clamp(1, BASH_MAX_SECS)
}

#[async_trait::async_trait]
impl Tool for JunoBashTool {
    fn name(&self) -> &'static str {
        "juno_bash"
    }

    fn description(&self) -> &'static str {
        "One shell command on Juno, Giuseppe's laptop (LucinaOS), in his home folder; output is capped. For quick checks and file work the driver tools do not cover. This is his own laptop, not your computer sandbox."
    }

    fn parameters(&self) -> Value {
        schema_for::<BashArgs>()
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Device
    }

    fn time_limit(&self, args: &Value) -> Duration {
        Duration::from_secs(bash_secs(args["timeout_seconds"].as_u64()) + 10)
    }

    fn summarize(&self, args: &Value) -> String {
        format!("juno_{}", driver_summary("bash", args))
    }

    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let limit = self.time_limit(&args);
        let a: BashArgs = super::args(args)?;
        let secs = bash_secs(a.timeout_seconds);
        let cmd = a.command.trim();
        if cmd.is_empty() || cmd.len() > 8000 {
            return Err(ToolError::new("`command` must be 1-8000 bytes"));
        }
        let r = run(
            ctx,
            "bash",
            json!({"command": cmd, "timeoutSecs": secs}),
            limit,
        )
        .await?;
        Ok(answer(ctx, "bash", &r, ""))
    }
}

/// The four tools, for the server's owner registry.
pub fn all() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(JunoToolsTool),
        Arc::new(JunoCallTool),
        Arc::new(JunoScreenshotTool),
        Arc::new(JunoBashTool),
    ]
}

/// Their names.
pub const NAMES: &[&str] = &["juno_tools", "juno_call", "juno_screenshot", "juno_bash"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// One recorded call: device, tool, arguments, timeout.
    type Sent = (Option<String>, String, Value, Duration);

    /// Records each call and answers with `reply` (or `fail`).
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<Sent>>,
        reply: Mutex<Value>,
        fail: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl JunoLink for Fake {
        async fn call(
            &self,
            device: Option<String>,
            tool: &str,
            args: Value,
            timeout: Duration,
        ) -> Result<Value, String> {
            self.calls
                .lock()
                .unwrap()
                .push((device, tool.to_owned(), args, timeout));
            match self.fail.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(self.reply.lock().unwrap().clone()),
            }
        }
        fn status(&self) -> Value {
            json!({"devices": [{"id": "dev1", "name": "Juno", "connected": true, "lastSeenSecs": 3}]})
        }
    }

    fn ctx(fake: &Arc<Fake>, session: &str) -> Ctx {
        let mut env = crate::ToolEnv::new(std::env::temp_dir(), chrono_tz::Europe::Copenhagen);
        env.juno = Some(fake.clone());
        Ctx::new(Arc::new(env), "ios", session)
    }

    fn fake(reply: Value) -> Arc<Fake> {
        let f = Arc::new(Fake::default());
        *f.reply.lock().unwrap() = reply;
        f
    }

    #[test]
    fn juno_device_comes_from_the_session() {
        assert_eq!(
            device_of_session("secretary_juno-3f2a-9c").as_deref(),
            Some("3f2a-9c")
        );
        assert_eq!(device_of_session("juno-abc_1").as_deref(), Some("abc_1"));
        assert_eq!(device_of_session("secretary_ios-1"), None);
        assert_eq!(device_of_session("secretary_juno-"), None);
        assert_eq!(device_of_session("secretary_mcp-claude-juno-x"), None);
        assert_eq!(device_of_session("juno"), None);
        assert_eq!(device_of_session("juno-a b"), None);
    }

    #[test]
    fn juno_confirmation_rules() {
        let c = |v: Value| JunoCallTool.needs_confirmation(&v);
        assert!(c(
            json!({"tool": "delete_file", "arguments": {"path": "/data/home/a"}})
        ));
        assert!(c(
            json!({"tool": "wifi_connect", "arguments": {"ssid": "Home"}})
        ));
        assert!(c(
            json!({"tool": "write_file", "arguments": {"path": "a", "text": "x", "overwrite": true}})
        ));
        assert!(!c(
            json!({"tool": "write_file", "arguments": {"path": "a", "text": "x"}})
        ));
        assert!(!c(
            json!({"tool": "write_file", "arguments": {"path": "a", "text": "x", "overwrite": false}})
        ));
        assert!(!c(
            json!({"tool": "open_app", "arguments": {"id": "gallery"}})
        ));
        assert!(!JunoBashTool.needs_confirmation(&json!({"command": "ls"})));
        assert!(!JunoScreenshotTool.needs_confirmation(&json!({})));
    }

    #[tokio::test]
    async fn juno_refused_tools_are_never_sent() {
        let f = fake(json!({"text": "ok"}));
        for tool in [
            "reload",
            "install",
            "poweroff",
            "reboot",
            "disk",
            "disk_wipe",
            "diskpart",
            "generation_install",
        ] {
            let e = JunoCallTool
                .call(json!({"tool": tool}), &ctx(&f, "secretary_ios"))
                .await
                .unwrap_err();
            assert!(e.0.contains("not available"), "{tool}: {e}");
        }
        for tool in ["Click", "a/b", "", &"x".repeat(65)] {
            assert!(
                JunoCallTool
                    .call(json!({"tool": tool}), &ctx(&f, "s"))
                    .await
                    .is_err(),
                "{tool}"
            );
        }
        assert!(f.calls.lock().unwrap().is_empty());
        assert!(!refused("status") && !refused("list_dir") && !refused("set_volume"));
    }

    #[tokio::test]
    async fn juno_call_goes_to_the_sessions_device() {
        let f = fake(json!({"text": "Gallery is open"}));
        let out = JunoCallTool
            .call(
                json!({"tool": "open_app", "arguments": {"id": "gallery"}}),
                &ctx(&f, "secretary_juno-dev1"),
            )
            .await
            .unwrap();
        assert_eq!(out["text"], "Gallery is open");
        assert_eq!(out["image"], false);
        JunoCallTool
            .call(json!({"tool": "status"}), &ctx(&f, "secretary_ios"))
            .await
            .unwrap();
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls[0].0.as_deref(), Some("dev1"));
        assert_eq!(calls[0].1, "open_app");
        assert_eq!(calls[0].2, json!({"id": "gallery"}));
        assert_eq!(
            calls[1].0, None,
            "another app's turn: the most recent laptop"
        );
        assert_eq!(calls[1].2, json!({}));
    }

    #[tokio::test]
    async fn juno_bash_clamps_and_maps_its_timeout() {
        let f = fake(json!({"text": "Linux juno"}));
        let c = ctx(&f, "s");
        JunoBashTool
            .call(json!({"command": "uname -a", "timeout_seconds": 500}), &c)
            .await
            .unwrap();
        JunoBashTool
            .call(json!({"command": "true", "timeout_seconds": 0}), &c)
            .await
            .unwrap();
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls[0].1, "bash");
        assert_eq!(
            calls[0].2,
            json!({"command": "uname -a", "timeoutSecs": 110})
        );
        assert_eq!(calls[1].2["timeoutSecs"], 1);
        assert_eq!(
            JunoBashTool.time_limit(&json!({"command": "x", "timeout_seconds": 20})),
            Duration::from_secs(30)
        );
        assert_eq!(
            JunoBashTool.time_limit(&json!({"command": "x"})),
            Duration::from_secs(120)
        );
        assert!(calls[0].3 < Duration::from_secs(120));
        let wait = |ms: u64| {
            JunoCallTool.time_limit(&json!({"tool": "wait", "arguments": {"timeout_ms": ms}}))
        };
        assert_eq!(wait(5_000), Duration::from_secs(15));
        assert_eq!(wait(900_000), Duration::from_secs(70));
        assert_eq!(
            JunoCallTool.time_limit(&json!({"tool": "click"})),
            DEFAULT_LIMIT
        );
        assert_eq!(JunoToolsTool.time_limit(&json!({})), DEFAULT_LIMIT);
    }

    #[tokio::test]
    async fn juno_text_is_clipped_and_redacted() {
        let long = format!(
            "Your verification code is 482913. {}",
            "y".repeat(MAX_TEXT * 2)
        );
        let f = fake(json!({"text": long}));
        let out = JunoBashTool
            .call(json!({"command": "cat log"}), &ctx(&f, "s"))
            .await
            .unwrap();
        let t = out["text"].as_str().unwrap();
        assert!(!t.contains("482913"), "{}", &t[..60]);
        assert!(t.ends_with("[truncated]"));
        assert!(t.chars().count() <= MAX_TEXT + 12);
        // A device error is redacted too.
        *f.fail.lock().unwrap() = Some("login failed, verification code 551204".into());
        let e = JunoBashTool
            .call(json!({"command": "x"}), &ctx(&f, "s"))
            .await
            .unwrap_err();
        assert!(!e.0.contains("551204"), "{e}");
    }

    #[tokio::test]
    async fn juno_screenshot_keeps_the_png_out_of_the_text() {
        let f = fake(json!({"text": "Gallery in front, 1920x1080",
            "image": {"src": "/v1/juno/screenshots/abc123.png", "mimeType": "image/png", "bytes": 1000}}));
        let c = ctx(&f, "secretary_juno-dev1");
        let out = JunoScreenshotTool.call(json!({}), &c).await.unwrap();
        assert_eq!(out["image"], true);
        assert_eq!(out["text"], "Gallery in front, 1920x1080");
        assert!(out.get("src").is_none());
        let shown: Vec<String> = c
            .effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Image { src, .. } => Some(src),
                _ => None,
            })
            .collect();
        assert_eq!(shown, ["/v1/juno/screenshots/abc123.png"]);
        // A raw base64 image (no bridge in between) never reaches the model either.
        let f = fake(
            json!({"text": "screen", "image": {"data": "iVBORw0KGgo=", "mimeType": "image/png"}}),
        );
        let c = ctx(&f, "s");
        let out = JunoScreenshotTool.call(json!({}), &c).await.unwrap();
        assert!(!out.to_string().contains("iVBOR"));
        assert!(c.effects().is_empty());
        assert_eq!(f.calls.lock().unwrap()[0].1, "screenshot");
    }

    #[tokio::test]
    async fn juno_tools_lists_and_says_who_is_connected() {
        let f = fake(json!({"text": "click, type_text, status", "tools": [{"name": "click"}]}));
        let out = JunoToolsTool
            .call(json!({"name": "cli"}), &ctx(&f, "s"))
            .await
            .unwrap();
        assert_eq!(out["text"], "click, type_text, status");
        assert_eq!(out["devices"][0]["connected"], true);
        assert_eq!(f.calls.lock().unwrap()[0].2, json!({"name": "cli"}));
    }

    #[tokio::test]
    async fn juno_without_a_bridge_says_so() {
        let c = Ctx::new(
            Arc::new(crate::ToolEnv::new(
                std::env::temp_dir(),
                chrono_tz::Europe::Copenhagen,
            )),
            "ios",
            "s",
        );
        let e = JunoScreenshotTool.call(json!({}), &c).await.unwrap_err();
        assert!(e.0.contains("not running"));
    }

    #[test]
    fn juno_summaries_carry_no_payloads() {
        let typed = JunoCallTool
            .summarize(&json!({"tool": "type_text", "arguments": {"text": "my secret words"}}));
        assert_eq!(typed, "juno_call type_text (15 chars)");
        let w = JunoCallTool.summarize(&json!({"tool": "write_file", "arguments": {"path": "/data/home/a.txt", "text": "hello", "overwrite": true}}));
        assert_eq!(
            w,
            "juno_call write_file /data/home/a.txt (5 chars, overwrite)"
        );
        assert_eq!(
            JunoCallTool
                .summarize(&json!({"tool": "delete_file", "arguments": {"path": "/data/home/x"}})),
            "juno_call delete_file /data/home/x"
        );
        assert_eq!(
            JunoCallTool.summarize(&json!({"tool": "click", "arguments": {"x": 10, "y": 20}})),
            "juno_call click 10,20"
        );
        assert_eq!(
            JunoBashTool.summarize(&json!({"command": "echo hunter2 > f"})),
            "juno_bash echo (16 chars)"
        );
        for t in all() {
            assert!(NAMES.contains(&t.name()));
            assert_eq!(t.side_effect(), SideEffect::Device);
            assert!(t.parameters().to_string().contains("\"type\":\"object\""));
        }
    }
}
