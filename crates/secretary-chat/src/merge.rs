//! `TimelineMerge.merge` of the iPhone app (`ios/Secretary/Features/Chat/Timeline.swift`) on key records.
//!
//! The Swift side passes one small [`Key`] per timeline item (never the message bodies) and gets back the
//! ordered ids plus the local-to-server replacements. The rules, in the order Swift applies them:
//!
//! 1. Start from `current`, keyed by id (a repeated id: the last one wins).
//! 2. Process `incoming` with every `user` item first, then every `lucina` item (each group in input order).
//!    For each incoming item `x`:
//!    - `byId[x.id] = x` (the incoming copy wins);
//!    - every **local** item `l` (id starts with `local-`) of the same role with another id is replaced by `x`
//!      when `x.client_message_id` equals `l`'s, or (`x` from Lucina) `x.reply_to` equals `l`'s, or (`x` from the
//!      user, a share) `x.share_id` equals `l`'s. The match is decided on the map as it was after `x` went in,
//!      so it never depends on the order the locals are visited in. A replaced `l` is removed and recorded in
//!      `replaced`; when `x` is a user item every **local** item whose `reply_to` was `l.id` now points at `x.id`.
//! 3. Output order: `ts_us` ascending, the user before Lucina, then the id in byte order.
//!
//! Proved in `verification/verus/chat_merge.rs` (M1-M4) on the same rules over interned `u64` ids; the
//! differential test `tests/verus_chat_merge.rs` runs this code against the proof file's function bodies.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The prefix of an optimistic bubble's id (`TimelineItem.localPrefix`).
pub const LOCAL_PREFIX: &str = "local-";

/// Who wrote the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Sorts before `Lucina` on a tie.
    User,
    Lucina,
}

/// What the merge needs to know of a timeline item.
///
/// `share_id` is `meta.shareId` of a **share** item (`meta.isShare == true`) and of a local item that has one; it
/// is `None` for any other item, so a server item that merely carries a `shareId` never matches by it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub id: String,
    /// The item's timestamp in microseconds since the epoch.
    pub ts_us: i64,
    pub role: Role,
    #[serde(default)]
    pub client_message_id: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub share_id: Option<String>,
}

impl Key {
    pub fn is_local(&self) -> bool {
        self.id.starts_with(LOCAL_PREFIX)
    }
}

/// A local item replaced by the server's copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replaced {
    pub local: String,
    pub server: String,
}

/// A kept local item whose `reply_to` now points at another id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyUpdate {
    pub id: String,
    pub reply_to: String,
}

/// The merge result. `ids` is the new display order, oldest first. A body in the result is the last incoming copy
/// of its id (users before Lucina) or else the current one; `reply_to_updates` then change `reply_to` on those
/// bodies. `replaced` and `reply_to_updates` are sorted by id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeResult {
    pub ids: Vec<String>,
    pub replaced: Vec<Replaced>,
    pub reply_to_updates: Vec<ReplyUpdate>,
}

/// An entry of the working map: the key plus its current `reply_to` and the one it was inserted with.
struct Entry<'a> {
    key: &'a Key,
    reply_to: Option<&'a str>,
}

impl Entry<'_> {
    fn new(key: &Key) -> Entry<'_> {
        Entry {
            key,
            reply_to: key.reply_to.as_deref(),
        }
    }
}

/// Is the local entry `l` replaced by the incoming `x`? (`x` is already in the map; `l.id != x.id` is checked here.)
fn hit(x: &Key, l: &Entry<'_>) -> bool {
    let lk = l.key;
    if lk.role != x.role || lk.id == x.id {
        return false;
    }
    let same_client = x.client_message_id.is_some() && x.client_message_id == lk.client_message_id;
    let same_reply =
        x.role == Role::Lucina && x.reply_to.is_some() && x.reply_to.as_deref() == l.reply_to;
    let same_share = x.role == Role::User && x.share_id.is_some() && x.share_id == lk.share_id;
    same_client || same_reply || same_share
}

