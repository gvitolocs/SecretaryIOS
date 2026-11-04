//! `juno_*`: Lucina's own tools on Juno, the owner's laptop (Galaxy Book Go,
//! LucinaOS). The same four as secretary-mcp's `android_*` (`juno_tools`,
//! `juno_call`, `juno_screenshot`, `juno_bash`), with the same argument names,
//! and `juno_events` for what the laptop reported (`docs/research/juno-lucina-tools.md`
//! in LucinaOS, "Rights").
//!
//! A call travels through [`JunoLink`] (lucina-server's `JunoBridge`): it is
//! queued for the laptop, which long-polls for work through secretary-server
//! and posts the result back. Registered by the server in the owner's
//! registry only (`LUCINA_JUNO_TOOLS`, default on), never on MCP turns.
//!
//! - Which laptop: the turn's session (`secretary_juno-<device>` from Juno's
//!   Chat) names it, see [`device_of_session`]; any other turn gets the one
//!   that polled most recently.
//! - Rights: while the owner is signed in on Juno, Lucina has his full rights.
//!   [`off_limits`] calls (the other UFS LUNs, the Qualcomm firmware, the PMIC
//!   charge limits) are refused here and never sent, with or without a
//!   decision. [`risky`] calls (installs, reloads, power, wipes, the boot
//!   chain, deletes outside his home) wait for a decision
//!   ([`JunoLink::ask`]): the call again with its `decision_id` runs once the
//!   decision says "Go" for exactly that call ([`call_hash`]). A device that
//!   answers `needs_decision` is handled the same way, the stricter
//!   [`CallEffect`] winning; `owner_only` is never sent.
//! - `juno_call` still asks the owner first for a plain `delete_file`,
//!   `write_file` with `overwrite`, and `wifi_connect`.
//! - Every text is secret- and OTP-redacted and clipped to [`MAX_TEXT`]. A
//!   screenshot's PNG never reaches the model: the bridge keeps it and the
//!   reply shows it as an image block ([`Effect::Image`]).

use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{schema_for, Ctx, Effect, Origin, SideEffect, Tool, ToolError, ToolResult};

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

/// How a call goes out besides its tool and arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallOpts {
    /// The decision that lets a risky call run (`dc-…`).
    pub decision_id: Option<String>,
    /// Lucina's own step (secret_keep's `juno:` source): the result never
    /// reaches a tool loop, the timeline, tracing or the ops record. Never set
    /// from a tool's arguments.
    pub internal: bool,
}

/// A decision to file for a risky call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The asking turn's session.
    pub session: String,
    /// The call's one-line summary (the question).
    pub summary: String,
    /// What can and cannot be undone.
    pub context: String,
    /// [`call_hash`] of the call as it will be sent.
    pub hash: String,
    /// The device's own `call_hash`, when the device asked for the decision.
    pub device_hash: Option<String>,
}

/// What `juno_events` asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventQuery {
    /// Only events with a `seq` above this.
    pub since: Option<u64>,
    /// Only this kind, or kinds starting with it and a dot (`app` → `app.focused`).
    pub kind: Option<String>,
    pub app: Option<String>,
    pub max: usize,
}

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
        opts: &CallOpts,
    ) -> Result<Value, String>;
    /// The laptops and when each was last seen (`{"devices": [{id, name, connected, lastSeenSecs, newEvents}]}`).
    fn status(&self) -> Value;
    /// File a decision for a risky call; its id.
    async fn ask(&self, ask: Ask) -> Result<String, String>;
    /// `Ok`: decision `id` was filed for the call with `hash` and the answer
    /// was "Go". `Err`: why the call may not run.
    fn decided(&self, id: &str, hash: &str) -> Result<(), String>;
    /// The laptop's latest events (`{"events": [...], "lost": n}`); reading
    /// them marks them seen.
    fn events(&self, device: Option<&str>, q: &EventQuery) -> Value;
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

/// What a call does, least to most strict. The device reports one with
/// `needs_decision`; the stricter of its class and the server's wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CallEffect {
    Read,
    Write,
    Destructive,
    External,
    /// Never sent: the owner does it himself.
    OwnerOnly,
}

impl CallEffect {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "read" => Self::Read,
            "write" => Self::Write,
            "destructive" => Self::Destructive,
            "external" => Self::External,
            "owner_only" => Self::OwnerOnly,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Destructive => "destructive",
            Self::External => "external",
            Self::OwnerOnly => "owner_only",
        }
    }
}

