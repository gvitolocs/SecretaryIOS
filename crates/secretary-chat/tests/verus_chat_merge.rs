//! Differential test: `secretary_chat` (production) against the function bodies proved in
//! `verification/verus/chat_merge.rs`, copied below without the Verus annotations (the spec functions as plain
//! recursive Rust, `next` as the exec function). Generated inputs, including the awkward ones (ties, locals in the
//! incoming batch, replies to replaced items, stuck cursors, every error at every step). If one side changes, this
//! fails until the other (and its proof) follows.
use proptest::prelude::*;
use secretary_chat::{
    merge::{merge, Key, Role},
    sync::{Action, Outcome, Phase, SyncPlan},
};
use std::collections::BTreeMap;

/// The proof file's `Item`: ids interned in byte order, 0 means absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Item {
    id: u64,
    ts: i64,
    lucina: bool,
    local: bool,
    client: u64,
    reply: u64,
    share: u64,
}

mod proved_merge {
    use super::Item;

    pub fn key_lt(a: Item, b: Item) -> bool {
        a.ts < b.ts || (a.ts == b.ts && ((!a.lucina && b.lucina) || (a.lucina == b.lucina && a.id < b.id)))
    }

    pub fn hit(x: Item, e: Item) -> bool {
        e.local
            && e.lucina == x.lucina
            && e.id != x.id
            && ((x.client != 0 && x.client == e.client)
                || (x.lucina && x.reply != 0 && x.reply == e.reply)
                || (!x.lucina && x.share != 0 && x.share == e.share))
    }

    pub fn hit_id(m: &[Item], x: Item, id: u64) -> bool {
        m.iter().any(|&e| hit(x, e) && e.id == id)
    }

    pub fn retarget(m: &[Item], x: Item, e: Item) -> Item {
        if !x.lucina && e.local && e.reply != 0 && hit_id(m, x, e.reply) {
            Item { reply: x.id, ..e }
        } else {
            e
        }
    }

    pub fn keep_map(s: &[Item], m: &[Item], x: Item) -> Vec<Item> {
        if s.is_empty() {
            return vec![];
        }
        let e = s[s.len() - 1];
        let mut rest = keep_map(&s[..s.len() - 1], m, x);
        if e.id != x.id && !hit(x, e) {
            rest.push(retarget(m, x, e));
        }
        rest
    }

    pub fn step(m: &[Item], x: Item) -> Vec<Item> {
        let mut v = keep_map(m, m, x);
        v.push(retarget(m, x, x));
        v
    }

    pub fn run(m: &[Item], ord: &[Item]) -> Vec<Item> {
        if ord.is_empty() {
            return m.to_vec();
        }
        step(&run(m, &ord[..ord.len() - 1]), ord[ord.len() - 1])
    }

    pub fn part(s: &[Item], l: bool) -> Vec<Item> {
        if s.is_empty() {
            return vec![];
        }
        let mut r = part(&s[..s.len() - 1], l);
        if s[s.len() - 1].lucina == l {
            r.push(s[s.len() - 1]);
        }
        r
    }

    pub fn ordered(inc: &[Item]) -> Vec<Item> {
        let mut v = part(inc, false);
        v.extend(part(inc, true));
        v
    }

    pub fn ins(s: &[Item], x: Item) -> Vec<Item> {
        if s.is_empty() {
            vec![x]
        } else if key_lt(x, s[0]) {
            let mut v = vec![x];
            v.extend_from_slice(s);
            v
        } else {
            let mut v = vec![s[0]];
            v.extend(ins(&s[1..], x));
            v
        }
    }

    pub fn sort(s: &[Item]) -> Vec<Item> {
        if s.is_empty() {
            return vec![];
        }
        ins(&sort(&s[..s.len() - 1]), s[s.len() - 1])
    }

    pub fn merge(cur: &[Item], inc: &[Item]) -> Vec<Item> {
        sort(&run(cur, &ordered(inc)))
    }

    /// `repl`'s ids with the incoming id that replaced them (the proof file only keeps the ids; the pair is the
    /// same `hit_id` witness, last writer wins like the production map).
    pub fn replaced(m: &[Item], ord: &[Item]) -> std::collections::BTreeMap<u64, u64> {
        let mut out = std::collections::BTreeMap::new();
        let mut cur = m.to_vec();
        for &x in ord {
            for e in &cur {
                if hit(x, *e) {
                    out.insert(e.id, x.id);
                }
            }
            cur = step(&cur, x);
        }
        out
    }
}

