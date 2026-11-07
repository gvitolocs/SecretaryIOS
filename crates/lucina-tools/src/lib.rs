//! Lucina's tool plugins.
//!
//! - [`Tool`]: name, description, JSON Schema of its arguments (schemars),
//!   [`SideEffect`], `confirm`, and `async fn call(args, &Ctx)`.
//! - [`Registry`]: the enabled tools (`LUCINA_TOOLS=all|name,name`), argument
//!   validation against each schema (`jsonschema`), and the **hard deny
//!   list**: no tool that buys anything or moves money can be registered or
//!   called.
//! - [`agent`]: the tool loop (at most 6 model rounds, a total deadline, tool
//!   errors returned to the model).
//! - [`pending`]: the confirmation flow. External or `confirm` tools never
//!   run in the turn that asked for them: a pending action (id, 10-minute
//!   expiry) is stored and runs only after the owner's explicit yes in a
//!   later turn.
//! - [`tools`]: `time`, `weather`, `calculator`, `unit_convert`, `places`,
//!   `au_people_search`, `au_building`, `au_campus_map` (AU Find, Aarhus
//!   University, through the local af-server; [`tools::au`]), `caller_screen`
//!   (unknown callers judged by Jev; [`tools::callers`]), `notes`, `reminders`, `calendar`, `tasks`, `memos` (voice memos), `shopping`, `presence`, `avatar_outfit`, `notify`,
//!   the owner-only `secrets_status` / `secrets_audit` ([`OWNER_ONLY_TOOLS`]),
//!   the delegation tools `ask_claude`, `ask_grok`, `ask_chatgpt`, `delegate`
//!   ([`SideEffect::Delegate`]: Lucina's own decision, no confirmation); none of
//!   these is offered on Lucina MCP turns ([`NO_MCP_TOOLS`]); `meshy` (3D
//!   models, registered by the server when `LUCINA_MESHY=true`); the owner-only plugin tools `instagram_profile`,
//!   `instagram_stories`, the watchlist tools, `pixiv_search`, `pixiv_ranking`, `save_to_gallery` ([`tools::plugins`]; safe Pixiv only); `drive`
//!   (Lucina's own Google Drive, read-only, registered by the server when
//!   `LUCINA_DRIVE=true` and the grant exists, see [`google`]).
//!   `juno_tools`, `juno_call`, `juno_screenshot`, `juno_bash` (the owner's laptop over
//!   the server's Juno bridge, owner's registry only; [`tools::juno`]).
//! - Secrets: a tool reads a secret by setting name through [`Ctx::secret`]
//!   (`weather.api_key` -> `secret://lucina/prod/lucina#OPEN_METEO_API_KEY`),
//!   uses the value for its own outbound call only, and every tool result is
//!   redacted before anything else sees it ([`agent::execute`]).

pub mod agent;
pub mod google;
pub mod pending;
pub mod reminder_writer;
pub mod shopping_gate;
pub mod tools;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use lucina_grocery::GroceryRepo;
use lucina_store::{Reminder, ReminderStore};
use serde_json::Value;

/// What running a tool can affect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffect {
    /// Reads only (including public web APIs).
    None,
    /// Writes Lucina's own data (notes, reminders, shopping list).
    Local,
    /// Acts in the outside world (messages, posts, ...): always confirmed.
    External,
    /// Acts on the owner's devices.
    Device,
    /// Hands work to another AI agent (Claude, Grok, ChatGPT). Not
    /// confirmed by the owner: Lucina's own self-review decides, and the
    /// hard limits of `lucina-delegate` hold (no secrets out, no money,
    /// depth 1, never from Lucina MCP, code only in a sandbox).
    Delegate,
}

impl SideEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            SideEffect::None => "none",
            SideEffect::Local => "local",
            SideEffect::External => "external",
            SideEffect::Device => "device",
            SideEffect::Delegate => "delegate",
        }
    }
}

/// A tool failure, returned to the model as `{"error": ...}`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ToolError(pub String);

impl ToolError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