/// The server's class of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Plain,
    /// Waits for a decision; the reason is what it touches.
    Risky(&'static str),
    /// Never sent.
    OffLimits(&'static str),
}

impl Class {
    pub fn effect(self) -> CallEffect {
        match self {
            Class::Plain => CallEffect::Read,
            Class::Risky(_) => CallEffect::Destructive,
            Class::OffLimits(_) => CallEffect::OwnerOnly,
        }
    }
}

/// Every string in `v`, lowercased, one per line.
fn strings(v: &Value, out: &mut String) {
    match v {
        Value::String(t) => {
            out.push_str(&t.to_lowercase());
            out.push('\n');
        }
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(m) => m.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// `tool` and every string of its arguments, lowercased.
fn haystack(tool: &str, args: &Value) -> String {
    let mut h = format!("{}\n", tool.to_lowercase());
    strings(args, &mut h);
    h
}

/// `word` occurs in `h` with no letter or digit right before or after it.
fn has_word(h: &str, word: &str) -> bool {
    let alnum = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    h.match_indices(word).any(|(i, _)| {
        !alnum(h[..i].chars().next_back()) && !alnum(h[i + word.len()..].chars().next())
    })
}

/// `prefix` followed by a digit in `digits` and then no further digit.
fn has_numbered(h: &str, prefix: &str, digits: std::ops::RangeInclusive<char>) -> bool {
    h.match_indices(prefix).any(|(i, _)| {
        let mut rest = h[i + prefix.len()..].chars();
        rest.next().is_some_and(|c| digits.contains(&c))
            && !rest.next().is_some_and(|c| c.is_ascii_digit())
    })
}

/// The whitespace- and quote-separated words of `h` starting with `/dev/`.
fn dev_paths(h: &str) -> impl Iterator<Item = &str> {
    h.split(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | ';' | '|' | '&' | '(' | ')' | '=' | ',')
    })
    .filter(|w| w.starts_with("/dev/"))
}

/// Why a call is never sent, from any origin and even with a decision: the
/// UFS LUNs other than the data disk (`ufs0lun1`..`5`), the Qualcomm firmware
/// (`/lib/firmware/qcom`, the modem, ADSP and WLAN firmware, `fsg`,
/// `modemst*`), and writes of the PMIC charge and current limits.
pub fn off_limits(tool: &str, args: &Value) -> Option<&'static str> {
    let h = haystack(tool, args);
    if has_numbered(&h, "ufs0lun", '1'..='5')
        || dev_paths(&h).any(|p| has_numbered(p, "lun", '1'..='5'))
    {
        return Some("the laptop's boot and firmware disks (UFS LUN 1-5)");
    }
    let firmware_dir = [
        "/lib/firmware",
        "/vendor/firmware",
        "/firmware/",
        "firmware_mnt",
        "by-partlabel/",
    ]
    .iter()
    .any(|d| h.contains(d));
    if h.contains("/lib/firmware/qcom")
        || has_word(&h, "fsg")
        || has_word(&h, "modemst")
        || has_numbered(&h, "modemst", '1'..='2')
        || (firmware_dir && ["modem", "adsp", "wlan"].iter().any(|w| h.contains(w)))
    {
        return Some("the Qualcomm firmware (modem, ADSP, Wi-Fi)");
    }
    let reads = matches!(tool, "read_file" | "list_dir" | "status");
    let limit = h.contains("constant_charge") || h.contains("input_current_limit");
    let charge_cmd = h.lines().chain(h.split(['&', ';', '|'])).any(|l| {
        let mut w = l.split_whitespace();
        while let Some(x) = w.next() {
            if x.ends_with("lucina-charge") {
                return w
                    .next()
                    .is_some_and(|v| v == "set" || v.starts_with("limit"));
            }
        }
        false
    });
    if charge_cmd || (limit && !reads) {
        return Some("the battery's charge and current limits (PMIC)");
    }
    None
}

/// The tools that wait for a decision whatever their arguments.
fn risky_tool(tool: &str) -> bool {
    matches!(
        tool,
        "generation_install" | "reload" | "reboot" | "poweroff"
    ) || tool.starts_with("wipe")
        || tool.starts_with("format")
}

/// A `delete_file` path outside `/data/home` (relative paths are in his home).
fn outside_home(path: &str) -> bool {
    let p = path.trim();
    p.split('/').any(|c| c == "..")
        || p.starts_with('~') && !(p == "~" || p.starts_with("~/"))
        || p.starts_with('/') && !p.starts_with("/data/home/")
        || p == "~"
        || p.is_empty()
}

/// A shell `rm`/`rmdir`/`unlink`/`shred` whose operand is an absolute path outside `/data/home`.
fn bash_deletes_outside_home(cmd: &str) -> bool {
    cmd.split(['&', ';', '|', '\n', '(', ')', '`']).any(|part| {
        let mut words = part.split_whitespace().map(|w| w.trim_matches(['"', '\'']));
        let Some(first) = words
            .by_ref()
            .find(|w| *w != "sudo" && *w != "doas" && !w.contains('='))
        else {
            return false;
        };
        let prog = first.rsplit('/').next().unwrap_or(first);
        matches!(prog, "rm" | "rmdir" | "unlink" | "shred")
            && words.filter(|w| !w.starts_with('-')).any(|w| {
                (w.starts_with('/') || w.starts_with('~') || w.contains("..")) && outside_home(w)
            })
    })
}