mod proved_sync {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Phase {
        Begin,
        Fetching,
        Applying,
        Finished,
        Aborted,
    }
    #[derive(Clone, Copy, Debug)]
    pub enum Outcome {
        Started,
        Page { cursor: u64, more: bool },
        Applied,
        Gone,
        Failed,
    }
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Action {
        Fetch { after: u64, limit: u32 },
        Apply { reset: bool },
        Done,
        Abort,
    }
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Plan {
        pub phase: Phase,
        pub had_cursor: bool,
        pub fresh: bool,
        pub cursor: u64,
        pub pages: u32,
        pub fetches: u32,
        pub pc: u64,
        pub more: bool,
    }

    pub fn new_plan(cursor: u64) -> Plan {
        Plan { phase: Phase::Begin, had_cursor: cursor != 0, fresh: false, cursor, pages: 0, fetches: 0, pc: 0, more: false }
    }

    pub fn next(p: Plan, o: Outcome) -> (Plan, Action) {
        match p.phase {
            Phase::Finished => (p, Action::Done),
            Phase::Aborted => {
                let mut q = p;
                q.phase = Phase::Aborted;
                (q, Action::Abort)
            }
            _ => match o {
                Outcome::Failed => {
                    let mut q = p;
                    q.phase = Phase::Aborted;
                    (q, Action::Abort)
                }
                Outcome::Started => match p.phase {
                    Phase::Begin => {
                        let mut q = p;
                        q.phase = Phase::Fetching;
                        q.fetches = p.fetches + 1;
                        if p.had_cursor {
                            q.pages = p.pages + 1;
                            (q, Action::Fetch { after: p.cursor, limit: 200 })
                        } else {
                            (q, Action::Fetch { after: 0, limit: 50 })
                        }
                    }
                    _ => {
                        let mut q = p;
                        q.phase = Phase::Aborted;
                        (q, Action::Abort)
                    }
                },
                Outcome::Page { cursor, more } => match p.phase {
                    Phase::Fetching => {
                        let mut q = p;
                        q.phase = Phase::Applying;
                        q.pc = cursor;
                        q.more = more;
                        (q, Action::Apply { reset: p.fresh })
                    }
                    _ => {
                        let mut q = p;
                        q.phase = Phase::Aborted;
                        (q, Action::Abort)
                    }
                },
                Outcome::Gone => match p.phase {
                    Phase::Fetching | Phase::Applying => {
                        let mut q = p;
                        if p.had_cursor && !p.fresh {
                            q.phase = Phase::Fetching;
                            q.fresh = true;
                            q.fetches = p.fetches + 1;
                            (q, Action::Fetch { after: 0, limit: 50 })
                        } else {
                            q.phase = Phase::Aborted;
                            (q, Action::Abort)
                        }
                    }
                    _ => {
                        let mut q = p;
                        q.phase = Phase::Aborted;
                        (q, Action::Abort)
                    }
                },
                Outcome::Applied => match p.phase {
                    Phase::Applying => {
                        let mut q = p;
                        if p.fresh || !p.had_cursor || !p.more {
                            q.phase = Phase::Finished;
                            (q, Action::Done)
                        } else if p.pc == 0 || p.pc == p.cursor {
                            q.phase = Phase::Fetching;
                            q.fresh = true;
                            q.fetches = p.fetches + 1;
                            (q, Action::Fetch { after: 0, limit: 50 })
                        } else if p.pages >= 10 {
                            q.cursor = p.pc;
                            q.phase = Phase::Fetching;
                            q.fresh = true;
                            q.fetches = p.fetches + 1;
                            (q, Action::Fetch { after: 0, limit: 50 })
                        } else {
                            q.cursor = p.pc;
                            q.phase = Phase::Fetching;
                            q.pages = p.pages + 1;
                            q.fetches = p.fetches + 1;
                            (q, Action::Fetch { after: p.pc, limit: 200 })
                        }
                    }
                    _ => {
                        let mut q = p;
                        q.phase = Phase::Aborted;
                        (q, Action::Abort)
                    }
                },
            },
        }
    }
}

// ---------------------------------------------------------------- merge

/// The ids of both lists, interned in byte order from 1.
fn intern(all: impl Iterator<Item = String>) -> BTreeMap<String, u64> {
    let mut set: Vec<String> = all.collect();
    set.sort();
    set.dedup();
    set.into_iter().enumerate().map(|(i, s)| (s, i as u64 + 1)).collect()
}

fn to_item(k: &Key, table: &BTreeMap<String, u64>) -> Item {
    let n = |s: &Option<String>| s.as_ref().map_or(0, |s| table[s]);
    Item {
        id: table[&k.id],
        ts: k.ts_us,
        lucina: k.role == Role::Lucina,
        local: k.is_local(),
        client: n(&k.client_message_id),
        reply: n(&k.reply_to),
        share: n(&k.share_id),
    }
}

const SERVER_IDS: [&str; 6] = ["tl_01", "tl_02", "tl_03", "tl_04", "tl_05", "tl_06"];
const LOCAL_IDS: [&str; 5] = ["local-a", "local-b", "local-c", "local-reply-a", "local-reply-b"];
const CLIENTS: [&str; 3] = ["c1", "c2", "c3"];
const SHARES: [&str; 2] = ["s1", "s2"];

