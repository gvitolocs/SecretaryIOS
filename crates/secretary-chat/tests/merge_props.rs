//! Property tests of `secretary_chat::merge` against a direct port of the Swift loop (`TimelineMerge.merge`), whose
//! dictionary iteration order is shuffled on every run: the result must not depend on it.
use proptest::prelude::*;
use secretary_chat::merge::{merge, Key, MergeResult, Role};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The bodies of the items in the result, the way the Swift caller assembles them: the last incoming copy of an id
/// (users before Lucina), else the current one, then the `reply_to` updates.
fn assemble(current: &[Key], incoming: &[Key], r: &MergeResult) -> Vec<Key> {
    let mut bodies: HashMap<&str, &Key> = HashMap::new();
    for k in current {
        bodies.insert(&k.id, k);
    }
    for k in incoming
        .iter()
        .filter(|k| k.role == Role::User)
        .chain(incoming.iter().filter(|k| k.role == Role::Lucina))
    {
        bodies.insert(&k.id, k);
    }
    let updates: HashMap<&str, &str> = r
        .reply_to_updates
        .iter()
        .map(|u| (u.id.as_str(), u.reply_to.as_str()))
        .collect();
    r.ids
        .iter()
        .map(|id| {
            let mut k = bodies[id.as_str()].clone();
            if let Some(new) = updates.get(id.as_str()) {
                k.reply_to = Some(new.to_string());
            }
            k
        })
        .collect()
}

/// `TimelineMerge.merge` as written in Swift, visiting the dictionary's values in the order `seed` gives.
fn swift_merge(
    current: &[Key],
    incoming: &[Key],
    seed: u64,
) -> (Vec<Key>, BTreeMap<String, String>) {
    fn shuffle<T>(v: &mut [T], seed: u64) {
        let mut s = seed | 1;
        for i in (1..v.len()).rev() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            v.swap(i, (s % (i as u64 + 1)) as usize);
        }
    }
    let mut by_id: HashMap<String, Key> = HashMap::new();
    for k in current {
        by_id.insert(k.id.clone(), k.clone());
    }
    let mut replaced = BTreeMap::new();
    let ordered: Vec<&Key> = incoming
        .iter()
        .filter(|k| k.role == Role::User)
        .chain(incoming.iter().filter(|k| k.role == Role::Lucina))
        .collect();
    let mut round = 0u64;
    for item in ordered {
        by_id.insert(item.id.clone(), item.clone());
        // `for local in byId.values`: a snapshot, visited in no particular order
        let mut snapshot: Vec<Key> = by_id.values().cloned().collect();
        round += 1;
        shuffle(&mut snapshot, seed.wrapping_add(round));
        for local in snapshot {
            if !(local.is_local() && local.role == item.role && local.id != item.id) {
                continue;
            }
            let same_client = item.client_message_id.is_some()
                && item.client_message_id == local.client_message_id;
            let same_reply = item.role == Role::Lucina
                && item.reply_to.is_some()
                && item.reply_to == local.reply_to;
            let same_share = item.role == Role::User
                && item.share_id.is_some()
                && item.share_id == local.share_id;
            if same_client || same_reply || same_share {
                by_id.remove(&local.id);
                replaced.insert(local.id.clone(), item.id.clone());
                if item.role == Role::User {
                    // `for (id, other) in byId where other.isLocal && other.replyTo == local.id`
                    let mut snap: Vec<(String, Key)> =
                        by_id.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
                    round += 1;
                    shuffle(&mut snap, seed.wrapping_add(round));
                    for (id, other) in snap {
                        if other.is_local() && other.reply_to.as_deref() == Some(local.id.as_str())
                        {
                            by_id.get_mut(&id).unwrap().reply_to = Some(item.id.clone());
                        }
                    }
                }
            }
        }
    }
    let mut items: Vec<Key> = by_id.into_values().collect();
    items.sort_by(|a, b| (a.ts_us, a.role, &a.id).cmp(&(b.ts_us, b.role, &b.id)));
    (items, replaced)
}

const IDS: [&str; 10] = [
    "tl_1", "tl_2", "tl_3", "tl_4", "tl_5", "local-a", "local-b", "local-c", "local-ra", "local-rb",
];
const SERVER: [&str; 5] = ["tl_1", "tl_2", "tl_3", "tl_4", "tl_5"];
const CLIENTS: [&str; 3] = ["c1", "c2", "c3"];
const SHARES: [&str; 2] = ["s1", "s2"];

fn arb_ref() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        3 => Just(None),
        3 => proptest::sample::select(&IDS[..]).prop_map(|s| Some(s.to_string())),
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