/// Why a call waits for a decision: installs, reloads, power and wipes; a
/// `delete_file`, `write_file` or `juno_bash` that names the boot chain
/// (`/boot`, the ESP, the kernel command line), her installed apps
/// (`/data/lucina/apps`), the data disk (`/dev/ufs0lun0`), the installer,
/// `lucina-data clean`, `mkfs` or `dd of=`, or deletes outside `/data/home`.
pub fn risky(tool: &str, args: &Value) -> Option<&'static str> {
    if risky_tool(tool) {
        return Some("installs, reloads, power and wipes");
    }
    if !matches!(tool, "delete_file" | "write_file" | "bash") {
        return None;
    }
    let h = haystack("", args);
    let named = [
        ("/boot", "the boot partition"),
        ("/efi", "the EFI system partition"),
        ("partlabel/esp", "the EFI system partition"),
        ("/data/lucina/apps", "Lucina's installed apps"),
        ("/dev/ufs0lun0", "the data disk"),
        ("lucina-install-disk", "the installer"),
        ("lucina-data clean", "wiping the data partition"),
        ("mkfs", "formatting a disk"),
        ("cmdline", "the kernel command line"),
    ];
    if let Some((_, why)) = named.iter().find(|(n, _)| h.contains(n)) {
        return Some(why);
    }
    if has_word(&h, "esp") {
        return Some("the EFI system partition");
    }
    if h.split(['&', ';', '|', '\n'])
        .any(|p| has_word(p, "dd") && p.contains("of="))
    {
        return Some("writing a disk image (dd of=)");
    }
    let deletes_out = match tool {
        "delete_file" => outside_home(s(args, "path")),
        "bash" => bash_deletes_outside_home(&s(args, "command").to_lowercase()),
        _ => false,
    };
    deletes_out.then_some("a delete outside his home folder")
}

/// The server's class of a device call (off-limits first).
pub fn classify(tool: &str, args: &Value) -> Class {
    if let Some(why) = off_limits(tool, args) {
        return Class::OffLimits(why);
    }
    risky(tool, args).map_or(Class::Plain, Class::Risky)
}

/// The canonical hash of a device call, as the laptop computes it:
/// sha256 hex of `{"tool": tool, "args": args}` serialised by serde_json
/// (maps sorted: no crate in the build enables `preserve_order`).
pub fn call_hash(tool: &str, args: &Value) -> String {
    hash_of(&json!({"tool": tool, "args": args}))
}

/// The hash of an intent: `{"tool": "intent", "name": name, "args": args}`.
pub fn intent_hash(name: &str, args: &Value) -> String {
    hash_of(&json!({"tool": "intent", "name": name, "args": args}))
}