fn arb_ref() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        3 => Just(None),
        2 => proptest::sample::select(&SERVER_IDS[..]).prop_map(|s| Some(s.to_string())),
        3 => proptest::sample::select(&LOCAL_IDS[..]).prop_map(|s| Some(s.to_string())),
    ]
}

fn arb_key(pool: &'static [&'static str]) -> impl Strategy<Value = Key> {
    (
        proptest::sample::select(pool),
        0i64..4,
        any::<bool>(),
        proptest::option::of(proptest::sample::select(&CLIENTS[..])),
        arb_ref(),
        proptest::option::of(proptest::sample::select(&SHARES[..])),
    )
        .prop_map(|(id, ts, lucina, client, reply_to, share)| Key {
            id: id.to_string(),
            ts_us: ts,
            role: if lucina { Role::Lucina } else { Role::User },
            client_message_id: client.map(String::from),
            reply_to,
            share_id: share.map(String::from),
        })
}

const ALL_IDS: [&str; 11] = [
    "tl_01",
    "tl_02",
    "tl_03",
    "tl_04",
    "tl_05",
    "tl_06",
    "local-a",
    "local-b",
    "local-c",
    "local-reply-a",
    "local-reply-b",
];

/// A list with distinct ids (what a timeline holds).
fn arb_unique(max: usize) -> impl Strategy<Value = Vec<Key>> {
    proptest::collection::vec(arb_key(&ALL_IDS), 0..=max).prop_map(|v| {
        let mut seen = std::collections::HashSet::new();
        v.into_iter().filter(|k| seen.insert(k.id.clone())).collect()
    })
}

fn arb_incoming(max: usize) -> impl Strategy<Value = Vec<Key>> {
    proptest::collection::vec(arb_key(&ALL_IDS), 0..=max)
}

proptest! {
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::with_cases(4000) })]

    #[test]
    fn merge_matches_the_proved_spec(current in arb_unique(7), incoming in arb_incoming(7)) {
        let table = intern(current.iter().chain(&incoming).flat_map(|k| {
            [Some(k.id.clone()), k.client_message_id.clone(), k.reply_to.clone(), k.share_id.clone()]
        }).flatten());
        let cur: Vec<Item> = current.iter().map(|k| to_item(k, &table)).collect();
        let inc: Vec<Item> = incoming.iter().map(|k| to_item(k, &table)).collect();

        let got = merge(&current, &incoming);
        let want = proved_merge::merge(&cur, &inc);

        // the ids, in order
        let got_ids: Vec<u64> = got.ids.iter().map(|id| table[id]).collect();
        let want_ids: Vec<u64> = want.iter().map(|o| o.id).collect();
        prop_assert_eq!(&got_ids, &want_ids);

        // the replaced map (last writer wins)
        let want_replaced = proved_merge::replaced(&cur, &proved_merge::ordered(&inc));
        let got_replaced: BTreeMap<u64, u64> = got.replaced.iter().map(|r| (table[&r.local], table[&r.server])).collect();
        prop_assert_eq!(&got_replaced, &want_replaced);

        // the reply_to updates: the proved final bodies against the body Swift would pick (the last incoming copy,
        // users before Lucina, else the current one)
        let ord = proved_merge::ordered(&inc);
        let mut want_updates: BTreeMap<u64, u64> = BTreeMap::new();
        for o in &want {
            let base = ord.iter().rev().find(|e| e.id == o.id).or_else(|| cur.iter().find(|e| e.id == o.id)).unwrap();
            prop_assert_eq!((base.id, base.ts, base.lucina, base.client, base.share), (o.id, o.ts, o.lucina, o.client, o.share));
            if o.reply != base.reply {
                want_updates.insert(o.id, o.reply);
            }
        }
        let got_updates: BTreeMap<u64, u64> = got.reply_to_updates.iter().map(|u| (table[&u.id], table[&u.reply_to])).collect();
        prop_assert_eq!(&got_updates, &want_updates);
    }

    #[test]
    fn the_result_is_order_independent_in_current(current in arb_unique(7), incoming in arb_incoming(7), rot in 0usize..8) {
        // M2 on production and on the proof file's own functions
        let mut rotated = current.clone();
        if !rotated.is_empty() {
            let r = rot % rotated.len();
            rotated.rotate_left(r);
            rotated.reverse();
        }
        prop_assert_eq!(merge(&current, &incoming), merge(&rotated, &incoming));
        let table = intern(current.iter().chain(&incoming).flat_map(|k| {
            [Some(k.id.clone()), k.client_message_id.clone(), k.reply_to.clone(), k.share_id.clone()]
        }).flatten());
        let cur: Vec<Item> = current.iter().map(|k| to_item(k, &table)).collect();
        let rot_items: Vec<Item> = rotated.iter().map(|k| to_item(k, &table)).collect();
        let inc: Vec<Item> = incoming.iter().map(|k| to_item(k, &table)).collect();
        prop_assert_eq!(proved_merge::merge(&cur, &inc), proved_merge::merge(&rot_items, &inc));
    }
}

