// OTP owner gate: who may receive a code, and Rust Lucina's `/otp` dispatch.
//
// Production (branch lucina-rs, c357646):
//   private_owner_exec <-> crates/lucina-phone-core/src/reply.rs `private_owner`
//   dispatch / secretary_chat_dispatch
//                      <-> crates/lucina-server/src/otp.rs `respond` + `run` (the Take arm)
//                          and `secretary_chat` (Asker{surface:"app", owner_authorized:true, mcp})
// Differential test: verification/verus/diff/otp_owner.rs.
//
// Modelled: the decision exactly as written: surface compared with "app" / "telegram"
// (the Surface enum: App, Telegram, Other for every other string); owner_id, user_id
// and chat_id as byte strings compared byte for byte; "all ASCII digits"; chat_type
// "private" (a bool); owner_authorized, mcp, is_bot.
// Abstracted: the relay call. Its answer to Take is RelayTake (unavailable, 204, or a
// Delivered with `valid_code(code)` and `expires_at` already evaluated); Arm, Disarm and
// Status replies never carry a code and are one `Text` outcome. The clock is `now`.
//
// Proven:
//   O1 private_owner_exec == private_owner (the truth table, exactly).
//   O2 corollaries: an MCP turn on the app, a non-owner, a group chat, a bot, an empty or
//      non-numeric owner id, or any other surface never qualifies.
//   O3 dispatch returns a code only for a private owner, for Take, with a valid code
//      that has not expired; secretary_chat_dispatch returns a code only when !mcp.
//   O4 non-vacuity: the owner's app (not MCP) and the owner's Telegram private chat get
//      the code (witness_* below).
// Note (not a bug in the Rust server, which has no Telegram transport): the Telegram
// branch does not look at `mcp`; a transport passing surface "telegram" with mcp = true
// would be treated as the owner's chat. See O2's telegram_ignores_mcp.
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Surface {
    App,
    Telegram,
    Other,
}

pub struct Asker {
    pub surface: Surface,
    pub owner_authorized: bool,
    pub mcp: bool,
    pub owner_id: Vec<u8>,
    pub user_id: Vec<u8>,
    pub chat_id: Vec<u8>,
    pub chat_private: bool,
    pub is_bot: bool,
}

pub open spec fn all_digits(s: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < s.len() ==> 48 <= #[trigger] s[i] <= 57
}

/// The truth table the gate must implement.
pub open spec fn private_owner(a: Asker) -> bool {
    (a.surface == Surface::App && a.owner_authorized && !a.mcp) || (a.surface == Surface::Telegram
        && a.owner_id@.len() > 0 && all_digits(a.owner_id@) && a.chat_private && a.user_id@
        == a.owner_id@ && a.chat_id@ == a.owner_id@ && !a.is_bot)
}

