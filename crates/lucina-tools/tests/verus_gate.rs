//! Differential tests for the confirmation gate against a plain-Rust copy of
//! the Verus kernel `verification/verus/confirmation_gate.rs` (branch
//! verus-proofs): `propose` / `execute_check` vs `agent::call_gate` and a real
//! `agent::run` with a scripted model, `settle` vs `pending::settle`, and the
//! pending store (`create` / `current` / `take`) vs the kernel's store model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use lucina_llm::ScriptedModel;
use lucina_tools::agent::{self, call_gate, AgentConfig, CallGate};
use lucina_tools::pending::{settle, PendingStore, Reply, Settle};
use lucina_tools::{
    is_denied_name, Ctx, Origin, Registry, SideEffect, Tool, ToolEnv, ToolResult, DENY_TERMS,
};
use proptest::prelude::*;
use serde_json::{json, Value};

/// Plain-Rust copy of the Verus kernel.
mod kernel {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Origin {
        Owner,
        Mcp,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Reply {
        Yes,
        No,
        Other,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct ToolInfo {
        pub registered: bool,
        pub needs_confirmation: bool,
        pub denied: bool,
        pub valid_args: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CallOutcome {
        Defer,
        Execute,
        Refuse,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Settle {
        Nothing,
        Run,
        Discard,
        StillPending,
    }

    pub fn execute_check(t: &ToolInfo) -> bool {
        if !t.registered {
            return false;
        }
        if t.denied {
            return false;
        }
        t.valid_args
    }

    pub fn propose(t: &ToolInfo, origin: Origin) -> CallOutcome {
        if t.registered && t.needs_confirmation && !t.denied && t.valid_args {
            match origin {
                Origin::Mcp => CallOutcome::Refuse,
                Origin::Owner => CallOutcome::Defer,
            }
        } else if execute_check(t) {
            CallOutcome::Execute
        } else {
            CallOutcome::Refuse
        }
    }

    pub fn settle(has_current: bool, reply: Reply, origin: Origin) -> Settle {
        if !has_current {
            return Settle::Nothing;
        }
        if let Origin::Mcp = origin {
            return Settle::Nothing;
        }
        match reply {
            Reply::Yes => Settle::Run,
            Reply::No => Settle::Discard,
            Reply::Other => Settle::StillPending,
        }
    }

    /// The store model (`create` / `current` / `take`); like production, the
    /// reads also drop expired entries.
    #[derive(Debug, Clone)]
    pub struct P {
        pub id: String,
        pub session: String,
        pub expires_ms: i64,
    }

    pub fn live(p: &P, now: i64) -> bool {
        now < p.expires_ms
    }

    pub fn create(store: &mut Vec<P>, p: P, now: i64) {
        store.retain(|q| q.session != p.session && live(q, now));
        store.push(p);
    }

    pub fn current(store: &mut Vec<P>, session: &str, now: i64) -> Option<P> {
        store.retain(|q| live(q, now));
        store.iter().rev().find(|q| q.session == session).cloned()
    }

    pub fn take(store: &mut Vec<P>, id: &str) -> Option<P> {
        let pos = store.iter().position(|q| q.id == id)?;
        Some(store.remove(pos))
    }
}

// ------------------------------------------------------------------ a configurable tool

struct Fake {
    name: &'static str,
    side: SideEffect,
    confirm: bool,
    money: bool,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for Fake {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        "test tool"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"x": {"type": "integer"}}, "required": ["x"]})
    }
    fn side_effect(&self) -> SideEffect {
        self.side
    }
    fn confirm(&self) -> bool {
        self.confirm
    }
    fn moves_money(&self) -> bool {
        self.money
    }
    async fn call(&self, _args: Value, _ctx: &Ctx) -> ToolResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"ok": true}))
    }
}

fn to_origin(o: kernel::Origin) -> Origin {
    match o {
        kernel::Origin::Owner => Origin::Owner,
        kernel::Origin::Mcp => Origin::Mcp,
    }
}