// ---------------------------------------------------------------- sync

fn pc(c: &Option<String>) -> u64 {
    c.as_ref().map_or(0, |s| s[1..].parse::<u64>().unwrap())
}

fn name(n: u64) -> Option<String> {
    (n != 0).then(|| format!("c{n}"))
}

/// One caller report, chosen without looking at the action it answers.
#[derive(Clone, Copy, Debug)]
enum Pick {
    Started,
    Page(u8, bool),
    Applied,
    Gone,
    Failed,
}

fn arb_pick() -> impl Strategy<Value = Pick> {
    prop_oneof![
        1 => Just(Pick::Started),
        6 => (0u8..4, any::<bool>()).prop_map(|(c, m)| Pick::Page(c, m)),
        6 => Just(Pick::Applied),
        2 => Just(Pick::Gone),
        1 => Just(Pick::Failed),
    ]
}

fn outcomes(p: Pick, serial: u64) -> (Outcome, proved_sync::Outcome) {
    match p {
        Pick::Started => (Outcome::Started, proved_sync::Outcome::Started),
        // cursor 0 = missing, 1 = repeats the first cursor, others are fresh names
        Pick::Page(c, more) => {
            let n = match c {
                0 => 0,
                1 => 1,
                _ => serial + 10,
            };
            (Outcome::Page { cursor: name(n), has_more: more }, proved_sync::Outcome::Page { cursor: n, more })
        }
        Pick::Applied => (Outcome::Applied, proved_sync::Outcome::Applied),
        Pick::Gone => (Outcome::Gone410, proved_sync::Outcome::Gone),
        Pick::Failed => (Outcome::Failed, proved_sync::Outcome::Failed),
    }
}

fn same_action(a: &Action, b: proved_sync::Action) -> bool {
    match (a, b) {
        (Action::Fetch { after, limit }, proved_sync::Action::Fetch { after: a2, limit: l2 }) => pc(after) == a2 && *limit == l2,
        (Action::Apply { reset }, proved_sync::Action::Apply { reset: r2 }) => *reset == r2,
        (Action::Done, proved_sync::Action::Done) | (Action::Abort, proved_sync::Action::Abort) => true,
        _ => false,
    }
}

fn same_phase(a: Phase, b: proved_sync::Phase) -> bool {
    matches!(
        (a, b),
        (Phase::Begin, proved_sync::Phase::Begin)
            | (Phase::Fetching, proved_sync::Phase::Fetching)
            | (Phase::Applying, proved_sync::Phase::Applying)
            | (Phase::Finished, proved_sync::Phase::Finished)
            | (Phase::Aborted, proved_sync::Phase::Aborted)
    )
}

fn drive(start: u64, picks: &[Pick]) -> Result<(), TestCaseError> {
    let mut prod = SyncPlan::new(name(start));
    let mut proved = proved_sync::new_plan(start);
    for (i, &pick) in picks.iter().enumerate() {
        let (po, qo) = outcomes(pick, i as u64);
        let a = prod.next(po);
        let (np, b) = proved_sync::next(proved, qo);
        proved = np;
        prop_assert!(same_action(&a, b), "step {i} {pick:?}: {a:?} against {b:?}");
        prop_assert!(same_phase(prod.phase(), proved.phase), "step {i} phase");
        prop_assert_eq!(prod.fetches(), proved.fetches, "step {} fetches", i);
        prop_assert!(proved.fetches <= 11);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::with_cases(6000) })]

    #[test]
    fn sync_matches_the_proved_machine(start in 0u64..3, picks in proptest::collection::vec(arb_pick(), 0..40)) {
        drive(start, &picks)?;
    }
}

#[test]
fn sync_matches_the_proved_machine_on_every_short_script() {
    // every sequence of 6 reports out of 9, from a missing and from a given cursor: 3.2M steps each way
    let alphabet = [
        Pick::Started,
        Pick::Page(0, true),
        Pick::Page(1, true),
        Pick::Page(2, true),
        Pick::Page(2, false),
        Pick::Applied,
        Pick::Gone,
        Pick::Failed,
        Pick::Page(3, true),
    ];
    for start in [0u64, 1] {
        let n = alphabet.len();
        for code in 0..n.pow(6) {
            let mut c = code;
            let picks: Vec<Pick> = (0..6)
                .map(|_| {
                    let p = alphabet[c % n];
                    c /= n;
                    p
                })
                .collect();
            drive(start, &picks).unwrap();
        }
    }
}