pub type ToolResult = Result<Value, ToolError>;

/// Something a tool did that the caller reports (chat `action`, timeline meta).
#[derive(Debug, Clone)]
pub enum Effect {
    ReminderCreated {
        reminder: Reminder,
        created: bool,
    },
    RemindersListed,
    ReminderCancelled {
        reminder: Reminder,
    },
    Shopping {
        list_id: String,
    },
    /// Lucina saved, read-modified or updated a note; `slug` is set when it lives on
    /// secretary-server (the chat item then links to it: `meta.kind = "note"`).
    Note {
        title: String,
        slug: Option<String>,
    },
    /// Lucina changed her look: the outfit and/or hairstyle she put on
    /// (both `None` = back to automatic).
    AvatarOutfit {
        outfit: Option<String>,
        hair: Option<String>,
    },
    /// Photos to show: the reply gets a button that opens this path in the app (the Gallery,
    /// `/films…` for films, `/drive…` for Drive documents).
    Gallery {
        path: String,
        count: usize,
    },
    /// Calendar events the tool returned (secretary-server shape, RFC 3339
    /// times): the reply gets a map card for those it names that have a location.
    CalendarEvents {
        events: Vec<Value>,
    },
    /// An image for the reply (an `image` block: the AU campus map PNG).
    Image {
        src: String,
        alt: String,
        caption: String,
    },
    /// The turn used an owner-only plugin tool (Instagram, Pixiv): the server
    /// keeps the exchange out of Honcho (the long-term memory).
    PrivateTurn,
    /// `calendar` created an event directly for the owner (secretary-server
    /// shape): the reply gets an Undo button for its first 10 minutes.
    CalendarCreated {
        event: Value,
    },
}

/// External endpoints (overridable for tests).
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub open_meteo_forecast: String,
    pub open_meteo_geocoding: String,
    pub nominatim: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            open_meteo_forecast: "https://api.open-meteo.com".into(),
            open_meteo_geocoding: "https://geocoding-api.open-meteo.com".into(),
            nominatim: "https://nominatim.openstreetmap.org".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Location {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

impl Default for Location {
    fn default() -> Self {
        Self {
            name: "Copenhagen, Denmark".into(),
            latitude: 55.6761,
            longitude: 12.5683,
        }
    }
}

/// At most one request per `interval` (Nominatim's usage policy: 1 req/s).
#[derive(Debug)]
pub struct RateGate {
    interval: Duration,
    last: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

impl RateGate {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: tokio::sync::Mutex::new(None),
        }
    }

    pub async fn wait(&self) {
        let mut last = self.last.lock().await;
        if let Some(t) = *last {
            let next = t + self.interval;
            if next > tokio::time::Instant::now() {
                tokio::time::sleep_until(next).await;
            }
        }
        *last = Some(tokio::time::Instant::now());
    }
}