fn hash_of(v: &Value) -> String {
    let text = serde_json::to_string(v).unwrap_or_default();
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A driver tool name: `[a-z0-9_]{1,64}`.
fn tool_name_ok(tool: &str) -> bool {
    (1..=64).contains(&tool.len())
        && tool
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Redact credential values her registry knows and OTP-looking codes, then
/// clip to [`MAX_TEXT`].
pub fn clean_text(s: &str) -> String {
    let mut v = Value::String(s.to_owned());
    lucina_secrets::redactor().redact_json(&mut v);
    let s = v.as_str().unwrap_or_default();
    let s = lucina_phone_core::redact_otps(s);
    if s.chars().count() <= MAX_TEXT {
        return s.into_owned();
    }
    s.chars().take(MAX_TEXT).collect::<String>() + "\n[truncated]"
}

/// [`clean_text`] on every string of `v`.
fn clean_json(v: &mut Value) {
    match v {
        Value::String(t) => *t = clean_text(t),
        Value::Array(a) => a.iter_mut().for_each(clean_json),
        Value::Object(m) => m.values_mut().for_each(clean_json),
        _ => {}
    }
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
    send(ctx, tool, args, limit, &CallOpts::default()).await
}

async fn send(
    ctx: &Ctx,
    tool: &str,
    args: Value,
    limit: Duration,
    opts: &CallOpts,
) -> Result<Value, ToolError> {
    let device = device_of_session(&ctx.session_id);
    link(ctx)?
        .call(device, tool, args, limit.saturating_sub(MARGIN), opts)
        .await
        .map_err(|e| ToolError::new(clean_text(&e)))
}

/// What can and cannot be undone, for the decision's context.
fn undo_note(why: &str) -> String {
    format!(
        "Lucina wants to run this on Juno, the owner's laptop. It touches {why}: it cannot be undone from Lucina's side (a deleted file or a wiped, formatted or overwritten partition is gone; a reboot or poweroff ends what is running; an install replaces the running generation, which the boot menu can still roll back). Nothing is sent until the answer is Go."
    )
}

/// File a decision for `tool` with `args` and say so to the model.
async fn ask_first(
    ctx: &Ctx,
    tool: &str,
    args: &Value,
    why: &str,
    device_hash: Option<String>,
) -> ToolResult {
    let summary = driver_summary(tool, args);
    let id = link(ctx)?
        .ask(Ask {
            session: ctx.session_id.clone(),
            summary: format!("Juno: {summary}"),
            context: undo_note(why),
            hash: call_hash(tool, args),
            device_hash,
        })
        .await
        .map_err(|e| ToolError::new(format!("could not ask for a decision: {e}")))?;
    Ok(json!({
        "tool": tool,
        "decision_id": id,
        "text": format!("waiting on decision {id}: \"{summary}\" touches {why} and needs a Go. Tell Giuseppe; once it is decided, make the same call again with decision_id {id}."),
    }))
}

/// A driver call with its rights: off-limits never goes out, a risky one (or
/// one the device asks a decision for) waits for a "Go" on exactly this call.
async fn guarded(
    ctx: &Ctx,
    tool: &str,
    args: Value,
    decision_id: Option<&str>,
    limit: Duration,
    empty: &str,
) -> ToolResult {
    let class = classify(tool, &args);
    if let Class::OffLimits(why) = class {
        return Err(ToolError::new(format!(
            "{} is off limits: it touches {why}, which stays with Giuseppe; nothing was sent",
            driver_summary(tool, &args)
        )));
    }
    let decision_id = decision_id.map(str::trim).filter(|d| !d.is_empty());
    if let Some(id) = decision_id {
        link(ctx)?
            .decided(id, &call_hash(tool, &args))
            .map_err(|e| ToolError::new(format!("decision {id} does not let this run: {e}")))?;
    } else if let Class::Risky(why) = class {
        return ask_first(ctx, tool, &args, why, None).await;
    }
    let opts = CallOpts {
        decision_id: decision_id.map(str::to_owned),
        internal: false,
    };
    let r = send(ctx, tool, args.clone(), limit, &opts).await?;
    if r["needs_decision"] == json!(true) {
        let device = CallEffect::parse(s(&r, "effect")).unwrap_or(CallEffect::Destructive);
        let effect = device.max(class.effect());
        if effect == CallEffect::OwnerOnly {
            return Err(ToolError::new(format!(
                "Juno says {} is for Giuseppe alone (owner_only); it was not run",
                driver_summary(tool, &args)
            )));
        }
        if decision_id.is_some() {
            return Err(ToolError::new(
                "Juno asked again for a decision on a call that already had one; it was not run",
            ));
        }
        let device_hash = r["call_hash"]
            .as_str()
            .filter(|h| (1..=128).contains(&h.len()))
            .map(str::to_owned);
        let why = match effect {
            CallEffect::External => "something outside the laptop (Juno's own judgement)",
            CallEffect::Destructive => "something that cannot be undone (Juno's own judgement)",
            _ => "something Juno wants a decision for",
        };
        return ask_first(ctx, tool, &args, why, device_hash).await;
    }
    Ok(answer(ctx, tool, &r, empty))
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

/// The laptop's new events since they were last read, as one line.
fn events_line(status: &Value) -> Option<String> {
    let n: u64 = status["devices"]
        .as_array()?
        .iter()
        .filter_map(|d| d["newEvents"].as_u64())
        .sum();
    (n > 0).then(|| {
        format!("{n} new events from the laptop since the last look (juno_events reads them).")
    })
}

#[async_trait::async_trait]
impl Tool for JunoToolsTool {
    fn name(&self) -> &'static str {
        "juno_tools"
    }

    fn description(&self) -> &'static str {
        "Juno, Giuseppe's laptop (Galaxy Book Go, LucinaOS): the driver tools it has right now (screen, input, apps, files, status, settings), from the laptop itself, plus whether it is connected, when it was last seen and how many new events it reported. Call it before juno_call when unsure which tools or arguments exist; `name` gives the full input schemas of the matching tools. When it lists ui_tree/ui_act or intent as available, prefer those."
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
        if let Some(line) = events_line(&status) {
            out["text"] = json!(format!("{}\n{line}", out["text"].as_str().unwrap_or("")));
        }
        out["devices"] = status["devices"].clone();
        Ok(out)
    }
}

#[derive(Deserialize, JsonSchema)]
struct CallArgs {
    /// A driver tool from juno_tools, e.g. ui_tree, ui_act, intent, click, type_text, press_key, scroll, list_apps, open_app, close_app, play_file, list_dir, read_file, write_file, status, set_volume, notify, wait.
    tool: String,
    /// The tool's arguments (see juno_tools for each tool's schema).
    #[serde(default)]
    arguments: Option<Map<String, Value>>,
    /// The decision that lets this exact call run (from an earlier "waiting on decision" answer).
    #[serde(default)]
    decision_id: Option<String>,
}

pub struct JunoCallTool;

/// A model's arguments, without the `internal` flag only Lucina's own steps set.
fn model_args(a: Option<Map<String, Value>>) -> Value {
    let mut a = a.unwrap_or_default();
    a.remove("internal");
    Value::Object(a)
}

/// The plain (not risky) calls that still wait for the owner's yes in chat.
fn plain_confirmation(tool: &str, args: &Value) -> bool {
    let a = &args["arguments"];
    let asks = match tool {
        "delete_file" | "wifi_connect" => true,
        "write_file" => a["overwrite"] == json!(true),
        _ => false,
    };
    asks && classify(tool, a) == Class::Plain && args.get("decision_id").is_none_or(Value::is_null)
}

#[async_trait::async_trait]
impl Tool for JunoCallTool {
    fn name(&self) -> &'static str {
        "juno_call"
    }

    fn description(&self) -> &'static str {
        "One driver tool on Juno, Giuseppe's laptop (LucinaOS), with his full rights while he is signed in there. When juno_tools lists them as available, prefer ui_tree (the screen's elements) and ui_act (act on one), and intent for app actions; use click and type_text only for what those do not cover. Also its apps (list_apps, open_app, close_app, focus_app, play_file), files (list_dir, read_file, write_file, delete_file), and status and settings (status: battery, Wi-Fi, CPU, memory; set_volume, wifi_connect, notify, notifications). Coordinates are pixels of the real screen (take juno_screenshot first). Juno shows one app at a time: opening an app closes the one in front. Installs, reloads, power, wipes, the boot chain and deletes outside his home answer \"waiting on decision <id>\": once it is decided, repeat the same call with that decision_id. The firmware, the other disks and the charge limits are off limits. Deleting or overwriting a file in his home and joining a Wi-Fi network wait for his yes. This is his own laptop, not your computer sandbox."
    }

    fn parameters(&self) -> Value {
        schema_for::<CallArgs>()
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Device
    }

    fn needs_confirmation(&self, args: &Value) -> bool {
        plain_confirmation(s(args, "tool").trim(), args)
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
        guarded(
            ctx,
            tool,
            model_args(a.arguments),
            a.decision_id.as_deref(),
            limit,
            "done",
        )
        .await
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
        "What is on Juno's screen now (Giuseppe's laptop): the app in front and the screen size as text; the picture itself is shown to Giuseppe with your reply. Take one before clicking on the laptop; when juno_tools lists ui_tree as available, that reads the screen's elements instead."
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
    /// The decision that lets this exact command run (from an earlier "waiting on decision" answer).
    #[serde(default)]
    decision_id: Option<String>,
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
        "One shell command on Juno, Giuseppe's laptop (LucinaOS), in his home folder, with his full rights while he is signed in there; output is capped. For quick checks and file work the driver tools do not cover. A command that touches the boot chain, formats or images a disk, or deletes outside his home answers \"waiting on decision <id>\": once it is decided, run the same command again with that decision_id. The firmware, the other disks and the charge limits are off limits. This is his own laptop, not your computer sandbox."
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
        guarded(
            ctx,
            "bash",
            json!({"command": cmd, "timeoutSecs": secs}),
            a.decision_id.as_deref(),
            limit,
            "",
        )
        .await
    }
}