/// The display order: time, the user before Lucina, then the id.
pub fn order(a: &Key, b: &Key) -> std::cmp::Ordering {
    (a.ts_us, a.role, &a.id).cmp(&(b.ts_us, b.role, &b.id))
}

/// `TimelineMerge.merge(current, incoming)`.
pub fn merge(current: &[Key], incoming: &[Key]) -> MergeResult {
    let mut by_id: BTreeMap<&str, Entry<'_>> = BTreeMap::new();
    let mut locals: BTreeSet<&str> = BTreeSet::new();
    for k in current {
        by_id.insert(&k.id, Entry::new(k));
        if k.is_local() {
            locals.insert(&k.id);
        }
    }
    let mut replaced: BTreeMap<&str, &str> = BTreeMap::new();

    let users = incoming.iter().filter(|k| k.role == Role::User);
    let lucinas = incoming.iter().filter(|k| k.role == Role::Lucina);
    for x in users.chain(lucinas) {
        by_id.insert(&x.id, Entry::new(x));
        if x.is_local() {
            locals.insert(&x.id);
        }
        // Decide the whole set first (the map as it stands with `x` in), then apply it.
        let hits: Vec<&str> = locals
            .iter()
            .copied()
            .filter(|id| hit(x, &by_id[id]))
            .collect();
        for l in hits {
            by_id.remove(l);
            locals.remove(l);
            replaced.insert(l, &x.id);
            if x.role == Role::User {
                for id in &locals {
                    let other = by_id.get_mut(id).expect("locals are a subset of the map");
                    if other.reply_to == Some(l) {
                        other.reply_to = Some(&x.id);
                    }
                }
            }
        }
    }

    let mut entries: Vec<&Entry<'_>> = by_id.values().collect();
    entries.sort_by(|a, b| order(a.key, b.key));
    MergeResult {
        ids: entries.iter().map(|e| e.key.id.clone()).collect(),
        replaced: replaced
            .into_iter()
            .map(|(l, s)| Replaced {
                local: l.to_string(),
                server: s.to_string(),
            })
            .collect(),
        reply_to_updates: by_id
            .values()
            .filter(|e| e.reply_to != e.key.reply_to.as_deref())
            .filter_map(|e| {
                e.reply_to.map(|r| ReplyUpdate {
                    id: e.key.id.clone(),
                    reply_to: r.to_string(),
                })
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str, secs: i64, role: Role) -> Key {
        Key {
            id: id.into(),
            ts_us: secs * 1_000_000,
            role,
            client_message_id: None,
            reply_to: None,
            share_id: None,
        }
    }
    fn user(id: &str, secs: i64) -> Key {
        key(id, secs, Role::User)
    }
    fn lucina(id: &str, secs: i64) -> Key {
        key(id, secs, Role::Lucina)
    }
    fn client(mut k: Key, c: &str) -> Key {
        k.client_message_id = Some(c.into());
        k
    }
    fn reply(mut k: Key, r: &str) -> Key {
        k.reply_to = Some(r.into());
        k
    }
    fn share(mut k: Key, s: &str) -> Key {
        k.share_id = Some(s.into());
        k
    }
    fn ids(r: &MergeResult) -> Vec<&str> {
        r.ids.iter().map(String::as_str).collect()
    }
    fn replaced(r: &MergeResult) -> Vec<(&str, &str)> {
        r.replaced
            .iter()
            .map(|p| (p.local.as_str(), p.server.as_str()))
            .collect()
    }

    // Ported from ios/SecretaryTests/TimelineTests.swift (TimelineMergeTests).

    #[test]
    fn dedupe_by_id_and_order_by_time() {
        let current = [user("a", 10), lucina("b", 20)];
        let incoming = [user("c", 15), lucina("b", 20), user("a", 10)];
        let r = merge(&current, &incoming);
        assert_eq!(ids(&r), ["a", "c", "b"]);
        assert!(r.replaced.is_empty());
        assert!(r.reply_to_updates.is_empty());
    }

    #[test]
    fn user_before_lucina_on_the_same_timestamp() {
        let r = merge(&[], &[lucina("z-lucina", 5), user("a-user", 5)]);
        assert_eq!(ids(&r), ["a-user", "z-lucina"]);
        let r = merge(&[], &[lucina("a-lucina", 5), user("z-user", 5)]);
        assert_eq!(
            ids(&r),
            ["z-user", "a-lucina"],
            "the role decides before the id"
        );
    }

    #[test]
    fn the_id_breaks_a_tie_in_byte_order() {
        let r = merge(&[], &[user("b", 5), user("B", 5), user("a", 5)]);
        assert_eq!(ids(&r), ["B", "a", "b"]);
    }

    #[test]
    fn server_copy_replaces_the_optimistic_bubble_by_client_message_id() {
        let optimistic = client(user("local-c1", 100), "c1");
        let server = client(user("tl_10", 101), "c1");
        let r = merge(&[optimistic], &[server]);
        assert_eq!(ids(&r), ["tl_10"]);
        assert_eq!(replaced(&r), [("local-c1", "tl_10")]);
    }

    #[test]
    fn local_reply_is_reconciled_after_its_user_item_in_the_same_batch() {
        // An older server gave no item ids: both bubbles are local placeholders.
        let u = client(user("local-c1", 100), "c1");
        let mut rep = reply(lucina("local-reply-c1", 100), "local-c1");
        rep.ts_us += 1_000;
        // The stream sends Lucina's item (no clientMessageId) before the user's.
        let server_reply = reply(lucina("tl_11", 101), "tl_10");
        let mut server_user = client(user("tl_10", 100), "c1");
        server_user.ts_us += 500_000;
        let r = merge(&[u, rep], &[server_reply, server_user]);
        assert_eq!(ids(&r), ["tl_10", "tl_11"]);
        assert_eq!(
            replaced(&r),
            [("local-c1", "tl_10"), ("local-reply-c1", "tl_11")]
        );
    }

    #[test]
    fn other_channels_with_the_same_text_are_not_merged() {
        // The Swift key has no text and no channel: a telegram item carries no clientMessageId, so nothing matches.
        let mine = client(user("local-c1", 100), "c1");
        let telegram = user("tl_5", 100);
        let r = merge(&[mine], &[telegram]);
        let mut got = r.ids.clone();
        got.sort();
        assert_eq!(got, ["local-c1", "tl_5"]);
        assert!(r.replaced.is_empty());
    }

    // The rules the Swift tests do not name.

    #[test]
    fn a_local_reply_is_repointed_at_the_server_user_id() {
        let u = client(user("local-c1", 100), "c1");
        let rep = reply(lucina("local-reply-c1", 101), "local-c1");
        let r = merge(&[u, rep], &[client(user("tl_10", 100), "c1")]);
        assert_eq!(ids(&r), ["tl_10", "local-reply-c1"]);
        assert_eq!(replaced(&r), [("local-c1", "tl_10")]);
        assert_eq!(
            r.reply_to_updates,
            [ReplyUpdate {
                id: "local-reply-c1".into(),
                reply_to: "tl_10".into()
            }]
        );
    }

    #[test]
    fn lucina_matches_a_local_placeholder_by_reply_to_after_the_repoint() {
        // The placeholder's reply_to is local-c1; the user item (first) repoints it to tl_10, then Lucina's
        // item with reply_to tl_10 replaces it, whatever the input order.
        let u = client(user("local-c1", 100), "c1");
        let rep = reply(lucina("local-reply-c1", 101), "local-c1");
        let server_user = client(user("tl_10", 100), "c1");
        let server_reply = reply(lucina("tl_11", 101), "tl_10");
        for incoming in [
            [server_user.clone(), server_reply.clone()],
            [server_reply, server_user],
        ] {
            let r = merge(&[u.clone(), rep.clone()], &incoming);
            assert_eq!(ids(&r), ["tl_10", "tl_11"]);
            assert_eq!(
                replaced(&r),
                [("local-c1", "tl_10"), ("local-reply-c1", "tl_11")]
            );
            assert!(r.reply_to_updates.is_empty(), "the updated item is gone");
        }
    }

    #[test]
    fn lucina_replaces_a_placeholder_by_client_message_id_too() {
        let rep = client(lucina("local-reply-c1", 100), "c1");
        let r = merge(&[rep], &[client(lucina("tl_11", 101), "c1")]);
        assert_eq!(ids(&r), ["tl_11"]);
        assert_eq!(replaced(&r), [("local-reply-c1", "tl_11")]);
    }

    #[test]
    fn a_share_item_replaces_the_local_bubble_by_share_id() {
        let local = share(user("local-share-s1", 100), "s1");
        let server = share(user("tl_20", 101), "s1");
        let r = merge(std::slice::from_ref(&local), &[server]);
        assert_eq!(ids(&r), ["tl_20"]);
        assert_eq!(replaced(&r), [("local-share-s1", "tl_20")]);
        // A server item without share_id (not a share) never matches by it.
        let r = merge(&[local], &[user("tl_21", 101)]);
        assert_eq!(ids(&r), ["local-share-s1", "tl_21"]);
        assert!(r.replaced.is_empty());
    }

    #[test]
    fn share_matching_is_for_user_items_only() {
        let local = share(lucina("local-x", 100), "s1");
        let r = merge(&[local], &[share(lucina("tl_1", 101), "s1")]);
        assert_eq!(ids(&r), ["local-x", "tl_1"]);
    }

    #[test]
    fn roles_must_agree() {
        let local = client(user("local-c1", 100), "c1");
        let r = merge(&[local], &[client(lucina("tl_1", 101), "c1")]);
        assert_eq!(ids(&r), ["local-c1", "tl_1"]);
        assert!(r.replaced.is_empty());
    }

    #[test]
    fn only_local_items_are_replaced() {
        let server_old = client(user("tl_1", 100), "c1");
        let r = merge(&[server_old], &[client(user("tl_2", 101), "c1")]);
        assert_eq!(ids(&r), ["tl_1", "tl_2"]);
    }

    #[test]
    fn one_server_item_replaces_every_matching_local() {
        let a = client(user("local-a", 100), "c1");
        let b = client(user("local-b", 101), "c1");
        let r = merge(&[a, b], &[client(user("tl_1", 102), "c1")]);
        assert_eq!(ids(&r), ["tl_1"]);
        assert_eq!(replaced(&r), [("local-a", "tl_1"), ("local-b", "tl_1")]);
    }

    #[test]
    fn replies_to_every_replaced_local_are_repointed() {
        let a = client(user("local-a", 100), "c1");
        let b = client(user("local-b", 101), "c1");
        let ra = reply(lucina("local-ra", 102), "local-a");
        let rb = reply(lucina("local-rb", 103), "local-b");
        let r = merge(&[a, b, ra, rb], &[client(user("tl_1", 99), "c1")]);
        assert_eq!(ids(&r), ["tl_1", "local-ra", "local-rb"]);
        assert_eq!(
            r.reply_to_updates,
            [
                ReplyUpdate {
                    id: "local-ra".into(),
                    reply_to: "tl_1".into()
                },
                ReplyUpdate {
                    id: "local-rb".into(),
                    reply_to: "tl_1".into()
                },
            ]
        );
    }

    #[test]
    fn only_local_items_have_their_reply_to_repointed() {
        let u = client(user("local-c1", 100), "c1");
        let server_reply = reply(lucina("tl_9", 101), "local-c1");
        let r = merge(&[u, server_reply], &[client(user("tl_10", 100), "c1")]);
        assert!(r.reply_to_updates.is_empty());
    }

    #[test]
    fn the_incoming_copy_wins_and_resets_a_repointed_reply() {
        // The current copy of the local reply is repointed, but the incoming batch carries the same id again.
        let u = client(user("local-c1", 100), "c1");
        let rep = reply(lucina("local-r", 101), "local-c1");
        let r = merge(&[u, rep.clone()], &[client(user("tl_10", 100), "c1"), rep]);
        assert_eq!(ids(&r), ["tl_10", "local-r"]);
        // Users go first, so the placeholder is repointed, then the incoming copy of it overwrites that: the
        // body Swift keeps is the incoming one and no update is reported on top of it.
        assert!(r.reply_to_updates.is_empty());
    }

    #[test]
    fn duplicate_ids_in_current_keep_the_last_one() {
        let mut first = user("a", 0);
        first.ts_us = 1;
        let mut second = user("a", 0);
        second.ts_us = 9_000_000;
        let r = merge(&[first, second, user("b", 2)], &[]);
        assert_eq!(ids(&r), ["b", "a"]);
    }

    #[test]
    fn a_local_in_the_incoming_batch_can_be_replaced_by_a_later_one() {
        // The hydrate path merges in-memory items (locals included) as the incoming batch.
        let a = client(user("local-a", 100), "c1");
        let s = client(user("tl_1", 101), "c1");
        // The local goes in after the server copy: nothing replaces it (the server copy is not local).
        let r = merge(&[], &[s.clone(), a.clone()]);
        assert_eq!(ids(&r), ["local-a", "tl_1"]);
        // The server copy goes in after the local one: it replaces it.
        let r = merge(&[], &[a, s]);
        assert_eq!(ids(&r), ["tl_1"]);
        assert_eq!(replaced(&r), [("local-a", "tl_1")]);
    }

    #[test]
    fn idempotence_fails_only_when_the_batch_carries_a_local_item() {
        // The counterexample documented in verification/verus/chat_merge.rs: the batch holds a server user item
        // and then the local item it would replace, and a local reply points at that local item.
        let r0 = reply(lucina("local-r", 101), "local-c1");
        let s = client(user("tl_10", 100), "c1");
        let u = client(user("local-c1", 100), "c1");
        let s2 = reply(lucina("tl_11", 101), "tl_10");
        let incoming = [s, u, s2];
        let first = merge(std::slice::from_ref(&r0), &incoming);
        assert_eq!(ids(&first), ["local-c1", "tl_10", "local-r", "tl_11"]);
        // The item bodies are the same as in the first pass except `local-r`, whose reply_to is now tl_10, so the
        // second pass replaces it by tl_11.
        let current: Vec<Key> = first
            .ids
            .iter()
            .map(|id| match id.as_str() {
                "local-r" => r0.clone(),
                other => incoming.iter().find(|k| k.id == other).unwrap().clone(),
            })
            .collect();
        let second = merge(&current, &incoming);
        assert_eq!(ids(&second), ["local-c1", "tl_10", "tl_11"]);
        assert_eq!(
            replaced(&second),
            [("local-c1", "tl_10"), ("local-r", "tl_11")]
        );
    }

    #[test]
    fn two_hundred_incoming_against_two_hundred_fifty_current_is_fast() {
        let current: Vec<Key> = (0..250).map(|i| user(&format!("tl_{i:04}"), i)).collect();
        let incoming: Vec<Key> = (150..350).map(|i| user(&format!("tl_{i:04}"), i)).collect();
        let t = std::time::Instant::now();
        let r = merge(&current, &incoming);
        let elapsed = t.elapsed();
        assert_eq!(r.ids.len(), 350);
        // Only a sanity bound for debug builds; `cargo run --release -p secretary-chat --example merge_speed` prints the
        // release number.
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "{elapsed:?}"
        );
    }
}