/// Shared services and settings for tools.
pub struct ToolEnv {
    pub http: reqwest::Client,
    pub time_zone: Tz,
    pub data_dir: PathBuf,
    /// Read side of `reminders.json`.
    pub reminders: Option<Arc<ReminderStore>>,
    /// Write side (Node forwarder in phase 1, or the local file).
    pub reminder_writer: Option<Arc<dyn reminder_writer::ReminderWriter>>,
    pub reminders_enabled: bool,
    pub reminder_default_hour: u32,
    pub reminder_default_minute: u32,
    pub owner_chat_id: Option<String>,
    pub owner_peer_id: String,
    pub user_label: String,
    pub persona: String,
    pub grocery: Option<Arc<GroceryRepo>>,
    pub grocery_user_id: String,
    pub endpoints: Endpoints,
    pub default_location: Location,
    pub nominatim_gate: RateGate,
    pub user_agent: String,
    pub nominatim_email: Option<String>,
    /// `<data>/presence` (the Discord presence watcher's files).
    pub presence_dir: PathBuf,
    /// Older presence files mean the watcher is down.
    pub presence_max_age: Duration,
    /// secretary-server base URL (`avatar_outfit`, `calendar`, `notify`, `notes`).
    pub secretary_url: Option<String>,
    /// File holding the secretary-server service token (`svc_…`, read per call).
    pub secretary_service_token_file: Option<PathBuf>,
    /// Infisical (`LUCINA_SECRETS_CONFIG`): secrets by reference, for tools.
    pub secrets: Option<Arc<lucina_secrets::SecretStore>>,
    /// Delegation to Claude, Grok and ChatGPT (`LUCINA_DELEGATE`).
    pub delegator: Option<Arc<lucina_delegate::Delegator>>,
    /// lucina-memo (`memos`): base URL and the file with its service token.
    pub memo_url: Option<String>,
    pub memo_token_file: Option<PathBuf>,
    /// lucina-diagnostics (`diagnostics`): base URL and the file with its service token.
    pub diagnostics_url: Option<String>,
    pub diagnostics_token_file: Option<PathBuf>,
    /// Lucina's own Google Drive (`LUCINA_DRIVE`).
    pub drive: Option<Arc<google::Drive>>,
    /// The owner's Instagram and Pixiv plugin services on this host.
    pub plugins: tools::plugins::Plugins,
    /// Owner approvals (pinned grant key present): the `root_exec` tool.
    pub approvals: Option<Arc<lucina_approvals::Approvals>>,
    /// The ops record (`ops`): deploys, incidents, decisions of every project.
    pub ops: Option<Arc<lucina_ops::Ops>>,
    /// The flaw tracker (`flaw`, `message_claude` with `issue`).
    pub flaws: Option<Arc<lucina_flaws::Flaws>>,
    /// Messages to Claude, Grok and Lucina Agents (`LUCINA_REACH`); `None`: off.
    pub reach: Option<Arc<tools::reach::Reach>>,
    /// af-server (AU Find) base URL (`LUCINA_AU_URL`); `None`: `LUCINA_AU=off`.
    pub au_url: Option<String>,
    /// Numbers the phones reported and the answers `caller_screen` gave; `None`: no store.
    pub callers: Option<Arc<tools::callers::CallerStore>>,
    /// Public caller-ID pages (`LUCINA_CALLERS_PUBLIC_URL`, default 180.dk); `None`: off.
    pub callers_public_url: Option<String>,
    /// Juno, the owner's laptop (`juno_*` tools, lucina-server's `JunoBridge`); `None`: off.
    pub juno: Option<Arc<dyn tools::juno::JunoLink>>,
}

impl ToolEnv {
    /// Defaults for tests and tools that need little.
    pub fn new(data_dir: impl Into<PathBuf>, time_zone: Tz) -> Self {
        let user_agent = concat!(
            "lucina-rs/",
            env!("CARGO_PKG_VERSION"),
            " (private personal assistant)"
        )
        .to_owned();
        let data_dir: PathBuf = data_dir.into();
        Self {
            http: reqwest::Client::builder()
                .user_agent(user_agent.clone())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(15))
                .build()
                .expect("http client"),
            time_zone,
            presence_dir: data_dir.join("presence"),
            presence_max_age: lucina_discord::store::DEFAULT_MAX_AGE,
            data_dir,
            reminders: None,
            reminder_writer: None,
            reminders_enabled: true,
            reminder_default_hour: 9,
            reminder_default_minute: 0,
            owner_chat_id: None,
            owner_peer_id: "giuseppe".into(),
            user_label: "Giuseppe (operator, Secretary app)".into(),
            persona: "Lucina".into(),
            grocery: None,
            grocery_user_id: "giuseppe".into(),
            endpoints: Endpoints::default(),
            default_location: Location::default(),
            nominatim_gate: RateGate::new(Duration::from_millis(1000)),
            user_agent,
            nominatim_email: None,
            secretary_url: None,
            secretary_service_token_file: None,
            secrets: None,
            delegator: None,
            memo_url: None,
            memo_token_file: None,
            diagnostics_url: None,
            diagnostics_token_file: None,
            drive: None,
            plugins: tools::plugins::Plugins::default(),
            approvals: None,
            ops: None,
            flaws: None,
            reach: None,
            au_url: None,
            callers: None,
            callers_public_url: None,
            juno: None,
        }
    }
}