#[derive(Deserialize, JsonSchema)]
struct EventsArgs {
    /// Only events after this `seq` (from an earlier juno_events answer).
    #[serde(default)]
    since: Option<u64>,
    /// Only this kind, e.g. `app.focused`, or a family such as `media`.
    #[serde(default)]
    kind: Option<String>,
    /// Only events of this app.
    #[serde(default)]
    app: Option<String>,
    /// At most this many, newest last (1-100, default 20).
    #[serde(default)]
    max: Option<usize>,
}

pub struct JunoEventsTool;

#[async_trait::async_trait]
impl Tool for JunoEventsTool {
    fn name(&self) -> &'static str {
        "juno_events"
    }

    fn description(&self) -> &'static str {
        "What happened on Juno, Giuseppe's laptop, as it reported it: apps coming to the front, media starting and ending, power and network changes (the last 500 per laptop). Filter by kind (app.focused, media, power, net), app, or since a seq."
    }

    fn parameters(&self) -> Value {
        schema_for::<EventsArgs>()
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Device
    }

    fn summarize(&self, args: &Value) -> String {
        match s(args, "kind").trim() {
            "" => "juno_events".into(),
            k => format!("juno_events {}", k.chars().take(64).collect::<String>()),
        }
    }

    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        if ctx.origin != Origin::Owner {
            return Err(ToolError::new(
                "Juno's events are for the owner's own turns",
            ));
        }
        let a: EventsArgs = super::args(args)?;
        let short = |v: Option<String>| {
            v.map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .map(|t| t.chars().take(64).collect::<String>())
        };
        let q = EventQuery {
            since: a.since,
            kind: short(a.kind),
            app: short(a.app),
            max: a.max.unwrap_or(20).clamp(1, 100),
        };
        let device = device_of_session(&ctx.session_id);
        let mut out = link(ctx)?.events(device.as_deref(), &q);
        clean_json(&mut out);
        Ok(json!({"tool": "events", "events": out["events"], "lost": out["lost"]}))
    }
}

/// The five tools, for the server's owner registry.
pub fn all() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(JunoToolsTool),
        Arc::new(JunoCallTool),
        Arc::new(JunoScreenshotTool),
        Arc::new(JunoBashTool),
        Arc::new(JunoEventsTool),
    ]
}

