//! `lucina`: the Rust Lucina.
//!
//! - `lucina serve` (default): the HTTP server (127.0.0.1:8790 by default).
//! - `lucina fake-deepseek --bind 127.0.0.1:8799 --script s.json [--record r.jsonl]`.
//! - `lucina check-data --data <dir>`: byte-compat round trip of the stores.
//! - `lucina presence-watch`: the owner's Discord presence watcher (its own
//!   gateway session, intents GUILDS | GUILD_PRESENCES only), writing
//!   `<data>/presence/discord.json{,l}`. Token from `--token-file`,
//!   `LUCINA_DISCORD_TOKEN_FILE` or `DISCORD_BOT_TOKEN_LUCINA`; owner from
//!   `LUCINA_OWNER_DISCORD_USER_ID`. Exit code 78 on a fatal gateway close
//!   (bad token, disallowed intents): systemd must not restart into an
//!   identify loop.
//! - `lucina deals list|cheapest <q>|lunch|map|digest|alerts [--dry-run|--send]`:
//!   the grocery deals checks of Node's `scripts/grocery-deals.mjs` (dry run
//!   unless `--send`; logs go to stderr).
//! - `lucina jev drift-report [--days 7] [--labels-dir DIR] [--dry-run] [--recalibrate]`:
//!   the weekly JEV-first drift report (docs/lucina-rust/JEV-ROUTING.md). Logs go
//!   to stderr; `--dry-run` prints the ops record and writes nothing.
//! - `lucina google-auth drive [--code <redirect URL or code>]`: Lucina's own
//!   Google Drive grant (read-only, `mylucina20@gmail.com` only).
//! - `lucina drive ls [--query Q] [--folder ID] [--search TEXT]`,
//!   `lucina drive get <ID or link> [--out PATH]`: her Drive from the shell.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::serve::ListenerExt as _;
use clap::{Parser, Subcommand};
use lucina_server::{app, AppState, Config};
use lucina_tools::google;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "lucina", version, about = "The Rust Lucina")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Args)]
struct ParityArgs {
    #[arg(long)]
    cases: PathBuf,
    #[arg(long)]
    node_url: String,
    #[arg(long)]
    rust_url: String,
    /// SECRETARY_API_TOKEN of the two instances (files).
    #[arg(long)]
    node_token_file: PathBuf,
    #[arg(long)]
    rust_token_file: PathBuf,
    /// The two fake DeepSeek record files.
    #[arg(long)]
    node_deepseek_record: PathBuf,
    #[arg(long)]
    rust_deepseek_record: PathBuf,
    /// The fake secretary-server record and each side's service token file.
    #[arg(long)]
    secretary_record: PathBuf,
    #[arg(long)]
    node_service_token_file: PathBuf,
    #[arg(long)]
    rust_service_token_file: PathBuf,
    /// Each side's timeline.jsonl (a copy).
    #[arg(long)]
    node_timeline: PathBuf,
    #[arg(long)]
    rust_timeline: PathBuf,
    /// Full JSON report.
    #[arg(long)]
    out: PathBuf,
    /// `name=value` placeholders for both sides (`{name}` in the cases).
    #[arg(long = "var")]
    vars: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve /health and /api/secretary/*.
    Serve,
    /// Bot Hospital: run the daily Poko-news audit once (`--dry-run`: only read
    /// and validate the proof; no incident, message or repair).
    BotHospital {
        #[arg(long)]
        dry_run: bool,
    },
    /// A scripted fake DeepSeek for parity runs.
    FakeDeepseek {
        #[arg(long, default_value = "127.0.0.1:8799")]
        bind: SocketAddr,
        #[arg(long)]
        script: Option<PathBuf>,
        #[arg(long)]
        record: Option<PathBuf>,
    },
    /// Parse and re-serialize timeline.jsonl and reminders.json under a data
    /// dir (read only) and report byte equality.
    CheckData {
        #[arg(long)]
        data: PathBuf,
    },
    /// Compare the deterministic parsers with the live Node ones on a corpus
    /// (runs the Node side read-only in the container with `docker exec`).
    ParityParsers {
        #[arg(long, default_value = "tools/parity/parser-corpus.json")]
        cases: PathBuf,
        #[arg(long, default_value = "tools/parity/node-parsers.mjs")]
        script: PathBuf,
        #[arg(long, default_value = "lucina-nez")]
        container: String,
        /// Fixed clock for both sides (default: now).
        #[arg(long)]
        now: Option<String>,
        /// Tier-0 settings as on lucina-nez (`LUCINA_FAST_CHAT_ENABLED=true`).
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        fast_chat: bool,
    },
    /// A stand-in secretary-server for parity runs (notes, calendar, avatar, notify).
    FakeSecretary {
        #[arg(long, default_value = "127.0.0.1:8798")]
        bind: SocketAddr,
        #[arg(long)]
        record: Option<PathBuf>,
        /// Read-through: forward GETs to this secretary-server (writes stay local).
        #[arg(long)]
        upstream: Option<String>,
        /// The upstream service token file (read once, never printed).
        #[arg(long)]
        upstream_token_file: Option<PathBuf>,
    },
    /// Write the live Node Lucina's environment as an env file (values never printed).
    EnvFromNode {
        #[arg(long, default_value = "lucina-nez")]
        container: String,
        #[arg(long)]
        out: PathBuf,
        /// `systemd` (Rust names, host paths) or `docker` (everything, as is).
        #[arg(long, default_value = "systemd")]
        format: String,
        /// Also write the Node SECRETARY_API_TOKEN to this file (0600).
        #[arg(long)]
        token_out: Option<PathBuf>,
    },
    /// The same /api/secretary requests against a Node reference and a Rust instance.
    Parity(Box<ParityArgs>),
    /// Replay the last real app/web chat turns of a timeline (read only) against a
    /// Rust Lucina and compare with what Node answered. Use a sandbox: turns write.
    ShadowReplay {
        /// The live timeline.jsonl (only read).
        #[arg(long, default_value = "/home/nez/Projects/Lucina/data/timeline.jsonl")]
        timeline: PathBuf,
        #[arg(long, default_value_t = 40)]
        last: usize,
        #[arg(long, default_value = "web,ios")]
        channels: String,
        #[arg(long)]
        rust_url: String,
        #[arg(long)]
        token_file: PathBuf,
        /// Full report with texts (written 0600).
        #[arg(long)]
        out: PathBuf,
        /// Print the messages and both replies on the console.
        #[arg(long)]
        show_text: bool,
        #[arg(long, default_value_t = 1500)]
        pause_ms: u64,
    },
    /// Print the "Runtime capabilities" block this environment produces.
    CapabilityBlock,
    /// Append `count` items to a timeline file as fast as possible (shared-writer test).
    TimelineAppendTest {
        #[arg(long)]
        path: PathBuf,
        #[arg(long, default_value_t = 300)]
        count: usize,
    },
    /// Check a timeline file: every line whole JSON with a unique id; count the
    /// shared-writer test lines (`node line N` / `rust line N`).
    CheckTimeline {
        #[arg(long)]
        path: PathBuf,
        #[arg(long, default_value_t = 0)]
        expect_node: usize,
        #[arg(long, default_value_t = 0)]
        expect_rust: usize,
    },
    /// Grant Lucina's own Google account to this host (opens nothing: prints
    /// the consent link, then takes the redirected URL with `--code`).
    GoogleAuth {
        /// The service (only `drive`).
        service: String,
        /// The whole address-bar URL Google redirected to, or the bare code.
        #[arg(long)]
        code: Option<String>,
        /// Ask for read-only Drive (default: full Drive, read and write).
        #[arg(long)]
        read_only: bool,
    },
    /// Lucina's Google Drive (read-only).
    Drive {
        #[command(subcommand)]
        cmd: DriveCmd,
    },
    /// The ops record (crate lucina_ops): record, search, Map/ pages, conflict sweep.
    Ops {
        #[command(subcommand)]
        cmd: OpsCmd,
        /// The synced folder (default LUCINA_OPS_VAULT_DIR, else /home/nez/data/lucina-vault/Lucina).
        #[arg(long, global = true)]
        vault: Option<PathBuf>,
        /// records.jsonl and the lock (default LUCINA_OPS_STATE_DIR, else <LUCINA_DATA_DIR>/ops).
        #[arg(long, global = true)]
        state: Option<PathBuf>,
        /// Log dates and times (default LUCINA_TIMEZONE, else Europe/Rome).
        #[arg(long, global = true)]
        tz: Option<String>,
    },
    /// JEV-first routing: the weekly drift report (docs/lucina-rust/JEV-ROUTING.md).
    Jev {
        #[command(subcommand)]
        cmd: JevCmd,
    },
    /// The weekly upstream watch (docs/upstream-watch.md): what the originals we ported released
    /// since our pin, JEV's verdict, and "Port into ours / Later / Ignore" for the owner.
    Upstream {
        #[command(subcommand)]
        cmd: UpstreamCmd,
    },
    /// Ask Lucina for a go-ahead through the running server (docs/design/decisions-api.md): she
    /// decides, or pushes the question to the owner's phone and answers `pending`. Prints the
    /// decision as JSON. `decide status <id> [--wait N]` and `decide list` read them back.
    /// Lucina moves a secret into Infisical herself: `save`, `status`, `scan`. A client of the running
    /// server; prints references, names and outcomes, never a value.
    SecretKeep {
        #[command(subcommand)]
        cmd: KeepCmd,
    },
    #[command(args_conflicts_with_subcommands = true)]
    /// One model call through a named provider, on the running server: no turn, no timeline, no tool runs.
    ModelTry {
        /// deepseek | mistral.
        #[arg(long, default_value = "mistral")]
        provider: String,
        /// A model id other than the provider's default.
        #[arg(long)]
        model: Option<String>,
        /// Stream, and report the time to the first text.
        #[arg(long)]
        stream: bool,
        /// Offer her real tools (nothing runs; the answer shows what the model called).
        #[arg(long)]
        tools: bool,
        prompt: String,
    },
    Decide {
        #[command(subcommand)]
        cmd: Option<DecideCmd>,
        #[command(flatten)]
        ask: DecideArgs,
    },
    /// The flaw tracker's database, beside the running server: ls, show, report, target,
    /// duplicate, merged, deployed, matrix.
    Flaws {
        #[command(subcommand)]
        cmd: FlawsCmd,
        /// The database (default LUCINA_FLAWS_DB, else <LUCINA_DATA_DIR>/flaws/flaws.db).
        #[arg(long, global = true)]
        db: Option<PathBuf>,
    },
    /// The parcels Lucina follows (the file beside the running server; the server sees changes):
    /// list, add <number> [--carrier c] [--label l], status <id|number|label>, remove <key>,
    /// tick [--send]. A tick without --send prints what it would say and changes nothing.
    Parcels {
        #[command(subcommand)]
        cmd: ParcelsCmd,
    },
    /// Lucina's messages to Claude, Grok and Lucina Agents, through the running server:
    /// `reach inbox [--to claude] [--all]`, `reach show <id>`, `reach reply <id> --from <who> <text>`.
    Reach {
        #[command(subcommand)]
        cmd: ReachCmd,
    },
    /// Grocery deals near home: list | cheapest <query> | lunch | map | digest | alerts
    /// [--dry-run|--send] [--channel app|telegram|both] [--user ID] [--fixtures DIR]
    /// [--items "A,B"] [--radiusM 500-15000]. Digest and alerts are dry runs unless --send.
    Deals {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Watch the owner's Discord presence (separate gateway session).
    PresenceWatch {
        /// Data dir (default LUCINA_DATA_DIR or ./data); files go to <data>/presence.
        #[arg(long)]
        data: Option<PathBuf>,
        /// File holding the bot token (mode 0600).
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ParcelsCmd {
    /// Every parcel with its last status.
    List,
    /// Follow a parcel (looked up once now).
    Add {
        number: String,
        /// The scraper's carrier id (default: detected).
        #[arg(long)]
        carrier: Option<String>,
        #[arg(long)]
        label: Option<String>,
    },
    /// One parcel, looked up again.
    Status { key: String },
    /// Stop following a parcel and forget it.
    Remove { key: String },
    /// Run one pass of the tracker. Without --send nothing is stored or sent.
    Tick {
        /// Look the parcels up for real, store the results, and tell the owner.
        #[arg(long)]
        send: bool,
    },
}

#[derive(Subcommand)]
enum FlawsCmd {
    /// Flaws, worst first.
    Ls {
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        state: Option<String>,
    },
    /// One flaw: targets and history.
    Show { id: String },
    /// Record a flaw, in the words given.
    Report {
        words: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        severity: Option<String>,
        /// Clients where it was seen (repeat).
        #[arg(long)]
        affected: Vec<String>,
        #[arg(long)]
        thread: Option<String>,
        #[arg(long)]
        discovered_from: Option<String>,
        /// irreversible, credentials or payment: its fix waits for the owner's go.
        #[arg(long)]
        gate: Option<String>,
        /// Someone already triaged it: no agent will.
        #[arg(long)]
        triaged: bool,
    },
    /// Change one client's row. verified and not_affected are refused without closing evidence.
    Target {
        id: String,
        platform: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        layer: Option<String>,
        #[arg(long)]
        evidence_kind: Option<String>,
        #[arg(long)]
        evidence_ref: Option<String>,
        #[arg(long)]
        build: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        commit: Option<String>,
        #[arg(long)]
        deploy_tag: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Mark it a duplicate of another flaw.
    Duplicate { id: String, of: String },
    /// agentd: a task's merge landed.
    Merged { id: String, platform: String, commit: String },
    /// agentd: a deploy holds this target's fix; kept if the runtime carries it.
    Deployed { id: String, platform: String, runtime: String, tag: String },
    /// Every live flaw against every client.
    Matrix,
}

#[derive(Subcommand, Debug)]
enum ReachCmd {
    /// Messages Lucina sent that wait for an answer (newest first).
    Inbox {
        /// claude, grok or agents.
        #[arg(long, default_value = "claude")]
        to: String,
        /// Answered ones too.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// JSON instead of one line per thread.
        #[arg(long)]
        json: bool,
    },
    /// One thread in full, with the text as sent.
    Show { id: String },
    /// Answer a thread: the reply goes back into the conversation it came from.
    Reply {
        id: String,
        /// Who answers (your session's name).
        #[arg(long, default_value = "Claude")]
        from: String,
        /// The reply (≤ 8000 characters).
        text: String,
    },
}

#[derive(clap::Args)]
struct DecideArgs {
    /// What to decide (1-2000 characters).
    #[arg(long)]
    question: Option<String>,
    /// One option per flag, 2-6 (`--option merge --option hold`).
    #[arg(long = "option")]
    options: Vec<String>,
    /// The option you recommend (its text or number).
    #[arg(long = "recommend")]
    recommendation: Option<String>,
    /// Facts Lucina should weigh (up to 8000 characters).
    #[arg(long)]
    context: Option<String>,
    /// low | medium | high.
    #[arg(long)]
    impact: Option<String>,
    /// true | false: can it be undone?
    #[arg(long)]
    reversible: Option<bool>,
    /// low | normal | high.
    #[arg(long)]
    urgency: Option<String>,
    /// Groups decisions on one subject (ops record project).
    #[arg(long)]
    topic: Option<String>,
    /// Who asks: your session name or id.
    #[arg(long = "asked-by")]
    asked_by: Option<String>,
    /// The client kind recorded with it.
    #[arg(long, default_value = "claude-code")]
    client: String,
    /// Seconds to wait (polling every 3 s) while the owner has not answered (0-600).
    #[arg(long, default_value_t = 0)]
    wait: u64,
}

#[derive(Subcommand)]
enum JevCmd {
    /// Journal, labelled replay, drift against the previous report, one ops record.
    DriftReport {
        /// How many days of `lucina-rs.service` journal to read.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// `cal-in.jsonl`, `labels.txt`, `extras.jsonl`. Default `LUCINA_JEV_CAL_DIR`, else
        /// `/home/nez/Projects/Lucina/data/jev-cal`. A missing directory skips the replay.
        #[arg(long)]
        labels_dir: Option<PathBuf>,
        /// Print the ops record. Do not write it or the summary.
        #[arg(long)]
        dry_run: bool,
        /// Add suggested `LUCINA_JEV_FIRST_INTENT_MIN` values. Does not edit mode.env.
        #[arg(long)]
        recalibrate: bool,
    },
}

#[derive(Subcommand)]
enum UpstreamCmd {
    /// Reconcile, then check every entry: changes, summary, verdict, questions, report, ops record.
    Check {
        /// A registry file instead of origin/main:docs/upstreams.toml of LUCINA_UPSTREAM_REPO.
        #[arg(long)]
        registry: Option<PathBuf>,
        /// Only these entries (repeatable).
        #[arg(long)]
        only: Vec<String>,
        /// Ask nothing, store no state, write no ops record (the report goes to runs/<date>-dry-run.md).
        #[arg(long)]
        dry_run: bool,
        /// Store the results but ask the owner nothing.
        #[arg(long)]
        no_notify: bool,
        /// Questions to the owner this run (default: the registry's max_decisions_per_run).
        #[arg(long)]
        max_decisions: Option<usize>,
    },
    /// Act on the owner's answers: workspace and brief for "Port into ours", draft plans.
    Reconcile {
        #[arg(long)]
        registry: Option<PathBuf>,
        /// Show what would happen; run and store nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// One line per entry: pin, latest seen, decision, backlog, workspace.
    Status {
        #[arg(long)]
        registry: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Parse and validate the registry; exit 1 on errors.
    Validate {
        #[arg(long)]
        registry: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum KeepCmd {
    /// Queue a move of a secret to Infisical: from a note or a chat message through the local model,
    /// from a file or a .env file by plain parsing.
    Save {
        /// note:<slug>, chat:<item id>, file:<path> or env:<path>.
        #[arg(long)]
        source: String,
        /// Required, except for env:, where each key keeps its own.
        #[arg(long, default_value = "")]
        name: String,
        /// env: only these keys, comma-separated. Default: the keys that look like secrets.
        #[arg(long, default_value = "")]
        keys: String,
        /// env: what stored names start with. Default with no project: KEEP_<FOLDER>_.
        #[arg(long, default_value = "")]
        prefix: String,
        /// file:, env: clear the source after the read-back, if nothing is seen using it.
        #[arg(long)]
        clear: bool,
        /// Which secret in that text, in words. Never the value.
        #[arg(long, default_value = "")]
        what: String,
        #[arg(long, default_value = "")]
        project: String,
        #[arg(long, default_value = "")]
        environment: String,
        #[arg(long, default_value = "")]
        path: String,
        /// Store the note's whole text as one secret.
        #[arg(long)]
        whole: bool,
        /// The open request (card) this answers.
        #[arg(long, default_value = "")]
        request: String,
        /// Wait this many seconds for the outcome.
        #[arg(long, default_value_t = 0)]
        wait: u64,
    },
    /// How a job went.
    Status {
        job: String,
        #[arg(long, default_value_t = 0)]
        wait: u64,
    },
    /// Where a secret may be: references only.
    Scan,
    /// What a project needs, what is found, what is missing: names and states.
    Project {
        dir: String,
        /// Also move what is found to Infisical. The files stay.
        #[arg(long)]
        deposit: bool,
        #[arg(long, default_value = "")]
        project: String,
        #[arg(long, default_value = "")]
        environment: String,
        #[arg(long, default_value = "")]
        path: String,
        #[arg(long, default_value = "")]
        prefix: String,
    },
}

#[derive(clap::Subcommand)]
enum DecideCmd {
    /// One decision as JSON (`--wait N`: poll while it is pending).
    Status {
        id: String,
        #[arg(long, default_value_t = 0)]
        wait: u64,
    },
    /// Take back a question still waiting for the owner (asked by mistake, settled elsewhere).
    Withdraw {
        id: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Recent decisions as JSON.
    List {
        /// pending | decided | expired | withdrawn.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum OpsCmd {
    /// Record from stdin (one JSON object, an array or JSON lines); prints one result per line.
    /// No Honcho here: the server's loop sends the records within 10 minutes.
    Record {
        /// Who reports (`cli`, `deploy-guard`, `project-map`, ...).
        #[arg(long, default_value = "cli")]
        source: String,
    },
    /// Search the store (no Honcho); prints the result as JSON.
    Search {
        #[arg(default_value = "")]
        query: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    /// Write a whole page under Map/ from stdin (the project-map session); prints {path, changed}.
    Page { path: String },
    /// Move Syncthing conflict copies to Conflicts/ and put lost entries back.
    Sweep,
    /// The Obsidian ingest (crate lucina_vault_ingest): records, commits and session
    /// reports become Zettelkasten notes, written by the local Qwen only while it is free.
    Ingest {
        #[command(subcommand)]
        cmd: IngestCmd,
        /// queue.jsonl and state.json (default LUCINA_VAULT_INGEST_STATE_DIR, else <state>/ingest).
        #[arg(long)]
        ingest_state: Option<PathBuf>,
        /// Repos followed, `name=path@ref#project;…` (default LUCINA_VAULT_INGEST_GIT).
        #[arg(long)]
        git: Option<String>,
    },
}

#[derive(Subcommand)]
enum IngestCmd {
    /// Queue depth, last run, next try, followed repos (JSON).
    Status,
    /// Queue what is new in the ops record and the repos (no model).
    Scan,
    /// Scan, then ingest now: ignores the backoff, never preempts another Qwen user.
    Run {
        /// May load an unloaded model when the GPU is entirely free.
        #[arg(long)]
        cold: bool,
        /// Items for the model this run.
        #[arg(long)]
        limit: Option<usize>,
        /// Do not scan the sources first.
        #[arg(long)]
        no_scan: bool,
    },
    /// Queue a Claude session report from stdin: {id?, title, project, summary, at?, files?}.
    Session,
    /// Re-arm items that gave up (all when no key is given).
    Retry {
        keys: Vec<String>,
        /// Also re-ingest done or skipped items.
        #[arg(long)]
        any: bool,
    },
}

#[derive(Subcommand)]
enum DriveCmd {
    /// Files, newest first (JSON lines).
    Ls {
        /// Name contains.
        #[arg(long)]
        query: Option<String>,
        /// Full-text search instead of names.
        #[arg(long)]
        search: Option<String>,
        /// Only this folder (id).
        #[arg(long)]
        folder: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Upload a local file (prints the new file as JSON).
    Put {
        file: PathBuf,
        /// Parent folder id (default: My Drive).
        #[arg(long)]
        folder: Option<String>,
        /// Name in Drive (default: the local file name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Create a folder (prints it as JSON).
    Mkdir {
        name: String,
        #[arg(long)]
        parent: Option<String>,
    },
    /// One file: its text on stdout, or its bytes to `--out` (with checksums).
    Get {
        /// File id or Drive link.
        file: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

struct GoogleEnv {
    client_file: PathBuf,
    token_file: PathBuf,
    pending_file: PathBuf,
    redirect_uri: String,
    account: String,
}

impl GoogleEnv {
    fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let home = var("HOME").unwrap_or_else(|| "/home/nez".into());
        Self {
            client_file: var("LUCINA_GOOGLE_CLIENT_FILE")
                .unwrap_or_else(|| google::DEFAULT_CLIENT_FILE.into())
                .into(),
            token_file: var("LUCINA_DRIVE_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| google::config_file(&home, "google-drive-token.json")),
            pending_file: google::config_file(&home, "google-auth-pending.json"),
            redirect_uri: var("LUCINA_GOOGLE_REDIRECT_URI")
                .unwrap_or_else(|| google::DEFAULT_REDIRECT_URI.into()),
            account: var("LUCINA_DRIVE_ACCOUNT").unwrap_or_else(|| google::DEFAULT_ACCOUNT.into()),
        }
    }

    fn drive(&self) -> anyhow::Result<google::Drive> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(google::Drive::open(
            http,
            google::GoogleEndpoints::default(),
            &self.client_file,
            &self.token_file,
        )?)
    }
}

async fn google_auth(service: &str, code: Option<String>, read_only: bool) -> anyhow::Result<()> {
    if service != "drive" {
        anyhow::bail!("unknown service {service:?} (only `drive`)");
    }
    let g = GoogleEnv::from_env();
    let client = google::GoogleClient::from_file(&g.client_file)?;
    let ep = google::GoogleEndpoints::default();
    let now = chrono::Utc::now().timestamp();
    let Some(pasted) = code else {
        let pending = google::Pending::new(now);
        pending.save(&g.pending_file)?;
        let scope = if read_only {
            google::SCOPE_DRIVE_READONLY
        } else {
            google::SCOPE_DRIVE
        };
        let url = google::authorize_url(&ep, &client, &g.redirect_uri, &pending, &g.account, scope);
        println!(
            "Open this link signed in as {} and allow {} access to Drive:\n",
            g.account,
            if read_only { "read" } else { "read and write" }
        );
        println!("{url}\n");
        println!(
            "Google then redirects to {}?...&code=... (the page may not load: that is fine).",
            g.redirect_uri
        );
        println!("Copy the whole address-bar URL within 15 minutes and run:");
        println!("  lucina google-auth drive --code '<the URL>'");
        return Ok(());
    };
    let pending = google::Pending::load(&g.pending_file)?;
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let token = google::complete_consent(
        &http,
        &ep,
        &client,
        &g.redirect_uri,
        &pending,
        &pasted,
        &g.account,
        now,
    )
    .await?;
    token.save(&g.token_file)?;
    let _ = std::fs::remove_file(&g.pending_file);
    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "account": token.account_email,
            "scopes": token.scopes(),
            "tokenPath": g.token_file,
        })
    );
    Ok(())
}

/// `lucina-rs jev drift-report`: journal, labelled replay, one ops record. The API token comes
/// from the same env files as `decide` and is never printed.
async fn jev_cmd(cmd: JevCmd) -> anyhow::Result<()> {
    match cmd {
        JevCmd::DriftReport {
            days,
            labels_dir,
            dry_run,
            recalibrate,
        } => {
            lucina_server::jev_drift::run(lucina_server::jev_drift::Args {
                days,
                labels_dir,
                dry_run,
                recalibrate,
            })
            .await
        }
    }
}

/// `lucina-rs upstream …` (lucina_server::upstream). The API token comes from the unit's env files
/// and is never printed.
async fn upstream_cmd(cmd: UpstreamCmd) -> anyhow::Result<()> {
    use lucina_server::upstream;
    match cmd {
        UpstreamCmd::Check {
            registry,
            only,
            dry_run,
            no_notify,
            max_decisions,
        } => {
            upstream::check(upstream::CheckArgs {
                registry,
                only,
                dry_run,
                no_notify,
                max_decisions,
            })
            .await
        }
        UpstreamCmd::Reconcile { registry, dry_run } => {
            upstream::reconcile(registry, dry_run).await
        }
        UpstreamCmd::Status { registry, json } => upstream::status(registry, json),
        UpstreamCmd::Validate { registry } => upstream::validate(registry),
    }
}

/// `lucina-rs decide …`: a client of the running server (`/api/secretary/decisions`), so the
/// decision goes through the same path as `lucina_decide`: evidence, Jev, the owner's push, the
/// ops record. The API token comes from the unit's env files and is never printed.
fn flaws_cmd(cmd: FlawsCmd, db: Option<PathBuf>) -> anyhow::Result<()> {
    use lucina_flaws::{Evidence, EvidenceKind, Gate, Layer, ListQuery, NewFlaw, Platform, Severity, TargetStatus, TargetUpdate};
    let flaws = lucina_server::flaws::open_cli(db)?;
    let actor = std::env::var("LAGENT_SESSION")
        .or_else(|_| std::env::var("LUCINA_FLAWS_ACTOR"))
        .unwrap_or_else(|_| "lucina-rs-cli".to_owned());
    let out = match cmd {
        FlawsCmd::Ls { q, state } => {
            let state = state.as_deref().map(lucina_flaws::FlawState::parse).transpose()?;
            serde_json::to_value(flaws.list(&ListQuery { q, state, ..ListQuery::default() })?)?
        }
        FlawsCmd::Show { id } => serde_json::to_value(flaws.get(&id)?)?,
        FlawsCmd::Report { words, title, severity, affected, thread, discovered_from, gate, triaged } => {
            let new = NewFlaw {
                words,
                title,
                severity: severity.as_deref().map(Severity::parse).transpose()?,
                affected: affected.iter().map(|p| Platform::parse(p)).collect::<Result<_, _>>()?,
                thread,
                discovered_from,
                gate: gate.as_deref().map(Gate::parse).transpose()?,
            };
            let r = flaws.report(new, &actor)?;
            if triaged {
                flaws.start_triage(&r.flaw.flaw.id, "manual", true, &actor)?;
            }
            serde_json::to_value(r)?
        }
        FlawsCmd::Target { id, platform, status, layer, evidence_kind, evidence_ref, build, workspace, session, commit, deploy_tag, note } => {
            let evidence = match evidence_kind {
                None => None,
                Some(k) => Some(Evidence {
                    kind: EvidenceKind::parse(&k)?,
                    reference: evidence_ref.unwrap_or_default(),
                    build: build.clone(),
                    note: note.clone(),
                }),
            };
            let u = TargetUpdate {
                status: status.as_deref().map(TargetStatus::parse).transpose()?,
                layer: layer.as_deref().map(Layer::parse).transpose()?,
                evidence,
                workspace,
                session,
                commit,
                deploy_tag,
                build,
                note,
            };
            serde_json::to_value(flaws.set_target(&id, Platform::parse(&platform)?, u, &actor)?)?
        }
        FlawsCmd::Duplicate { id, of } => {
            let p = lucina_flaws::FlawPatch { duplicate_of: Some(of), ..Default::default() };
            serde_json::to_value(flaws.patch(&id, p, &actor)?)?
        }
        FlawsCmd::Merged { id, platform, commit } => serde_json::to_value(flaws.record_merge(&id, Platform::parse(&platform)?, &commit, "lagent")?)?,
        FlawsCmd::Deployed { id, platform, runtime, tag } => {
            let p = Platform::parse(&platform)?;
            let t = flaws.get(&id)?.targets.into_iter().find(|t| t.platform == p).ok_or(lucina_flaws::FlawError::NotFound)?;
            let first_for_runtime = !t.deploy_tag.as_deref().is_some_and(|d| d.starts_with(&format!("deploy/{runtime}/")));
            if lucina_flaws::policy::runtime_serves(p, t.layer, &runtime) && first_for_runtime {
                serde_json::to_value(flaws.record_deploy(&id, p, &tag, "lagent")?)?
            } else {
                serde_json::to_value(t)?
            }
        }
        FlawsCmd::Matrix => flaws.matrix()?,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

async fn parcels_cmd(cmd: ParcelsCmd) -> anyhow::Result<()> {
    use lucina_parcels::text;
    let svc = lucina_server::parcels::open_cli()?;
    let now = chrono::Utc::now();
    let view = |p: &lucina_parcels::Parcel| {
        serde_json::json!({
            "id": p.id, "name": text::name(p), "number": p.number, "state": p.state,
            "status": p.status.as_ref().map(|s| text::status_line(s, svc.cfg.tz)),
            "last_checked": p.last_checked, "last_error": p.last_error,
        })
    };
    let out = match cmd {
        ParcelsCmd::List => serde_json::Value::Array(svc.store.list().iter().map(view).collect()),
        ParcelsCmd::Add { number, carrier, label } => {
            let a = svc.add(&number, carrier.as_deref(), label.as_deref(), now, true).await;
            let mut v = view(&a.parcel);
            v["created"] = a.created.into();
            v["saved"] = a.saved.into();
            if let Err(e) = &a.lookup {
                v["lookup"] = text::failure_line(e).into();
            }
            v
        }
        ParcelsCmd::Status { key } => {
            let p = svc.store.get(&key).ok_or_else(|| anyhow::anyhow!("no parcel matches {key}"))?;
            let looked = svc.refresh(&p, now).await;
            let mut v = view(&svc.store.get(&p.id).unwrap_or(p));
            if let Err(e) = &looked {
                v["lookup"] = text::failure_line(e).into();
            }
            v
        }
        ParcelsCmd::Remove { key } => {
            let p = svc.store.remove(&key).ok_or_else(|| anyhow::anyhow!("no parcel matches {key}"))?;
            serde_json::json!({ "removed": text::name(&p) })
        }
        ParcelsCmd::Tick { send: false } => {
            // A copy of the list in a scratch file: the lookups are real, nothing of ours changes.
            let dir = std::env::temp_dir().join(format!("lucina-parcels-dry-{}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            let copy = dir.join("parcels.json");
            if svc.store.path().exists() {
                std::fs::copy(svc.store.path(), &copy)?;
            }
            let scratch = lucina_parcels::Store::open(&copy);
            let messages = lucina_parcels::tick(&scratch, &svc.scraper, now, &svc.cfg).await;
            std::fs::remove_dir_all(&dir).ok();
            serde_json::json!(messages.iter().map(|m| serde_json::json!({"kind": format!("{:?}", m.kind), "text": m.text, "dedupKey": m.dedup_key})).collect::<Vec<_>>())
        }
        ParcelsCmd::Tick { send: true } => {
            let cfg = Config::from_env()?;
            let state = AppState::build(cfg, None)?;
            let svc = state.tool_env.parcels.clone().ok_or_else(|| anyhow::anyhow!("parcel tracking is off"))?;
            let messages = lucina_parcels::tick(&svc.store, &svc.scraper, now, &svc.cfg).await;
            lucina_server::parcels::deliver(&state, &messages).await;
            serde_json::json!({ "sent": messages.len() })
        }
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

async fn reach_cmd(cmd: ReachCmd) -> anyhow::Result<()> {
    use lucina_server::reach::cli;
    let out = match cmd {
        ReachCmd::Inbox { to, all, limit, json } => {
            let v = cli::inbox(&to, all, limit).await?;
            if !json {
                let lines = cli::lines(&v);
                println!("{}", if lines.is_empty() { "(nothing waiting)".into() } else { lines });
                return Ok(());
            }
            v
        }
        ReachCmd::Show { id } => cli::show(&id).await?,
        ReachCmd::Reply { id, from, text } => cli::reply(&id, &from, &text).await?,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

async fn keep_cmd(cmd: KeepCmd) -> anyhow::Result<()> {
    use lucina_server::decide::cli;
    let c = cli::Client::from_env()?;
    let mut wait_for = 0;
    let mut out = match cmd {
        KeepCmd::Save { source, name, keys, prefix, clear, what, project, environment, path, whole, request, wait } => {
            wait_for = wait;
            let keys: Vec<&str> = keys.split(',').map(str::trim).filter(|k| !k.is_empty()).collect();
            c.post(
                "secrets/keep",
                &serde_json::json!({"source": source, "name": name, "what": what, "project": project,
                                    "environment": environment, "path": path, "whole": whole, "request": request,
                                    "keys": keys, "prefix": prefix, "clear": clear}),
            )
            .await?
        }
        KeepCmd::Status { job, wait } => {
            wait_for = wait;
            c.post("secrets/keep", &serde_json::json!({"job": job})).await?
        }
        KeepCmd::Scan => c.post("secrets/scan", &serde_json::json!({})).await?,
        KeepCmd::Project { dir, deposit, project, environment, path, prefix } => {
            c.post(
                "secrets/project",
                &serde_json::json!({"dir": dir, "deposit": deposit, "project": project, "environment": environment,
                                    "path": path, "prefix": prefix}),
            )
            .await?
        }
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(wait_for);
    while out["state"] == "queued" && std::time::Instant::now() < until {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        out = c.post("secrets/keep", &serde_json::json!({"job": out["job"]})).await?;
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

async fn decide_cmd(cmd: Option<DecideCmd>, a: DecideArgs) -> anyhow::Result<()> {
    use lucina_server::decide::cli;
    let c = cli::Client::from_env()?;
    let out = match cmd {
        Some(DecideCmd::Status { id, wait }) => c.wait(c.get(&id).await?, wait).await?,
        Some(DecideCmd::Withdraw { id, reason }) => c.withdraw(&id, &reason).await?,
        Some(DecideCmd::List { status, limit }) => c.list(status.as_deref(), limit).await?,
        None => {
            let question = a.question.ok_or_else(|| {
                anyhow::anyhow!("--question is required (or use `decide status <id>`)")
            })?;
            let body = serde_json::json!({
                "question": question,
                "options": a.options,
                "recommendation": a.recommendation,
                "context": a.context,
                "impact": a.impact,
                "reversible": a.reversible,
                "urgency": a.urgency,
                "topic": a.topic,
                "asking_session": a.asked_by,
                "asking_client": a.client,
            });
            c.wait(c.create(&body).await?, a.wait).await?
        }
    };
    if out.get("decision_id").is_some() {
        eprintln!("{}", cli::line(&out));
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

async fn ops_cmd(
    cmd: OpsCmd,
    vault: Option<PathBuf>,
    state: Option<PathBuf>,
    tz: Option<String>,
) -> anyhow::Result<()> {
    use std::io::Read as _;
    let ops = std::sync::Arc::new(lucina_server::ops::open_cli(vault, state, tz)?);
    let stdin = || -> anyhow::Result<String> {
        let mut s = String::new();
        std::io::stdin()
            .take(lucina_ops::vault::MAX_PAGE_BYTES as u64 + 1)
            .read_to_string(&mut s)?;
        if s.len() > lucina_ops::vault::MAX_PAGE_BYTES {
            anyhow::bail!("stdin exceeds {} bytes", lucina_ops::vault::MAX_PAGE_BYTES);
        }
        Ok(s)
    };
    match cmd {
        OpsCmd::Record { source } => {
            let mut failed = 0;
            for v in lucina_server::ops::parse_records_input(&stdin()?)? {
                let r = match serde_json::from_value::<lucina_ops::NewRecord>(v) {
                    Ok(new) => ops.record(new, &source).await.map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                match r {
                    Ok(r) => println!("{}", serde_json::to_string(&r)?),
                    Err(e) => {
                        failed += 1;
                        println!("{}", serde_json::json!({"error": e}));
                    }
                }
            }
            if failed > 0 {
                anyhow::bail!("{failed} record(s) refused");
            }
        }
        OpsCmd::Search {
            query,
            project,
            kind,
            limit,
        } => {
            let kind = kind
                .map(|k| serde_json::from_value(serde_json::json!(k)))
                .transpose()
                .map_err(|_| {
                    anyhow::anyhow!("kind: deploy, incident, decision, status or finding")
                })?;
            let r = ops
                .search(lucina_ops::SearchQuery {
                    q: query,
                    project,
                    kind,
                    limit: Some(limit),
                    ..Default::default()
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        OpsCmd::Page { path } => {
            let changed = ops.vault().write_page(&path, &stdin()?)?;
            println!("{}", serde_json::json!({"path": path, "changed": changed}));
        }
        OpsCmd::Sweep => {
            let r = ops.sweep_conflicts()?;
            println!("{}", serde_json::to_string(&r)?);
        }
        OpsCmd::Ingest {
            cmd,
            ingest_state,
            git,
        } => {
            let (state_dir, tz) = (ops.config().state_dir.clone(), ops.config().time_zone);
            let mut cfg = lucina_vault_ingest::IngestConfig::from_lookup(
                |k| std::env::var(k).ok(),
                &state_dir.join("ingest"),
                tz,
            )?;
            if let Some(s) = ingest_state {
                cfg.state_dir = s;
            }
            if let Some(g) = git {
                cfg.git = lucina_vault_ingest::sources::GitRepo::parse_list(&g)
                    .map_err(|e| anyhow::anyhow!(e))?;
            }
            let ingest = lucina_vault_ingest::Ingest::open(cfg, ops.clone())?;
            let now = chrono::Utc::now;
            let out = match cmd {
                IngestCmd::Status => serde_json::to_value(ingest.status()?)?,
                IngestCmd::Scan => serde_json::to_value(ingest.scan(now()).await?)?,
                IngestCmd::Run {
                    cold,
                    limit,
                    no_scan,
                } => {
                    let scan = if no_scan {
                        None
                    } else {
                        Some(ingest.scan(now()).await?)
                    };
                    let opts = lucina_vault_ingest::RunOpts {
                        force: true,
                        cold,
                        limit,
                    };
                    let run = ingest.run(opts, now()).await?;
                    serde_json::json!({"scan": scan, "run": run})
                }
                IngestCmd::Session => {
                    let v: serde_json::Value = serde_json::from_str(&stdin()?)?;
                    let (key, added) = ingest.enqueue_session(&v, now())?;
                    serde_json::json!({"key": key, "queued": added})
                }
                IngestCmd::Retry { keys, any } => {
                    serde_json::json!({"rearmed": ingest.retry(&keys, any, now())?})
                }
            };
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
    }
    Ok(())
}

async fn drive_cmd(cmd: DriveCmd) -> anyhow::Result<()> {
    let d = GoogleEnv::from_env().drive()?;
    match cmd {
        DriveCmd::Ls {
            query,
            search,
            folder,
            limit,
        } => {
            let mut q = google::ListQuery {
                folder_id: folder,
                name_contains: query,
                full_text: search,
                page_size: limit.clamp(1, 100),
                page_token: None,
            };
            let mut left = limit as usize;
            loop {
                let page = d.list(&q).await?;
                for f in page.files.iter().take(left) {
                    println!("{}", serde_json::to_string(f)?);
                }
                left = left.saturating_sub(page.files.len());
                match page.next_page_token {
                    Some(t) if left > 0 => q.page_token = Some(t),
                    _ => break,
                }
            }
        }
        DriveCmd::Put { file, folder, name } => {
            let bytes = std::fs::read(&file)?;
            let name = name.unwrap_or_else(|| {
                file.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "upload".into())
            });
            let f = d
                .upload(
                    &name,
                    &bytes,
                    google::mime_for_name(&name),
                    folder.as_deref(),
                    None,
                )
                .await?;
            let sha = google::sha256_hex(&bytes);
            println!(
                "{}",
                serde_json::json!({"ok": true, "file": f, "sha256": sha,
                    "sha256Verified": f.sha256_checksum.as_deref().map(|s| s.eq_ignore_ascii_case(&sha))})
            );
        }
        DriveCmd::Mkdir { name, parent } => {
            let f = d.create_folder(&name, parent.as_deref()).await?;
            println!("{}", serde_json::to_string(&f)?);
        }
        DriveCmd::Get { file, out } => {
            let id = google::file_id_of(&file)
                .ok_or_else(|| anyhow::anyhow!("not a Drive file id or link"))?;
            let f = d.get(&id).await?;
            match out {
                None if !google::is_text(&f) => {
                    println!("{}", serde_json::to_string_pretty(&f)?);
                    eprintln!("binary file: use --out PATH to download it");
                }
                None => {
                    let bytes = d.content(&f).await?.unwrap_or_default();
                    use std::io::Write;
                    std::io::stdout().write_all(&bytes)?;
                }
                Some(path) => {
                    let bytes = d.content(&f).await?.ok_or_else(|| {
                        anyhow::anyhow!("{} has no downloadable form", f.mime_type)
                    })?;
                    let sha = google::sha256_hex(&bytes);
                    let verified = f
                        .sha256_checksum
                        .as_deref()
                        .map(|s| s.eq_ignore_ascii_case(&sha));
                    if verified == Some(false) {
                        anyhow::bail!(
                            "sha256 mismatch: Drive says {:?}, got {sha}; nothing written",
                            f.sha256_checksum
                        );
                    }
                    let tmp = path.with_extension("part");
                    std::fs::write(&tmp, &bytes)?;
                    std::fs::rename(&tmp, &path)?;
                    println!(
                        "{}",
                        serde_json::json!({
                            "ok": true,
                            "id": f.id,
                            "name": f.name,
                            "mimeType": f.mime_type,
                            "bytes": bytes.len(),
                            "sha256": sha,
                            "driveSha256": f.sha256_checksum,
                            "driveMd5": f.md5_checksum,
                            "sha256Verified": verified,
                            "out": path,
                        })
                    );
                }
            }
        }
    }
    Ok(())
}

/// Exit code for a fatal gateway close (`RestartPreventExitStatus=78`).
const EXIT_FATAL: i32 = 78;

fn presence_watch(data: Option<PathBuf>, token_file: Option<PathBuf>) -> anyhow::Result<()> {
    let mut cfg = Config::from_env()?;
    if let Some(d) = data {
        cfg.presence_dir = d.join("presence");
    }
    let owner: u64 = cfg
        .owner_discord_user_id
        .as_deref()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("LUCINA_OWNER_DISCORD_USER_ID is not set"))?;
    let token_path = token_file.or_else(|| {
        std::env::var_os("LUCINA_DISCORD_TOKEN_FILE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    let token = match token_path {
        Some(p) => std::fs::read_to_string(&p)
            .map_err(|e| anyhow::anyhow!("token file {}: {}", p.display(), e.kind()))?
            .trim()
            .to_owned(),
        None => std::env::var("DISCORD_BOT_TOKEN_LUCINA")
            .unwrap_or_default()
            .trim()
            .to_owned(),
    };
    if token.is_empty() {
        anyhow::bail!("no Discord bot token (--token-file, LUCINA_DISCORD_TOKEN_FILE or DISCORD_BOT_TOKEN_LUCINA)");
    }
    let wc = lucina_discord::watcher::WatchConfig {
        token,
        owner_id: owner,
        presence_dir: cfg.presence_dir.clone(),
        retention_days: cfg.presence_retention_days,
        refresh: std::time::Duration::from_secs(60),
        first_observation_timeout: std::time::Duration::from_secs(60),
    };
    tracing::info!(config = ?wc, "presence watcher starting");
    // One thread is plenty for one shard; it also keeps RSS small.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(lucina_discord::watcher::run(wc, shutdown_signal()));
    match result {
        Ok(()) => Ok(()),
        Err(lucina_discord::watcher::WatchError::Fatal(code)) => {
            tracing::error!(?code, "presence watcher: fatal gateway close, not retrying");
            std::process::exit(EXIT_FATAL);
        }
        Err(e) => Err(e.into()),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn check_data(dir: &std::path::Path) -> anyhow::Result<bool> {
    let mut all_ok = true;
    let timeline = dir.join("timeline.jsonl");
    if let Ok(text) = std::fs::read_to_string(&timeline) {
        let (mut n, mut bad) = (0usize, 0usize);
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            n += 1;
            let same = serde_json::from_str::<lucina_store::TimelineItem>(line)
                .ok()
                .and_then(|i| serde_json::to_string(&i).ok())
                .as_deref()
                == Some(line);
            if !same {
                bad += 1;
            }
        }
        println!("timeline.jsonl: {n} lines, {bad} not byte-identical");
        all_ok &= bad == 0;
    }
    if let Ok(bytes) = std::fs::read(dir.join("reminders.json")) {
        let same = lucina_store::ReminderStore::reserialize(&bytes)
            .ok()
            .as_deref()
            .map(str::as_bytes)
            == Some(&bytes[..]);
        println!(
            "reminders.json: {}",
            if same { "byte-identical" } else { "DIFFERS" }
        );
        all_ok &= same;
    }
    Ok(all_ok)
}

fn main() -> anyhow::Result<()> {
    let cmd = Cli::parse().cmd.unwrap_or(Cmd::Serve);
    let filter = |default: &str| {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default))
    };
    // Logs are a sink: every formatted line goes through the redactor.
    // `deals`, `decide`, `jev` and `upstream` print their results on stdout, so their logs go to stderr.
    if matches!(
        cmd,
        Cmd::Deals { .. } | Cmd::Decide { .. } | Cmd::Jev { .. } | Cmd::Upstream { .. } | Cmd::SecretKeep { .. } | Cmd::ModelTry { .. }
    ) {
        tracing_subscriber::fmt()
            .with_env_filter(filter("warn"))
            .with_writer(lucina_secrets::log::RedactingMakeWriter::new(
                std::io::stderr,
            ))
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter("info"))
            .with_writer(lucina_secrets::log::RedactingMakeWriter::new(
                std::io::stdout,
            ))
            .init();
    }
    match cmd {
        Cmd::PresenceWatch { data, token_file } => presence_watch(data, token_file),
        cmd => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let out = rt.block_on(run(cmd));
            // Do not wait on blocking work still running (a Whisper or vision subprocess,
            // a scheduler tick): it would push a stop past systemd's timeout.
            rt.shutdown_timeout(std::time::Duration::from_secs(1));
            out
        }
    }
}

async fn run(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::PresenceWatch { .. } => unreachable!("handled in main"),
        Cmd::Deals { args } => {
            let code = lucina_server::deals::cli(args).await;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Cmd::GoogleAuth {
            service,
            code,
            read_only,
        } => google_auth(&service, code, read_only).await?,
        Cmd::Drive { cmd } => drive_cmd(cmd).await?,
        Cmd::Decide { cmd, ask } => decide_cmd(cmd, ask).await?,
        Cmd::ModelTry { provider, model, stream, tools, prompt } => {
            let c = lucina_server::decide::cli::Client::from_env()?;
            let mut body = serde_json::json!({"provider": provider, "prompt": prompt, "stream": stream});
            if let Some(m) = model {
                body["model"] = m.into();
            }
            if tools {
                body["tools"] = "registry".into();
            }
            println!("{}", serde_json::to_string_pretty(&c.post("models/try", &body).await?)?);
        }
        Cmd::SecretKeep { cmd } => keep_cmd(cmd).await?,
        Cmd::Flaws { cmd, db } => flaws_cmd(cmd, db)?,
        Cmd::Parcels { cmd } => parcels_cmd(cmd).await?,
        Cmd::Reach { cmd } => reach_cmd(cmd).await?,
        Cmd::Jev { cmd } => jev_cmd(cmd).await?,
        Cmd::Upstream { cmd } => upstream_cmd(cmd).await?,
        Cmd::Ops {
            cmd,
            vault,
            state,
            tz,
        } => ops_cmd(cmd, vault, state, tz).await?,
        Cmd::Serve => {
            let cfg = Config::from_env()?;
            lucina_server::share::stop_orphans(&cfg.share);
            tracing::info!(config = ?cfg, "lucina starting (shadow mode: scheduler, Telegram and Discord off unless stated)");
            let binds = cfg.binds.clone();
            let state = AppState::build(cfg, None)?;
            state.spawn_background();
            // One server per address (127.0.0.1 and the Tailscale IP), one shutdown.
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            let telegram = state.spawn_telegram(stop_rx.clone());
            let discord = state.spawn_discord(stop_rx.clone());
            let mut servers = tokio::task::JoinSet::new();
            for bind in binds {
                // TCP_NODELAY: the live view's and LucinaDesk's sockets carry small messages
                // (acks, input) that Nagle plus delayed ACKs would hold for ~40 ms each.
                let listener = tokio::net::TcpListener::bind(bind).await?.tap_io(|tcp| {
                    let _ = tcp.set_nodelay(true);
                });
                tracing::info!(%bind, "listening");
                let router = app(state.clone());
                let mut rx = stop_rx.clone();
                servers.spawn(async move {
                    axum::serve(listener, router)
                        .with_graceful_shutdown(async move {
                            let _ = rx.wait_for(|stop| *stop).await;
                        })
                        .await
                });
            }
            // The home-network door to the live view (lan.rs), when configured. Its own task:
            // a door that cannot open (port taken, no certificate) never stops Lucina.
            if let Some(lan) = lucina_server::lan::config() {
                let rx = stop_rx.clone();
                tokio::spawn(async move {
                    if let Err(e) = lucina_server::lan::serve(lan, rx).await {
                        tracing::warn!(error = %e, "home-network door closed");
                    }
                });
            }
            tokio::select! {
                _ = shutdown_signal() => tracing::info!("shutting down"),
                Some(r) = servers.join_next() => {
                    tracing::error!(result = ?r.map(|x| x.map_err(|e| e.to_string())), "a listener stopped");
                }
            }
            let _ = stop_tx.send(true);
            // Everything fits in systemd's stop timeout (10 s, drop-in 90-stop.conf):
            // a stop that ends in SIGKILL counts as a failure and pages the owner.
            // The pollers release their leases first (both share 4 s), then the
            // listeners drain for 3 s: open SSE timeline streams never end on their
            // own, so whatever is still open after that is dropped.
            let pollers = async {
                if let Some(t) = telegram {
                    let _ = t.await;
                }
                if let Some(d) = discord {
                    let _ = d.await;
                }
            };
            if tokio::time::timeout(std::time::Duration::from_secs(4), pollers)
                .await
                .is_err()
            {
                tracing::warn!("Telegram/Discord did not stop in 4 s; exiting anyway (the kernel drops the lease flocks)");
            }
            let drain = async { while servers.join_next().await.is_some() {} };
            if tokio::time::timeout(std::time::Duration::from_secs(3), drain)
                .await
                .is_err()
            {
                tracing::info!("open HTTP streams dropped after 3 s");
                servers.abort_all();
            }
            tracing::info!("stopped");
        }
        Cmd::FakeDeepseek {
            bind,
            script,
            record,
        } => {
            let script = match script {
                Some(p) => serde_json::from_slice(&std::fs::read(p)?)?,
                None => lucina_server::fake::Script::default(),
            };
            let fake = lucina_server::fake::spawn(bind, script, record).await?;
            println!("fake DeepSeek at {}", fake.base_url);
            tokio::signal::ctrl_c().await?;
        }
        Cmd::ParityParsers {
            cases,
            script,
            container,
            now,
            fast_chat,
        } => {
            let mut input: serde_json::Value = serde_json::from_slice(&std::fs::read(&cases)?)?;
            let now = now.unwrap_or_else(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
            input["now"] = serde_json::json!(now);
            let js = std::fs::read_to_string(&script)?;
            let mut child = std::process::Command::new("docker")
                .args([
                    "exec",
                    "-i",
                    &container,
                    "node",
                    "--input-type=module",
                    "-e",
                    &js,
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;
            {
                use std::io::Write;
                let mut stdin = child.stdin.take().expect("stdin");
                stdin.write_all(input.to_string().as_bytes())?;
            }
            let out = child.wait_with_output()?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let line = stdout
                .lines()
                .find_map(|l| l.strip_prefix("PARITY_JSON:"))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no PARITY_JSON from the Node side: {}",
                        String::from_utf8_lossy(&out.stderr)
                            .chars()
                            .take(500)
                            .collect::<String>()
                    )
                })?;
            let node: serde_json::Value = serde_json::from_str(line)?;
            let settings = lucina_intent::heuristics::Settings {
                fast_chat,
                ..Default::default()
            };
            let (n, bad, report) =
                lucina_server::parity::parsers::compare(&input, &node, &settings);
            for l in &report {
                println!("{l}");
            }
            println!(
                "parity-parsers: {n} phrases, {} identical, {bad} different",
                n - bad
            );
            if bad > 0 {
                std::process::exit(1);
            }
        }
        Cmd::FakeSecretary {
            bind,
            record,
            upstream,
            upstream_token_file,
        } => {
            let upstream = match (upstream, upstream_token_file) {
                (Some(u), Some(f)) => Some((u, std::fs::read_to_string(f)?.trim().to_owned())),
                (Some(_), None) => anyhow::bail!("--upstream needs --upstream-token-file"),
                _ => None,
            };
            println!(
                "fake secretary-server at http://{bind}{}",
                if upstream.is_some() {
                    " (GETs read through)"
                } else {
                    ""
                }
            );
            lucina_server::parity::fake_secretary::serve(
                bind,
                record,
                chrono_tz::Europe::Rome,
                upstream,
            )
            .await?;
        }
        Cmd::EnvFromNode {
            container,
            out,
            format,
            token_out,
        } => {
            let names = lucina_server::parity::envgen::run(
                &container,
                &out,
                &format,
                token_out.as_deref(),
            )?;
            println!(
                "{} variables written to {} ({format}): {}",
                names.len(),
                out.display(),
                names.join(" ")
            );
        }
        Cmd::Parity(args) => {
            let ParityArgs {
                cases,
                node_url,
                rust_url,
                node_token_file,
                rust_token_file,
                node_deepseek_record,
                rust_deepseek_record,
                secretary_record,
                node_service_token_file,
                rust_service_token_file,
                node_timeline,
                rust_timeline,
                out,
                vars,
            } = *args;
            use lucina_server::parity::{fake_secretary::space_for, http};
            let initial: std::collections::HashMap<String, String> = vars
                .iter()
                .filter_map(|v| v.split_once('='))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            let read = |p: &PathBuf| std::fs::read_to_string(p).map(|s| s.trim().to_owned());
            let cases: serde_json::Value = serde_json::from_slice(&std::fs::read(&cases)?)?;
            let run = http::Run {
                node: http::Side {
                    name: "node",
                    vars: std::sync::Mutex::new(initial.clone()),
                    base: node_url,
                    token: read(&node_token_file)?,
                    deepseek_record: node_deepseek_record,
                    secretary_space: space_for(&read(&node_service_token_file)?),
                    timeline: node_timeline,
                },
                rust: http::Side {
                    name: "rust",
                    vars: std::sync::Mutex::new(initial.clone()),
                    base: rust_url,
                    token: read(&rust_token_file)?,
                    deepseek_record: rust_deepseek_record,
                    secretary_space: space_for(&read(&rust_service_token_file)?),
                    timeline: rust_timeline,
                },
                secretary_record,
            };
            let (report, ok) = http::run(&cases, &run).await;
            std::fs::write(&out, serde_json::to_string_pretty(&report)?)?;
            println!("{}", http::summary(&report));
            println!("report: {}", out.display());
            if !ok {
                std::process::exit(1);
            }
        }
        Cmd::ShadowReplay {
            timeline,
            last,
            channels,
            rust_url,
            token_file,
            out,
            show_text,
            pause_ms,
        } => {
            use lucina_server::parity::replay;
            let channels: Vec<String> = channels.split(',').map(|c| c.trim().to_owned()).collect();
            let turns = replay::turns(&timeline, &channels, last)?;
            let token = std::fs::read_to_string(&token_file)?.trim().to_owned();
            let report = replay::run(
                &turns,
                &rust_url,
                &token,
                "Lucina",
                std::time::Duration::from_millis(pause_ms),
            )
            .await;
            {
                use std::io::Write;
                let mut opts = std::fs::OpenOptions::new();
                opts.create(true).write(true).truncate(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                opts.open(&out)?
                    .write_all(serde_json::to_string_pretty(&report)?.as_bytes())?;
            }
            println!("{}", replay::summary(&report, show_text));
            println!("report (with texts): {}", out.display());
        }
        Cmd::BotHospital { dry_run } => {
            use lucina_server::bot_hospital as bh;
            let cfg = Config::from_env()?;
            let hospital = bh::Hospital {
                cfg: cfg.hospital.clone(),
                http: reqwest::Client::new(),
                health: Default::default(),
            };
            if dry_run {
                let state = hospital.fetch_state().await;
                let proof = bh::latest_proof(&state.body["state"]);
                let v = bh::validate(
                    proof.as_ref(),
                    chrono::Utc::now(),
                    cfg.hospital.window_hours,
                );
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "stateHttpStatus": state.status, "ok": v.ok, "missing": v.missing,
                        "warnings": v.warnings,
                        "proof": proof.map(|p| serde_json::json!({"title": p["title"], "postedAt": p["postedAt"], "postedTargets": p["postedTargets"]})),
                    }))?
                );
            } else {
                let state = AppState::build(cfg.clone(), None)?;
                let live = bh::Live {
                    st: state,
                    alert_chat_id: cfg.hospital.alert_chat_id.clone(),
                };
                match hospital.audit(&live, chrono::Utc::now()).await {
                    bh::AuditOutcome::Healthy => println!("healthy"),
                    bh::AuditOutcome::Duplicate(k) => println!("duplicate incident {k}"),
                    bh::AuditOutcome::Incident(b) => println!(
                        "incident {} ({}): {}",
                        b.0.id,
                        b.0.kind,
                        bh::final_message(&b.0, &b.1)
                    ),
                }
            }
        }
        Cmd::CapabilityBlock => {
            println!("{}", Config::from_env()?.caps.block());
        }
        Cmd::TimelineAppendTest { path, count } => {
            let store = lucina_store::TimelineStore::new(&path, u64::MAX / 2, true);
            for i in 0..count {
                store.append(lucina_store::timeline::NewItem {
                    channel: "web".into(),
                    role: "user".into(),
                    text: format!("rust line {i}"),
                    ..Default::default()
                })?;
            }
        }
        Cmd::CheckTimeline {
            path,
            expect_node,
            expect_rust,
        } => {
            let text = std::fs::read_to_string(&path)?;
            let (mut bad, mut node, mut rust, mut dup, mut out_of_order) = (0, 0, 0, 0, 0);
            let mut ids = std::collections::HashSet::new();
            let mut last_rust_id = String::new();
            for line in text.split('\n').filter(|l| !l.is_empty()) {
                match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(v) => {
                        let id = v["id"].as_str().unwrap_or("").to_owned();
                        if !ids.insert(id.clone()) {
                            dup += 1;
                        }
                        let t = v["text"].as_str().unwrap_or("");
                        if t.starts_with("node line ") {
                            node += 1;
                        }
                        if t.starts_with("rust line ") {
                            rust += 1;
                            if id <= last_rust_id {
                                out_of_order += 1;
                            }
                            last_rust_id = id;
                        }
                    }
                    Err(_) => bad += 1,
                }
            }
            let ok = bad == 0
                && dup == 0
                && node == expect_node
                && rust == expect_rust
                && out_of_order == 0
                && text.ends_with('\n');
            println!("check-timeline: {} lines, {bad} broken, {dup} duplicate ids, node {node}/{expect_node}, rust {rust}/{expect_rust}, rust ids out of order {out_of_order}: {}", ids.len(), if ok { "ok" } else { "FAILED" });
            if !ok {
                std::process::exit(1);
            }
        }
        Cmd::CheckData { data } => {
            if !check_data(&data)? {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
