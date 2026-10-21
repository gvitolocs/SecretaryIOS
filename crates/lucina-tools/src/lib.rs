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
//!   `notes`, `reminders`, `shopping`.

pub mod agent;
pub mod pending;
pub mod reminder_writer;
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
}

impl SideEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            SideEffect::None => "none",
            SideEffect::Local => "local",
            SideEffect::External => "external",
            SideEffect::Device => "device",
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
    ReminderCreated { reminder: Reminder, created: bool },
    RemindersListed,
    ReminderCancelled { reminder: Reminder },
    Shopping { list_id: String },
    Note { title: String },
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
        Self { name: "Copenhagen, Denmark".into(), latitude: 55.6761, longitude: 12.5683 }
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
        Self { interval, last: tokio::sync::Mutex::new(None) }
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
}

impl ToolEnv {
    /// Defaults for tests and tools that need little.
    pub fn new(data_dir: impl Into<PathBuf>, time_zone: Tz) -> Self {
        let user_agent = concat!("lucina-rs/", env!("CARGO_PKG_VERSION"), " (private personal assistant)").to_owned();
        Self {
            http: reqwest::Client::builder()
                .user_agent(user_agent.clone())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(15))
                .build()
                .expect("http client"),
            time_zone,
            data_dir: data_dir.into(),
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
        }
    }
}

/// Per-turn context.
pub struct Ctx {
    pub env: Arc<ToolEnv>,
    /// `web` | `ios` (recorded as the shopping item's `updatedBy`).
    pub channel: String,
    pub session_id: String,
    pub now: DateTime<Utc>,
    effects: Mutex<Vec<Effect>>,
}

impl Ctx {
    pub fn new(env: Arc<ToolEnv>, channel: &str, session_id: &str) -> Self {
        Self { env, channel: channel.into(), session_id: session_id.into(), now: Utc::now(), effects: Mutex::new(Vec::new()) }
    }

    pub fn push_effect(&self, e: Effect) {
        self.effects.lock().unwrap_or_else(|e| e.into_inner()).push(e);
    }

    pub fn effects(&self) -> Vec<Effect> {
        self.effects.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
    "pay", "payment", "payments", "paypal", "buy", "checkout", "order", "orders", "transfer", "wire", "withdraw", "deposit",
    "money", "trade", "trading", "invest", "crypto", "wallet", "bank", "iban", "card", "stripe", "invoice",
];

pub fn is_denied_name(name: &str) -> bool {
    name.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|t| DENY_TERMS.contains(&t))
}

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
        ToolSelection::Only(t.split(',').map(|n| n.trim().to_ascii_lowercase()).filter(|n| !n.is_empty()).collect())
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
        f.debug_list().entries(self.tools.iter().map(|t| t.name())).finish()
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
            return Err(format!("tool {name} refused: no purchases and no money movement"));
        }
        if self.validators.contains_key(name) {
            return Err(format!("tool {name} registered twice"));
        }
        let validator = jsonschema::validator_for(&tool.parameters()).map_err(|e| format!("tool {name}: bad schema: {e}"))?;
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
        let v = self.validators.get(name).ok_or_else(|| format!("unknown tool {name}"))?;
        let errors: Vec<String> = v.iter_errors(args).take(3).map(|e| format!("{e} at {}", e.instance_path())).collect();
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
        assert!(r.register(Arc::new(Payer)).unwrap_err().contains("no money movement"));
        let all = Registry::builtin(&ToolSelection::All);
        assert_eq!(all.names().len(), 8);
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