#[derive(Debug, Clone)]
struct Case {
    name: &'static str,
    side: SideEffect,
    confirm: bool,
    money: bool,
    valid: bool,
    origin: kernel::Origin,
}

fn case() -> impl Strategy<Value = Case> {
    (
        prop_oneof![
            Just("lookup"),
            Just("post_message"),
            Just("send_money"),
            Just("buyTicket"),
            Just("IBANTransfer"),
            Just("notes_x")
        ],
        prop_oneof![
            Just(SideEffect::None),
            Just(SideEffect::Local),
            Just(SideEffect::External),
            Just(SideEffect::Device)
        ],
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        prop_oneof![Just(kernel::Origin::Owner), Just(kernel::Origin::Mcp)],
    )
        .prop_map(|(name, side, confirm, money, valid, origin)| Case {
            name,
            side,
            confirm,
            money,
            valid,
            origin,
        })
}

fn env() -> (tempfile::TempDir, Arc<ToolEnv>) {
    let dir = tempfile::tempdir().unwrap();
    let env = ToolEnv::new(dir.path(), chrono_tz::Europe::Copenhagen);
    (dir, Arc::new(env))
}

/// Run one proposed call through the real tool loop.
fn run_case(c: &Case) -> (kernel::CallOutcome, kernel::CallOutcome, CallGate) {
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = Fake {
        name: c.name,
        side: c.side,
        confirm: c.confirm,
        money: c.money,
        calls: calls.clone(),
    };
    let needs = tool.needs_confirmation(&json!({}));
    let denied = is_denied_name(c.name) || c.money;
    let mut reg = Registry::new();
    let registered = reg.register(Arc::new(tool)).is_ok();
    assert_eq!(
        registered, !denied,
        "the registry refuses exactly the denied tools"
    );
    let args = if c.valid {
        json!({"x": 1})
    } else {
        json!({"x": "no"})
    };
    let info = kernel::ToolInfo {
        registered,
        needs_confirmation: needs,
        denied,
        valid_args: c.valid,
    };
    let expected = kernel::propose(&info, c.origin);
    let gate = call_gate(&reg, c.name, &args, to_origin(c.origin));

    let model = ScriptedModel::new();
    model.push_calls(&[(c.name, args)]);
    model.push_text("done");
    let (_dir, env) = env();
    let ctx = Ctx::new(env, "web", "s").with_origin(to_origin(c.origin));
    let store = PendingStore::memory(lucina_tools::pending::DEFAULT_TTL);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt
        .block_on(agent::run(
            &model,
            &reg,
            Vec::new(),
            &ctx,
            &store,
            &AgentConfig::default(),
            &|_| {},
        ))
        .unwrap();
    let rec = &out.records[0];
    let observed = if calls.load(Ordering::SeqCst) == 1 {
        assert!(rec.ok && rec.pending.is_none());
        kernel::CallOutcome::Execute
    } else if rec.pending.is_some() {
        assert!(store.current("s", ctx.now).is_some());
        kernel::CallOutcome::Defer
    } else {
        assert!(!rec.ok);
        assert!(store.current("s", ctx.now).is_none());
        kernel::CallOutcome::Refuse
    };
    (expected, observed, gate)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    /// propose (kernel) == what the tool loop really does with the call.
    #[test]
    fn proposals_match_kernel(c in case()) {
        let (expected, observed, gate) = run_case(&c);
        prop_assert_eq!(expected, observed, "{:?} gate {:?}", c, gate);
        let gate_as = match gate {
            CallGate::Defer => kernel::CallOutcome::Defer,
            CallGate::RefuseMcp => kernel::CallOutcome::Refuse,
            CallGate::Execute => observed,
        };
        prop_assert_eq!(gate_as, expected);
    }

    /// Kernel store model == PendingStore under random create/current/take
    /// sequences with a moving clock.
    #[test]
    fn pending_store_matches_kernel(ops in proptest::collection::vec((0u8..4, 0u8..3, 0u32..15), 1..40)) {
        let store = PendingStore::memory(lucina_tools::pending::DEFAULT_TTL);
        let mut model: Vec<kernel::P> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let base: DateTime<Utc> = "2026-10-22T10:00:00Z".parse().unwrap();
        let mut now = base;
        let ms = |t: DateTime<Utc>| t.timestamp_millis();
        for (op, session, n) in ops {
            let s = format!("s{session}");
            match op {
                0 => {
                    let p = store.create(&s, "t", json!({}), "x", now);
                    let exp = lucina_store::timeutil::parse_js_date(&p.expires_at).unwrap();
                    kernel::create(&mut model, kernel::P { id: p.id.clone(), session: s, expires_ms: ms(exp) }, ms(now));
                    ids.push(p.id);
                }
                1 => now += Duration::minutes(i64::from(n)),
                2 => {
                    let a = store.current(&s, now).map(|p| p.id);
                    let b = kernel::current(&mut model, &s, ms(now)).map(|p| p.id);
                    prop_assert_eq!(a, b);
                }
                _ => {
                    let id = ids.get(n as usize % ids.len().max(1)).cloned().unwrap_or_default();
                    let a = store.take(&id).map(|p| p.id);
                    let b = kernel::take(&mut model, &id).map(|p| p.id);
                    prop_assert_eq!(a, b);
                }
            }
        }
    }

    /// A deny-listed word is caught in any naming style.
    #[test]
    fn deny_terms_in_any_style(term in proptest::sample::select(DENY_TERMS), lead in "[a-z]{2,6}", style in 0u8..6) {
        let cap = |w: &str| format!("{}{}", w[..1].to_ascii_uppercase(), &w[1..]);
        let name = match style {
            0 => format!("{lead}_{term}"),
            1 => format!("{lead}-{term}"),
            2 => format!("{lead}{}", cap(term)),
            3 => format!("{}{}", cap(&lead), cap(term)),
            4 => format!("{}_{}", lead.to_uppercase(), term.to_uppercase()),
            _ => format!("{}{}", term.to_uppercase(), cap(&lead)),
        };
        prop_assert!(is_denied_name(&name), "{name}");
    }
}