/// Who started a turn. A Lucina MCP turn (another assistant asking through
/// secretary-mcp) never confirms or creates a pending action: tools that
/// need the owner's yes are refused there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Origin {
    #[default]
    Owner,
    Mcp,
}

/// Per-turn context.
pub struct Ctx {
    pub env: Arc<ToolEnv>,
    /// `web` | `ios` (recorded as the shopping item's `updatedBy`).
    pub channel: String,
    pub session_id: String,
    pub now: DateTime<Utc>,
    pub origin: Origin,
    /// True only while running a pending action the owner confirmed.
    pub confirmed: bool,
    /// The owner's own message started this turn (his app, his DMs, his voice): not an MCP
    /// client, an agent or Lucina's own initiative. Calendar creates go straight to Google
    /// only then; otherwise they become proposals in Links. Off unless the caller says so.
    pub owner_turn: bool,
    effects: Mutex<Vec<Effect>>,
}

impl Ctx {
    pub fn new(env: Arc<ToolEnv>, channel: &str, session_id: &str) -> Self {
        Self {
            env,
            channel: channel.into(),
            session_id: session_id.into(),
            now: Utc::now(),
            origin: Origin::Owner,
            confirmed: false,
            owner_turn: false,
            effects: Mutex::new(Vec::new()),
        }
    }

    /// Running an action the owner confirmed.
    pub fn confirmed(mut self) -> Self {
        self.confirmed = true;
        self
    }

    /// The turn is the owner's own message (see [`Ctx::owner_turn`]).
    pub fn owner_turn(mut self) -> Self {
        self.owner_turn = true;
        self
    }

    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    pub fn push_effect(&self, e: Effect) {
        self.effects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(e);
    }

    /// The secret behind a tool setting (`weather.api_key`), for this tool's
    /// outbound call only. `Ok(None)`: no secret is configured for it.
    pub async fn secret(
        &self,
        setting: &str,
        tool: &str,
    ) -> Result<Option<lucina_secrets::SecretValue>, ToolError> {
        let Some(store) = &self.env.secrets else {
            return Ok(None);
        };
        store
            .resolve_setting(setting, tool)
            .await
            .map_err(|e| ToolError::new(format!("secret for {setting} unavailable: {e}")))
    }

    pub fn effects(&self) -> Vec<Effect> {
        self.effects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// A tool plugin.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// JSON Schema (an object schema) for the arguments.
    fn parameters(&self) -> Value;
    fn side_effect(&self) -> SideEffect;
    /// Always ask the owner first.
    fn confirm(&self) -> bool {
        false
    }
    /// Per call: does this call need the owner's yes? External tools and
    /// `confirm` tools always do; a tool may ask for specific actions.
    fn needs_confirmation(&self, _args: &Value) -> bool {
        self.confirm() || self.side_effect() == SideEffect::External
    }
    /// Longest one call may run. Cutting a call off does not undo what it started on another
    /// machine, so a tool whose work takes minutes says so.
    fn time_limit(&self, _args: &Value) -> std::time::Duration {
        agent::PER_TOOL_TIMEOUT
    }
    /// Declared money movement: such a tool is refused by the registry.
    fn moves_money(&self) -> bool {
        false
    }
    /// One line for confirmations and the timeline (no private payloads).
    fn summarize(&self, args: &Value) -> String {
        let mut s = args.to_string();
        if s.chars().count() > 160 {
            s = s.chars().take(157).collect::<String>() + "...";
        }
        format!("{} {s}", self.name())
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult;
}

/// JSON Schema of `T`, inlined (no `$ref`), without `$schema`/`title`.
pub fn schema_for<T: schemars::JsonSchema>() -> Value {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator();
    let schema = generator.into_root_schema_for::<T>();
    let mut v = serde_json::to_value(schema).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.entry("type").or_insert(Value::String("object".into()));
    }
    v
}

/// Words that mark a purchase or money movement. Matched against the
/// tokens of a tool name (`send_money` -> `send`, `money`).
pub const DENY_TERMS: &[&str] = &[
    "pay", "payment", "payments", "paypal", "buy", "checkout", "order", "orders", "transfer",
    "wire", "withdraw", "deposit", "money", "trade", "trading", "invest", "crypto", "wallet",
    "bank", "iban", "card", "stripe", "invoice",
];

pub fn is_denied_name(name: &str) -> bool {
    name.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|t| DENY_TERMS.contains(&t))
        || name_words(name)
            .iter()
            .any(|t| DENY_TERMS.contains(&t.as_str()))
}