/// Their names.
pub const NAMES: &[&str] = &[
    "juno_tools",
    "juno_call",
    "juno_screenshot",
    "juno_bash",
    "juno_events",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// One recorded call: device, tool, arguments, timeout, options.
    type Sent = (Option<String>, String, Value, Duration, CallOpts);

    /// Records each call and answers with `reply` (or `fail`); decisions are
    /// filed as `dc-000000000001`… and `go` holds the hashes decided "Go".
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<Sent>>,
        reply: Mutex<Value>,
        fail: Mutex<Option<String>>,
        asked: Mutex<Vec<Ask>>,
        go: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl JunoLink for Fake {
        async fn call(
            &self,
            device: Option<String>,
            tool: &str,
            args: Value,
            timeout: Duration,
            opts: &CallOpts,
        ) -> Result<Value, String> {
            self.calls
                .lock()
                .unwrap()
                .push((device, tool.to_owned(), args, timeout, opts.clone()));
            match self.fail.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(self.reply.lock().unwrap().clone()),
            }
        }
        fn status(&self) -> Value {
            json!({"devices": [{"id": "dev1", "name": "Juno", "connected": true, "lastSeenSecs": 3, "newEvents": 4}]})
        }
        async fn ask(&self, ask: Ask) -> Result<String, String> {
            let mut asked = self.asked.lock().unwrap();
            asked.push(ask);
            Ok(format!("dc-{:012}", asked.len()))
        }
        fn decided(&self, id: &str, hash: &str) -> Result<(), String> {
            let go = self.go.lock().unwrap();
            if go.iter().any(|(i, h)| i == id && h == hash) {
                Ok(())
            } else {
                Err("not decided Go for this call".into())
            }
        }
        fn events(&self, _device: Option<&str>, q: &EventQuery) -> Value {
            json!({"events": [{"seq": 1, "kind": q.kind.clone().unwrap_or_default(), "subject": "Your verification code is 482913"}], "lost": 0})
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
        assert!(!c(
            json!({"tool": "delete_file", "arguments": {"path": "/data/home/a"}, "decision_id": "dc-000000000001"})
        ));
        assert!(!JunoBashTool.needs_confirmation(&json!({"command": "ls"})));
        assert!(!JunoScreenshotTool.needs_confirmation(&json!({})));
    }

    #[test]
    fn juno_off_limits_are_named() {
        let off = |tool: &str, a: Value| off_limits(tool, &a).is_some();
        for cmd in [
            "dd if=/dev/zero of=/dev/ufs0lun3",
            "cat /dev/ufs0lun1 > x",
            "ls /dev/disk/by-path/platform-ufs-lun4",
            "cp x /lib/firmware/qcom/sc7180/modem.mbn",
            "dd if=x of=/dev/disk/by-partlabel/modem",
            "cat /dev/disk/by-partlabel/fsg",
            "dd if=/dev/disk/by-partlabel/modemst1 of=m",
            "echo 500000 > /sys/class/power_supply/battery/constant_charge_current",
            "echo 1 > /sys/class/power_supply/usb/input_current_limit",
            "sudo lucina-charge set 80",
            "lucina-charge limit 60",
            "x && lucina-charge limit_current 1",
        ] {
            assert!(off("bash", json!({"command": cmd})), "{cmd}");
        }
        assert!(off(
            "write_file",
            json!({"path": "/lib/firmware/qcom/a", "text": "x"})
        ));
        assert!(off(
            "delete_file",
            json!({"path": "/vendor/firmware/wlan/bdwlan.bin"})
        ));
        for cmd in [
            "ls /data/home",
            "cat /dev/ufs0lun0 | head -c 1",
            "lucina-charge status",
            "ls /dev/ufs0lun10",
            "echo modem is fine",
            "grep fsgx log",
        ] {
            assert!(!off("bash", json!({"command": cmd})), "{cmd}");
        }
        // Reading a limit is not a write.
        assert!(!off(
            "read_file",
            json!({"path": "/sys/class/power_supply/battery/constant_charge_current"})
        ));
        assert!(!off("status", json!({})));
    }

    #[test]
    fn juno_risky_steps_are_classified() {
        let risky_ = |tool: &str, a: Value| risky(tool, &a).is_some();
        for tool in [
            "generation_install",
            "reload",
            "reboot",
            "poweroff",
            "wipe",
            "wipe_data",
            "format_disk",
        ] {
            assert!(risky_(tool, json!({})), "{tool}");
        }
        for cmd in [
            "rm -rf /boot/loader",
            "cp k /boot/efi/EFI/x",
            "mount /dev/disk/by-partlabel/esp /mnt",
            "rm -rf /data/lucina/apps/gallery",
            "dd if=img of=/dev/ufs0lun0",
            "dd if=a.img of=b.img",
            "sudo lucina-install-disk",
            "lucina-data clean",
            "mkfs.ext4 /dev/sdb1",
            "echo quiet > /etc/kernel/cmdline",
            "rm /etc/hosts",
            "cd /tmp && sudo rm -r /var/lib/x",
            "rm ../../etc/x",
        ] {
            assert!(risky_("bash", json!({"command": cmd})), "{cmd}");
        }
        for cmd in [
            "ls -la",
            "rm /data/home/a.txt",
            "rm -rf ~/tmp",
            "rm notes.txt",
            "cat /proc/meminfo",
            "cp a /tmp/b",
        ] {
            assert!(!risky_("bash", json!({"command": cmd})), "{cmd}");
        }
        assert!(risky_("delete_file", json!({"path": "/etc/passwd"})));
        assert!(risky_(
            "delete_file",
            json!({"path": "/data/home/../lucina/x"})
        ));
        assert!(risky_("delete_file", json!({"path": "/data/home"})));
        assert!(!risky_(
            "delete_file",
            json!({"path": "/data/home/Downloads/a.pdf"})
        ));
        assert!(!risky_("delete_file", json!({"path": "Downloads/a.pdf"})));
        assert!(risky_(
            "write_file",
            json!({"path": "/boot/loader/entries/x.conf", "text": "x"})
        ));
        assert!(!risky_(
            "write_file",
            json!({"path": "/data/home/a", "text": "x"})
        ));
        // Other tools are not read for paths: their own class comes from the device.
        assert!(!risky_("open_app", json!({"id": "boot"})));
        assert_eq!(
            classify("bash", &json!({"command": "dd if=x of=/dev/ufs0lun2"})),
            Class::OffLimits("the laptop's boot and firmware disks (UFS LUN 1-5)")
        );
        assert_eq!(classify("status", &json!({})), Class::Plain);
    }

    #[test]
    fn juno_effects_are_ordered() {
        use CallEffect::*;
        let order = [Read, Write, Destructive, External, OwnerOnly];
        assert!(order.windows(2).all(|w| w[0] < w[1]));
        for e in order {
            assert_eq!(CallEffect::parse(e.as_str()), Some(e));
        }
        assert_eq!(CallEffect::parse("other"), None);
        assert_eq!(Write.max(Class::Risky("x").effect()), Destructive);
        assert_eq!(External.max(Class::Risky("x").effect()), External);
        assert_eq!(Read.max(Class::OffLimits("x").effect()), OwnerOnly);
        assert_eq!(Class::Plain.effect(), Read);
    }

    #[test]
    fn juno_call_hash_is_canonical() {
        // Keys sorted whatever the order they were written in.
        let a = call_hash("bash", &json!({"timeoutSecs": 5, "command": "ls"}));
        let b = call_hash("bash", &json!({"command": "ls", "timeoutSecs": 5}));
        assert_eq!(a, b);
        assert_eq!(
            serde_json::to_string(
                &json!({"tool": "bash", "args": {"timeoutSecs": 5, "command": "ls"}})
            )
            .unwrap(),
            r#"{"args":{"command":"ls","timeoutSecs":5},"tool":"bash"}"#
        );
        assert_eq!(
            a,
            "50ea942416a0a5ec43f1df6d30cb42aef59c468e61663d2e59459187894680fe"
        );
        assert_eq!(
            intent_hash("play", &json!({"path": "a.mp4"})),
            "3a711ec903a04fd76612815444c52005a6483a816bb3d2c65397dfc2181a8a4f"
        );
    }

    #[tokio::test]
    async fn juno_off_limits_are_never_sent_even_with_a_decision() {
        let f = fake(json!({"text": "ok"}));
        let args = json!({"command": "dd if=x of=/dev/ufs0lun1"});
        let hash = call_hash(
            "bash",
            &json!({"command": "dd if=x of=/dev/ufs0lun1", "timeoutSecs": 110}),
        );
        f.go.lock().unwrap().push(("dc-000000000001".into(), hash));
        let c = ctx(&f, "secretary_juno-dev1");
        let e = JunoBashTool.call(args.clone(), &c).await.unwrap_err();
        assert!(e.0.contains("off limits"), "{e}");
        let mut with = args;
        with["decision_id"] = json!("dc-000000000001");
        assert!(JunoBashTool.call(with, &c).await.is_err());
        let e = JunoCallTool
            .call(json!({"tool": "write_file", "arguments": {"path": "/lib/firmware/qcom/x", "text": "y"}}), &c)
            .await
            .unwrap_err();
        assert!(e.0.contains("off limits"), "{e}");
        assert!(f.calls.lock().unwrap().is_empty());
        assert!(f.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn juno_risky_call_waits_for_a_go_on_exactly_that_call() {
        let f = fake(json!({"text": "rebooting"}));
        let c = ctx(&f, "secretary_juno-dev1");
        let out = JunoCallTool
            .call(json!({"tool": "reboot"}), &c)
            .await
            .unwrap();
        let t = out["text"].as_str().unwrap();
        assert!(t.starts_with("waiting on decision dc-000000000001"), "{t}");
        assert!(
            f.calls.lock().unwrap().is_empty(),
            "nothing sent before the Go"
        );
        let ask = f.asked.lock().unwrap()[0].clone();
        assert_eq!(ask.summary, "Juno: reboot");
        assert_eq!(ask.hash, call_hash("reboot", &json!({})));
        assert!(ask.context.contains("cannot be undone"));
        // Not decided: refused.
        let again = json!({"tool": "reboot", "decision_id": "dc-000000000001"});
        assert!(JunoCallTool.call(again.clone(), &c).await.is_err());
        // Decided Go for another call: refused.
        f.go.lock()
            .unwrap()
            .push(("dc-000000000001".into(), call_hash("poweroff", &json!({}))));
        assert!(JunoCallTool.call(again.clone(), &c).await.is_err());
        assert!(f.calls.lock().unwrap().is_empty());
        // Go for this call: sent, with the decision.
        f.go.lock()
            .unwrap()
            .push(("dc-000000000001".into(), ask.hash.clone()));
        let out = JunoCallTool.call(again, &c).await.unwrap();
        assert_eq!(out["text"], "rebooting");
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls[0].1, "reboot");
        assert_eq!(calls[0].4.decision_id.as_deref(), Some("dc-000000000001"));
        assert!(!calls[0].4.internal);
        // A risky call never asks the chat confirmation too.
        assert!(!JunoCallTool
            .needs_confirmation(&json!({"tool": "delete_file", "arguments": {"path": "/etc/x"}})));
    }

    #[tokio::test]
    async fn juno_bash_risky_command_waits_and_runs_with_its_decision() {
        let f = fake(json!({"text": "formatted"}));
        let c = ctx(&f, "s");
        let cmd = json!({"command": "mkfs.ext4 /dev/sdb1", "timeout_seconds": 30});
        let out = JunoBashTool.call(cmd.clone(), &c).await.unwrap();
        assert!(out["text"]
            .as_str()
            .unwrap()
            .starts_with("waiting on decision"));
        let hash = f.asked.lock().unwrap()[0].hash.clone();
        assert_eq!(
            hash,
            call_hash(
                "bash",
                &json!({"command": "mkfs.ext4 /dev/sdb1", "timeoutSecs": 30})
            )
        );
        f.go.lock().unwrap().push(("dc-000000000001".into(), hash));
        let mut with = cmd;
        with["decision_id"] = json!("dc-000000000001");
        assert_eq!(
            JunoBashTool.call(with, &c).await.unwrap()["text"],
            "formatted"
        );
        assert_eq!(f.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn juno_device_needs_decision_is_filed_with_its_hash() {
        let f = fake(json!({"needs_decision": true, "effect": "external", "call_hash": "abc123"}));
        let c = ctx(&f, "secretary_juno-dev1");
        let call = json!({"tool": "intent", "arguments": {"name": "send_mail", "args": {}}});
        let out = JunoCallTool.call(call.clone(), &c).await.unwrap();
        assert!(out["text"]
            .as_str()
            .unwrap()
            .starts_with("waiting on decision dc-000000000001"));
        let ask = f.asked.lock().unwrap()[0].clone();
        assert_eq!(ask.device_hash.as_deref(), Some("abc123"));
        assert_eq!(
            ask.hash,
            call_hash("intent", &json!({"name": "send_mail", "args": {}}))
        );
        // Again with the decision: sent again with the id.
        f.go.lock()
            .unwrap()
            .push(("dc-000000000001".into(), ask.hash));
        *f.reply.lock().unwrap() = json!({"text": "sent"});
        let mut with = call;
        with["decision_id"] = json!("dc-000000000001");
        assert_eq!(JunoCallTool.call(with, &c).await.unwrap()["text"], "sent");
        assert_eq!(
            f.calls.lock().unwrap()[1].4.decision_id.as_deref(),
            Some("dc-000000000001")
        );
        // owner_only: never filed, never run.
        let f = fake(json!({"needs_decision": true, "effect": "owner_only", "call_hash": "x"}));
        let e = JunoCallTool
            .call(json!({"tool": "pay"}), &ctx(&f, "s"))
            .await
            .unwrap_err();
        assert!(e.0.contains("owner_only"), "{e}");
        assert!(f.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn juno_call_cannot_set_internal() {
        let f = fake(json!({"text": "ok"}));
        JunoCallTool
            .call(json!({"tool": "secret_source", "arguments": {"path": "/data/home/k", "internal": true}}), &ctx(&f, "s"))
            .await
            .unwrap();
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls[0].2, json!({"path": "/data/home/k"}));
        assert!(!calls[0].4.internal);
    }

    #[tokio::test]
    async fn juno_once_refused_tools_now_go_out() {
        let f = fake(json!({"text": "ok"}));
        for tool in ["install", "shutdown", "disk", "disk_usage", "diskpart"] {
            let out = JunoCallTool
                .call(json!({"tool": tool}), &ctx(&f, "s"))
                .await
                .unwrap();
            assert_eq!(out["text"], "ok", "{tool}");
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
        assert_eq!(f.calls.lock().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn juno_events_reads_and_redacts() {
        let f = fake(json!({}));
        let out = JunoEventsTool
            .call(json!({"kind": "media", "max": 500}), &ctx(&f, "s"))
            .await
            .unwrap();
        assert_eq!(out["events"][0]["kind"], "media");
        assert!(!out.to_string().contains("482913"));
        let mcp = ctx(&f, "s").with_origin(Origin::Mcp);
        assert!(JunoEventsTool.call(json!({}), &mcp).await.is_err());
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
        assert_eq!(
            out["text"],
            "click, type_text, status\n4 new events from the laptop since the last look (juno_events reads them)."
        );
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