fn unique(v: Vec<Key>) -> Vec<Key> {
    let mut seen = BTreeSet::new();
    v.into_iter()
        .filter(|k| seen.insert(k.id.clone()))
        .collect()
}

fn arb_current() -> impl Strategy<Value = Vec<Key>> {
    proptest::collection::vec(arb_key(&IDS), 0..8).prop_map(unique)
}
fn arb_incoming() -> impl Strategy<Value = Vec<Key>> {
    proptest::collection::vec(arb_key(&IDS), 0..8)
}
/// A server page: distinct non-local ids.
fn arb_page() -> impl Strategy<Value = Vec<Key>> {
    proptest::collection::vec(arb_key(&SERVER), 0..6).prop_map(unique)
}

proptest! {
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::with_cases(5000) })]

    /// The port of the Swift loop agrees with the kernel, whatever order it visits the dictionary in.
    #[test]
    fn agrees_with_the_swift_loop_in_any_dictionary_order(current in arb_current(), incoming in arb_incoming(), seed in any::<u64>()) {
        let got = merge(&current, &incoming);
        let (items, replaced) = swift_merge(&current, &incoming, seed);
        prop_assert_eq!(&got.ids, &items.iter().map(|k| k.id.clone()).collect::<Vec<_>>());
        prop_assert_eq!(assemble(&current, &incoming, &got), items);
        let got_replaced: BTreeMap<String, String> = got.replaced.iter().map(|r| (r.local.clone(), r.server.clone())).collect();
        prop_assert_eq!(got_replaced, replaced);
    }

    #[test]
    fn no_duplicate_ids_and_sorted_by_the_key(current in arb_current(), incoming in arb_incoming()) {
        let r = merge(&current, &incoming);
        let items = assemble(&current, &incoming, &r);
        let ids: BTreeSet<&String> = r.ids.iter().collect();
        prop_assert_eq!(ids.len(), r.ids.len());
        for w in items.windows(2) {
            prop_assert!((w[0].ts_us, w[0].role, &w[0].id) < (w[1].ts_us, w[1].role, &w[1].id));
        }
    }

    #[test]
    fn the_result_does_not_depend_on_the_input_order_of_current(current in arb_current(), incoming in arb_incoming(), rot in 0usize..8) {
        let mut shuffled = current.clone();
        if !shuffled.is_empty() {
            let r = rot % shuffled.len();
            shuffled.rotate_left(r);
            shuffled.reverse();
        }
        prop_assert_eq!(merge(&current, &incoming), merge(&shuffled, &incoming));
    }

    /// M3: merging a server page again changes nothing (items, bodies and order).
    #[test]
    fn merge_is_idempotent_for_a_page_of_server_items(current in arb_current(), page in arb_page()) {
        let first = merge(&current, &page);
        let bodies = assemble(&current, &page, &first);
        let second = merge(&bodies, &page);
        prop_assert_eq!(&second.ids, &first.ids);
        prop_assert_eq!(assemble(&bodies, &page, &second), bodies);
        prop_assert!(second.replaced.is_empty());
        prop_assert!(second.reply_to_updates.is_empty());
    }

    /// M4: every incoming id is in the result unless it is a local item a later incoming item replaced, and a
    /// current item disappears only as a recorded replacement of a local item.
    #[test]
    fn nothing_is_lost_except_recorded_replacements(current in arb_current(), incoming in arb_incoming()) {
        let r = merge(&current, &incoming);
        let kept: BTreeSet<&String> = r.ids.iter().collect();
        let replaced: BTreeMap<&str, &str> = r.replaced.iter().map(|p| (p.local.as_str(), p.server.as_str())).collect();
        for k in current.iter().chain(&incoming) {
            if !kept.contains(&k.id) {
                prop_assert!(k.is_local(), "{} vanished without being local", k.id);
                prop_assert!(replaced.contains_key(k.id.as_str()), "{} vanished without a record", k.id);
            }
        }
        // a recorded replacement points at an incoming item, and the local item's id differs from it
        for p in &r.replaced {
            prop_assert!(incoming.iter().any(|x| x.id == p.server));
            prop_assert!(p.local.starts_with("local-"));
            prop_assert!(p.local != p.server);
        }
    }

    #[test]
    fn non_local_items_always_survive(current in arb_current(), incoming in arb_incoming()) {
        let r = merge(&current, &incoming);
        let kept: BTreeSet<&String> = r.ids.iter().collect();
        for k in current.iter().chain(&incoming).filter(|k| !k.is_local()) {
            prop_assert!(kept.contains(&k.id));
        }
    }
}