/// The words of a tool name, lower-cased: split at non-alphanumerics, at
/// lower-to-upper case changes and before the last capital of an acronym
/// (`sendMoney` -> `send`, `money`; `IBANTransfer` -> `iban`, `transfer`),
/// and between letters and digits.
pub fn name_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(&p) = i.checked_sub(1).and_then(|j| chars.get(j)) {
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            let boundary = (c.is_ascii_uppercase()
                && (p.is_ascii_lowercase() || p.is_ascii_digit()))
                || (c.is_ascii_uppercase() && p.is_ascii_uppercase() && next_lower)
                || (c.is_ascii_digit() != p.is_ascii_digit() && p.is_ascii_alphanumeric());
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Tools for the owner only: never registered on Lucina MCP turns.
pub const OWNER_ONLY_TOOLS: &[&str] = &[
    "secrets_status",
    "secrets_audit",
    // A flaw report can start a coding agent.
    "flaw",
    // The Instagram / Pixiv plugins (`tools::plugins::NAMES`): owner's chat only.
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
    // Messages to Claude, Grok and Lucina Agents (`tools::reach::REACH_TOOLS`).
    "message_claude",
    "message_grok",
    "message_mistral",
    "lucina_agents",
    "reach_threads",
];

/// Every tool a Lucina MCP turn never gets: the owner-only tools and the
/// delegation tools (outside agents calling Lucina must not drive her).
pub const NO_MCP_TOOLS: &[&str] = &[
    "secrets_status",
    "secrets_audit",
    "flaw",
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
    "ask_claude",
    "ask_grok",
    "ask_chatgpt",
    "delegate",
    "message_claude",
    "message_grok",
    "message_mistral",
    "lucina_agents",
    "reach_threads",
    // Root commands (owner approval): never for a guarded MCP caller.
    "root_exec",
];

/// Which tools are enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSelection {
    All,
    Only(HashSet<String>),
}

impl ToolSelection {
    /// `all` (or empty) | `none` | `name,name`.
    pub fn parse(s: &str) -> Self {
        let t = s.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("all") {
            return ToolSelection::All;
        }
        if t.eq_ignore_ascii_case("none") {
            return ToolSelection::Only(HashSet::new());
        }
        ToolSelection::Only(
            t.split(',')
                .map(|n| n.trim().to_ascii_lowercase())
                .filter(|n| !n.is_empty())
                .collect(),
        )
    }

    pub fn allows(&self, name: &str) -> bool {
        match self {
            ToolSelection::All => true,
            ToolSelection::Only(set) => set.contains(name),
        }
    }
}