#[test]
fn settle_matches_kernel_exhaustively() {
    for has in [false, true] {
        for (r, kr) in [
            (Reply::Yes, kernel::Reply::Yes),
            (Reply::No, kernel::Reply::No),
            (Reply::Other, kernel::Reply::Other),
        ] {
            for ko in [kernel::Origin::Owner, kernel::Origin::Mcp] {
                let k = kernel::settle(has, kr, ko);
                let p = match settle(has, r, to_origin(ko)) {
                    Settle::Nothing => kernel::Settle::Nothing,
                    Settle::Run => kernel::Settle::Run,
                    Settle::Discard => kernel::Settle::Discard,
                    Settle::StillPending => kernel::Settle::StillPending,
                };
                assert_eq!(k, p, "{has} {kr:?} {ko:?}");
            }
        }
    }
}

#[test]
fn built_in_tools_are_not_denied() {
    let all = Registry::builtin(&lucina_tools::ToolSelection::All);
    // 12 + tasks + secrets_status + secrets_audit + ask_claude/grok/chatgpt + delegate;
    // `meshy` is registered by the server, not here.
    assert_eq!(all.names().len(), 19, "the 19 built-in tools of lib.rs");
    for n in all.names() {
        assert!(!is_denied_name(n), "{n}");
    }
    // the camelCase / acronym gaps of the old token split
    for n in [
        "sendMoney",
        "buyNow",
        "IBANTransfer",
        "makePayment",
        "PayPal",
        "cryptoWallet2",
    ] {
        assert!(is_denied_name(n), "{n}");
    }
}
