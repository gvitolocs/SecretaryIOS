//! Stateright model of the resumable memo upload (docs/memos.md, "Upload protocol"),
//! checked exhaustively for a small file.
//!
//! Actors: one client (the iPhone's or the web's upload queue) and the server
//! (`store::Store`). The network may delay, reorder, duplicate or drop any message,
//! and a chunk may arrive cut short (the connection broke mid-body: the server keeps
//! the bytes it got). The client may crash at any time and restart from its saved queue
//! entry (it remembers the server id once it saw one, nothing else).
//!
//! Properties:
//! - `prefix`: the server's bytes are always a prefix of the client's file (no gap, no
//!   duplicate byte, nothing past the end).
//! - `one memo per recording`: retried creates never make a second memo.
//! - `complete only when whole`: a memo is queued for the pipeline only with every byte.
//! - `done means queued`: when the client drops the entry, the server has the memo queued.
//! - reachability: some run ends with the memo queued and the client done.
use stateright::{Checker, Model, Property};

/// File length in bytes; chunks are at most `CHUNK` bytes.
const N: u8 = 3;
const CHUNK: u8 = 2;

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum Msg {
    // client -> server
    Create,
    Query,
    /// Bytes [offset, offset+len) of the file.
    Put { offset: u8, len: u8 },
    Complete,
    // server -> client
    Created { received: u8, done: bool },
    Offset { received: u8, done: bool },
    Completed,
    Short { received: u8 },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum Client {
    /// Needs to (re)create or query the memo.
    Start,
    /// Waiting for an answer.
    Waiting,
    /// Knows the server's offset.
    At(u8),
    Done,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    client: Client,
    /// The client saved the server id (its queue entry has `serverId`).
    has_id: bool,
    crashes: u8,
    /// Memos the server created for this recording's client id.
    memos: u8,
    received: u8,
    /// The server once appended somewhere other than its end (a gap or overlap).
    corrupt: bool,
    queued: bool,
    /// Messages in flight (a multiset, kept sorted).
    net: Vec<(bool, Msg)>,
    sends: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Action {
    ClientSend,
    Deliver(usize),
    /// A PUT that arrives cut to `keep` bytes.
    DeliverShort(usize, u8),
    Drop(usize),
    Duplicate(usize),
    Crash,
}

struct Upload;

const MAX_SENDS: u8 = 6;
const MAX_CRASHES: u8 = 1;
const MAX_NET: usize = 3;

fn push(net: &mut Vec<(bool, Msg)>, to_server: bool, m: Msg) {
    net.push((to_server, m));
    net.sort();
}

impl Model for Upload {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            client: Client::Start,
            has_id: false,
            crashes: 0,
            memos: 0,
            received: 0,
            corrupt: false,
            queued: false,
            net: vec![],
            sends: 0,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.sends < MAX_SENDS && s.net.len() < MAX_NET && matches!(s.client, Client::Start | Client::At(_) | Client::Waiting) {
            out.push(Action::ClientSend);
        }
        for (i, (to_server, m)) in s.net.iter().enumerate() {
            out.push(Action::Deliver(i));
            out.push(Action::Drop(i));
            if s.net.len() < MAX_NET {
                out.push(Action::Duplicate(i));
            }
            if let (true, Msg::Put { len, .. }) = (to_server, m) {
                for keep in 0..*len {
                    out.push(Action::DeliverShort(i, keep));
                }
            }
        }
        if s.crashes < MAX_CRASHES && s.client != Client::Done {
            out.push(Action::Crash);
        }
    }

    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        let mut n = s.clone();
        match a {
            Action::ClientSend => {
                n.sends += 1;
                let m = match s.client {
                    // A retry while waiting (timeout) re-asks where the server is.
                    Client::Start | Client::Waiting => if s.has_id { Msg::Query } else { Msg::Create },
                    Client::At(off) if off < N => Msg::Put { offset: off, len: CHUNK.min(N - off) },
                    Client::At(_) => Msg::Complete,
                    Client::Done => return None,
                };
                push(&mut n.net, true, m);
                n.client = Client::Waiting;
            }
            Action::Drop(i) => {
                n.net.remove(i);
            }
            Action::Duplicate(i) => {
                let m = n.net[i].clone();
                push(&mut n.net, m.0, m.1);
            }
            Action::Crash => {
                n.crashes += 1;
                n.client = Client::Start;
            }
            Action::Deliver(i) | Action::DeliverShort(i, _) => {
                let keep = if let Action::DeliverShort(_, k) = a { Some(k) } else { None };
                let (to_server, m) = n.net.remove(i);
                if to_server {
                    // The server (`Store::create/append/complete`).
                    let reply = match m {
                        Msg::Create => {
                            if n.memos == 0 {
                                n.memos = 1;
                            }
                            Msg::Created { received: n.received, done: n.queued }
                        }
                        Msg::Query => Msg::Offset { received: n.received, done: n.queued },
                        Msg::Put { offset, len } => {
                            if n.queued || offset != n.received {
                                Msg::Offset { received: n.received, done: n.queued }
                            } else {
                                let got = keep.unwrap_or(len);
                                n.corrupt |= offset != n.received;
                                n.received = (offset + got).min(N);
                                Msg::Offset { received: n.received, done: false }
                            }
                        }
                        Msg::Complete => {
                            if n.queued {
                                Msg::Completed
                            } else if n.received == N {
                                // (and the SHA-256 matches: the bytes are the file's, by `prefix`)
                                n.queued = true;
                                Msg::Completed
                            } else {
                                Msg::Short { received: n.received }
                            }
                        }
                        _ => return None,
                    };
                    if n.net.len() < MAX_NET + 1 {
                        push(&mut n.net, false, reply);
                    }
                } else {
                    // The client (`MemoUploadQueue.upload` / `flushQueue`).
                    if n.client == Client::Done {
                        return Some(n);
                    }
                    match m {
                        Msg::Created { received, done } | Msg::Offset { received, done } => {
                            n.has_id = true;
                            n.client = if done { Client::Done } else { Client::At(received) };
                        }
                        Msg::Short { received } => n.client = Client::At(received),
                        Msg::Completed => n.client = Client::Done,
                        _ => return None,
                    }
                }
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("prefix", |_, s: &State| !s.corrupt && s.received <= N),
            Property::always("one memo per recording", |_, s: &State| s.memos <= 1),
            Property::always("complete only when whole", |_, s: &State| !s.queued || s.received == N),
            Property::always("done means queued", |_, s: &State| s.client != Client::Done || s.queued),
            Property::sometimes("uploaded", |_, s: &State| s.queued && s.client == Client::Done),
        ]
    }
}

#[test]
fn resumable_upload_is_safe() {
    let checker = Upload.checker().spawn_bfs().join();
    checker.assert_properties();
    assert!(checker.unique_state_count() > 1000, "model too small: {}", checker.unique_state_count());
}