#[derive(Default)]
pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
    validators: HashMap<String, jsonschema::Validator>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.tools.iter().map(|t| t.name()))
            .finish()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Refused: duplicate names, deny-listed names or
    /// declared money movement, and schemas that do not compile.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), String> {
        let name = tool.name();
        if is_denied_name(name) || tool.moves_money() {
            return Err(format!(
                "tool {name} refused: no purchases and no money movement"
            ));
        }
        if self.validators.contains_key(name) {
            return Err(format!("tool {name} registered twice"));
        }
        let validator = jsonschema::validator_for(&tool.parameters())
            .map_err(|e| format!("tool {name}: bad schema: {e}"))?;
        self.validators.insert(name.to_owned(), validator);
        self.tools.push(tool);
        Ok(())
    }

    /// The built-in tools allowed by `selection`.
    pub fn builtin(selection: &ToolSelection) -> Self {
        let mut r = Self::new();
        for tool in tools::builtin() {
            if selection.allows(tool.name()) {
                r.register(tool).expect("built-in tool registers");
            }
        }
        r
    }

    /// The same registry without `names` (MCP turns: [`OWNER_ONLY_TOOLS`]).
    pub fn without(&self, names: &[&str]) -> Self {
        let mut r = Self::new();
        for t in &self.tools {
            if !names.contains(&t.name()) {
                r.register(t.clone()).expect("tool registered before");
            }
        }
        r
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Function specs for the model.
    pub fn specs(&self) -> Vec<lucina_llm::Tool> {
        self.tools
            .iter()
            .map(|t| lucina_llm::Tool::function(t.name(), t.description(), t.parameters()))
            .collect()
    }

    /// Validate `args` against the tool's schema (first 3 errors).
    pub fn validate(&self, name: &str, args: &Value) -> Result<(), String> {
        let v = self
            .validators
            .get(name)
            .ok_or_else(|| format!("unknown tool {name}"))?;
        let errors: Vec<String> = v
            .iter_errors(args)
            .take(3)
            .map(|e| format!("{e} at {}", e.instance_path()))
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("invalid arguments: {}", errors.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Payer;
    #[async_trait::async_trait]
    impl Tool for Payer {
        fn name(&self) -> &'static str {
            "send_money"
        }
        fn description(&self) -> &'static str {
            "x"
        }
        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        fn side_effect(&self) -> SideEffect {
            SideEffect::External
        }
        async fn call(&self, _: Value, _: &Ctx) -> ToolResult {
            unreachable!()
        }
    }

    #[test]
    fn deny_list_and_selection() {
        assert!(is_denied_name("send_money"));
        assert!(is_denied_name("buy-ticket"));
        assert!(!is_denied_name("shopping"));
        assert!(!is_denied_name("reminders"));
        let mut r = Registry::new();
        assert!(r
            .register(Arc::new(Payer))
            .unwrap_err()
            .contains("no money movement"));
        let all = Registry::builtin(&ToolSelection::All);
        assert_eq!(all.names().len(), 41);
        assert_eq!(all.without(OWNER_ONLY_TOOLS).names().len(), 24);
        let mcp = all.without(NO_MCP_TOOLS);
        assert_eq!(mcp.names().len(), 20);
        for n in tools::reach::REACH_TOOLS {
            assert!(OWNER_ONLY_TOOLS.contains(n) && NO_MCP_TOOLS.contains(n), "{n}");
            assert!(all.get(n).is_some() && mcp.get(n).is_none(), "{n}");
        }
        assert!(mcp.get("au_people_search").is_some());
        for n in tools::plugins::NAMES {
            assert!(OWNER_ONLY_TOOLS.contains(n) && all.get(n).is_some(), "{n}");
            assert!(mcp.get(n).is_none(), "{n}");
        }
        assert!(mcp.get("memos").is_some());
        assert!(mcp.get("diagnostics").is_some());
        for t in tools::delegate::DELEGATION_TOOLS
            .iter()
            .chain(OWNER_ONLY_TOOLS)
        {
            assert!(NO_MCP_TOOLS.contains(t), "{t}");
        }
        assert!(mcp.get("secrets_status").is_none() && mcp.get("secrets_audit").is_none());
        let some = Registry::builtin(&ToolSelection::parse("time, calculator"));
        assert_eq!(some.names(), vec!["time", "calculator"]);
        assert!(Registry::builtin(&ToolSelection::parse("none")).is_empty());
        // every built-in schema is an object schema without $ref
        for spec in all.specs() {
            let p = spec.function.parameters.to_string();
            assert!(p.contains("\"type\":\"object\""), "{}", spec.function.name);
            assert!(!p.contains("$ref"), "{}: {p}", spec.function.name);
        }
    }
}