fn bytes_eq(a: &Vec<u8>, b: &Vec<u8>) -> (r: bool)
    ensures
        r == (a@ == b@),
{
    if a.len() != b.len() {
        return false;
    }
    let mut i: usize = 0;
    while i < a.len()
        invariant
            a.len() == b.len(),
            0 <= i <= a.len(),
            forall|j: int| 0 <= j < i ==> a@[j] == b@[j],
        decreases a.len() - i,
    {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    assert(a@ =~= b@);
    true
}

fn bytes_all_digits(a: &Vec<u8>) -> (r: bool)
    ensures
        r == all_digits(a@),
{
    let mut i: usize = 0;
    while i < a.len()
        invariant
            0 <= i <= a.len(),
            forall|j: int| 0 <= j < i ==> 48 <= #[trigger] a@[j] <= 57,
        decreases a.len() - i,
    {
        if a[i] < 48 || a[i] > 57 {
            return false;
        }
        i += 1;
    }
    true
}

/// `private_owner`, statement for statement.
pub fn private_owner_exec(a: &Asker) -> (r: bool)
    ensures
        r == private_owner(*a),
{
    let (app, telegram) = match a.surface {
        Surface::App => (true, false),
        Surface::Telegram => (false, true),
        Surface::Other => (false, false),
    };
    if app {
        return a.owner_authorized && !a.mcp;
    }
    telegram && a.owner_id.len() > 0 && bytes_all_digits(&a.owner_id)
        && a.chat_private && bytes_eq(&a.user_id, &a.owner_id) && bytes_eq(&a.chat_id, &a.owner_id)
        && !a.is_bot
}

// ---------------------------------------------------------------- O2

proof fn lemma_gate_corollaries(a: Asker)
    ensures
        a.surface == Surface::App && a.mcp ==> !private_owner(a),
        a.surface == Surface::App && !a.owner_authorized ==> !private_owner(a),
        a.surface == Surface::Other ==> !private_owner(a),
        a.surface == Surface::Telegram && !a.chat_private ==> !private_owner(a),
        a.surface == Surface::Telegram && a.is_bot ==> !private_owner(a),
        a.surface == Surface::Telegram && a.owner_id@.len() == 0 ==> !private_owner(a),
        a.surface == Surface::Telegram && a.user_id@ != a.owner_id@ ==> !private_owner(a),
        a.surface == Surface::Telegram && a.chat_id@ != a.owner_id@ ==> !private_owner(a),
        a.surface == Surface::Telegram && !all_digits(a.owner_id@) ==> !private_owner(a),
        // A private owner's Telegram chat id is the owner id: never a group (negative ids).
        a.surface == Surface::Telegram && private_owner(a) ==> all_digits(a.chat_id@),
{
}

/// The observation in the header: on Telegram, `mcp` does not change the answer.
proof fn lemma_telegram_ignores_mcp(a: Asker, b: Asker)
    requires
        a.surface == Surface::Telegram,
        b == (Asker { mcp: !a.mcp, ..a }),
    ensures
        private_owner(a) == private_owner(b),
{
}

// ---------------------------------------------------------------- O3: dispatch

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Action {
    Take,
    Arm,
    Disarm,
    Status,
}

/// What the relay answered to `GET /otp` (and, for the 204, the follow-up status/arm).
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum RelayTake {
    Unavailable,
    Nothing { follow_up_ok: bool },
    Delivered { valid_code: bool, expires_at: u64 },
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Reply {
    NotPrivate,
    Unavailable,
    /// A reply without a code (status, armed, disarmed, waiting).
    Text,
    /// `reply::delivered(&d)` with `expires_at: Some(d.expires_at)`.
    Code { expires_at: u64 },
}

/// `run`: what the relay answers becomes the reply. Non-Take actions never carry a code;
/// `other_ok` is whether their relay call succeeded.
pub open spec fn run_spec(action: Action, relay: RelayTake, other_ok: bool, now: u64) -> Reply {
    match action {
        Action::Take => match relay {
            RelayTake::Unavailable => Reply::Unavailable,
            RelayTake::Nothing { follow_up_ok } => if follow_up_ok { Reply::Text } else { Reply::Unavailable },
            RelayTake::Delivered { valid_code, expires_at } =>
                if valid_code && expires_at > now { Reply::Code { expires_at } } else { Reply::Unavailable },
        },
        _ => if other_ok { Reply::Text } else { Reply::Unavailable },
    }
}

pub fn run(action: Action, relay: RelayTake, other_ok: bool, now: u64) -> (r: Reply)
    ensures
        r == run_spec(action, relay, other_ok, now),
        r is Code ==> action == Action::Take && (relay matches RelayTake::Delivered {
            valid_code,
            expires_at,
        } && valid_code && expires_at > now && r == (Reply::Code { expires_at })),
        r != Reply::NotPrivate,
{
    match action {
        Action::Take => match relay {
            RelayTake::Unavailable => Reply::Unavailable,
            RelayTake::Nothing { follow_up_ok } => if follow_up_ok {
                Reply::Text
            } else {
                Reply::Unavailable
            },
            RelayTake::Delivered { valid_code, expires_at } => {
                if !valid_code {
                    Reply::Unavailable
                } else if expires_at <= now {
                    Reply::Unavailable
                } else {
                    Reply::Code { expires_at }
                }
            },
        },
        _ => if other_ok {
            Reply::Text
        } else {
            Reply::Unavailable
        },
    }
}

/// `respond`: the gate first; the relay is never called for anyone else.
pub fn dispatch(a: &Asker, action: Action, relay: RelayTake, other_ok: bool, now: u64) -> (r: Reply)
    ensures
        r == (if private_owner(*a) { run_spec(action, relay, other_ok, now) } else { Reply::NotPrivate }),
        !private_owner(*a) ==> r == Reply::NotPrivate,
        r is Code ==> private_owner(*a) && action == Action::Take,
        r is Code ==> !(a.surface == Surface::App && a.mcp),
        r is Code ==> (relay matches RelayTake::Delivered { valid_code, expires_at } && valid_code
            && expires_at > now && r == (Reply::Code { expires_at })),
{
    if !private_owner_exec(a) {
        return Reply::NotPrivate;
    }
    run(action, relay, other_ok, now)
}

/// `secretary_chat`: the app surface with the owner's session; `mcp` from the turn.
pub fn secretary_chat_dispatch(mcp: bool, action: Action, relay: RelayTake, other_ok: bool, now: u64) -> (r: Reply)
    ensures
        r == (if mcp { Reply::NotPrivate } else { run_spec(action, relay, other_ok, now) }),
        r is Code ==> !mcp,
        mcp ==> r == Reply::NotPrivate,
{
    let a = Asker {
        surface: Surface::App,
        owner_authorized: true,
        mcp,
        owner_id: Vec::new(),
        user_id: Vec::new(),
        chat_id: Vec::new(),
        chat_private: false,
        is_bot: false,
    };
    if !private_owner_exec(&a) {
        return Reply::NotPrivate;
    }
    dispatch(&a, action, relay, other_ok, now)
}

// ---------------------------------------------------------------- O4: non-vacuity

fn witness_owner_app_gets_code() {
    let r = secretary_chat_dispatch(
        false,
        Action::Take,
        RelayTake::Delivered { valid_code: true, expires_at: 2000 },
        true,
        1000,
    );
    assert(r == (Reply::Code { expires_at: 2000 }));
    let m = secretary_chat_dispatch(
        true,
        Action::Take,
        RelayTake::Delivered { valid_code: true, expires_at: 2000 },
        true,
        1000,
    );
    assert(m == Reply::NotPrivate);
}

fn witness_owner_telegram_gets_code() {
    let mut owner: Vec<u8> = Vec::new();
    owner.push(52);
    owner.push(50);  // "42"
    let a = Asker {
        surface: Surface::Telegram,
        owner_authorized: false,
        mcp: false,
        owner_id: owner.clone(),
        user_id: owner.clone(),
        chat_id: owner,
        chat_private: true,
        is_bot: false,
    };
    assert(all_digits(a.owner_id@));
    let r = dispatch(&a, Action::Take, RelayTake::Delivered { valid_code: true, expires_at: 2000 }, true, 1000);
    assert(r == (Reply::Code { expires_at: 2000 }));
    // The same code, already expired at `now`, is not delivered.
    let late = dispatch(&a, Action::Take, RelayTake::Delivered { valid_code: true, expires_at: 1000 }, true, 1000);
    assert(late == Reply::Unavailable);
}

fn main() {
}

} // verus!
