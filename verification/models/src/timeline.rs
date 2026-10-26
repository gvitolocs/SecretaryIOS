//! Shared timeline writers: `Lucina/data/timeline.jsonl`.
//!
//! Writers (phase 1 of the Rust port, see `docs/lucina-rust/PHASE1.md`):
//! - **Node Lucina** (`Lucina/src/timeline.js`): id generated at
//!   `normalizeTimelineItem` (before the in-process lock), then refresh,
//!   rotation check against its own `LUCINA_TIMELINE_MAX_BYTES`, and
//!   `fs.appendFile` = open(O_APPEND) + one write. No cross-process lock.
//! - **Rust Lucina** (`lucina-store/src/timeline.rs`, branch `lucina-rs`):
//!   `flock(timeline.jsonl.lock)`, refresh, id moved past the newest indexed
//!   id, rotation (owner in phase 1), one O_APPEND write, refresh + publish.
//! - **Proactive engine** (`lucina-proactive/src/channels.rs`, branch
//!   `lucina-proactive`): id from the decision time, then
//!   `docker exec -i lucina sh -c 'cat >> "$0"'`: `cat` copies stdin in the
//!   chunks it reads from the pipe, so one line can become several writes.
//! - **SSE reader** (`lucina-server/src/api.rs::timeline_stream`): replay
//!   after `Last-Event-ID` by id, then live items (broadcast of newly
//!   indexed lines) and a 1 s follow tick (`poll` + `after(cursor)`).
//!
//! The file system is modelled as inodes of chunks: a whole line, the head
//! and tail of a split write, or a lone newline (Node and Rust prepend `\n`
//! when the file ends in a partial line). Paths map to inodes, so a writer
//! that opened the file before a rotation writes into `.1`.

use stateright::{Model, Property};

use crate::{set_has, set_insert, Case, Expect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum W {
    /// A line that was in the file before the model starts.
    Old,
    Node,
    Rust,
    Pro,
}

/// A ULID: (ms, random rank). The writer tag stands in for the 80 random
/// bits, so ids of different writers never collide and every order between
/// writers in the same millisecond is reachable (the rank is chosen freely).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id {
    pub ms: u8,
    pub rank: u8,
    pub w: W,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chunk {
    Line(Id),
    Head(Id),
    Tail(Id),
    Nl,
}

impl Chunk {
    fn ends_line(self) -> bool {
        !matches!(self, Chunk::Head(_))
    }
}

/// The complete lines of `chunks[from..]`: each parsed id (`None` = garbage)
/// and the chunk index after the last newline.
fn parse(chunks: &[Chunk], from: usize) -> (Vec<Option<Id>>, usize) {
    let mut out = Vec::new();
    let mut start = from;
    let mut boundary = from;
    for i in from..chunks.len() {
        if !chunks[i].ends_line() {
            continue;
        }
        let line = &chunks[start..=i];
        let id = match line {
            [Chunk::Line(a)] => Some(*a),
            [Chunk::Head(a), Chunk::Tail(b)] if a == b => Some(*a),
            [Chunk::Nl] => None, // empty line: skipped by both readers, not garbage
            _ => {
                out.push(None);
                start = i + 1;
                boundary = i + 1;
                continue;
            }
        };
        if let Some(id) = id {
            out.push(Some(id));
        }
        start = i + 1;
        boundary = i + 1;
    }
    (out, boundary)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Fs {
    pub inodes: Vec<Vec<Chunk>>,
    /// `timeline.jsonl`, `.1`, `.2`.
    pub paths: [Option<u8>; 3],
    pub rotations: u8,
    /// A rotation moved an empty current file (a wasted generation).
    pub empty_rotations: u8,
}

impl Fs {
    fn cur(&mut self) -> u8 {
        if let Some(i) = self.paths[0] {
            return i;
        }
        self.inodes.push(Vec::new());
        let i = (self.inodes.len() - 1) as u8;
        self.paths[0] = Some(i);
        i
    }

    /// `rename(.1, .2); rename(cur, .1)`; the next open creates a new file.
    fn rotate(&mut self) {
        let empty = self.paths[0].is_none_or(|i| self.inodes[i as usize].is_empty());
        if empty {
            self.empty_rotations += 1;
        }
        self.paths[2] = self.paths[1];
        self.paths[1] = self.paths[0];
        self.paths[0] = None;
        self.rotations += 1;
    }

    fn len(&self, ino: u8) -> usize {
        self.inodes[ino as usize].len()
    }

    fn ends_partial(&self, ino: u8) -> bool {
        matches!(self.inodes[ino as usize].last(), Some(Chunk::Head(_)))
    }

    /// Every complete line of the retained files.
    fn retained_lines(&self) -> Vec<Option<Id>> {
        let mut v = Vec::new();
        for ino in self.paths.iter().rev().flatten() {
            v.extend(parse(&self.inodes[*ino as usize], 0).0);
        }
        v
    }

    fn valid_ids(&self) -> Vec<Id> {
        let mut v: Vec<Id> = self.retained_lines().into_iter().flatten().collect();
        v.sort();
        v
    }
}

/// Per-process monotonic ULID state (`newTimelineId` / `ulid::new_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ulid {
    last: Option<(u8, u8)>,
}

impl Ulid {
    fn next(&mut self, now: u8, fresh_rank: u8, w: W) -> Id {
        let (ms, rank) = match self.last {
            Some((ms, r)) if now <= ms => (ms, r + 1),
            _ => (now, fresh_rank),
        };
        self.last = Some((ms, rank));
        Id { ms, rank, w }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Idle,
    /// Node / proactive: id generated, file not opened yet.
    Gen(Id),
    /// File opened (bound to an inode); `nl`: prepend a newline.
    Opened(Id, u8, bool),
    /// Proactive split write: head written.
    Half(Id, u8),
    /// Rust: flock held, refreshed, id chosen; rotation pending or not.
    Locked(Id, bool, bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Writer {
    pub left: u8,
    pub step: Step,
    pub ulid: Ulid,
    /// Node's own index view for its rotation check: (inode, complete chunks).
    pub view: Option<(u8, usize)>,
}

/// The Rust store's in-memory index (`Inner`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Index {
    pub up: bool,
    pub loaded: bool,
    pub cur: Option<u8>,
    pub known: usize,
    /// Rotation fix: the previous generation's inode and indexed length.
    pub prev: Option<(u8, usize)>,
    /// Indexed ids in arrival (= file) order, with their inode.
    pub entries: Vec<(Id, u8)>,
}

impl Index {
    fn max_id(&self) -> Option<Id> {
        self.entries.iter().map(|e| e.0).max()
    }
    fn has(&self, id: Id) -> bool {
        self.entries.iter().any(|e| e.0 == id)
    }
    fn after_by_id(&self, after: Option<Id>) -> Vec<Id> {
        let mut v: Vec<Id> = self
            .entries
            .iter()
            .map(|e| e.0)
            .filter(|id| after.is_none_or(|a| *id > a))
            .collect();
        v.sort();
        v
    }
    fn after_by_arrival(&self, after: Option<Id>) -> Vec<Id> {
        match after {
            None => self.entries.iter().map(|e| e.0).collect(),
            Some(a) => match self.entries.iter().position(|e| e.0 == a) {
                Some(p) => self.entries[p + 1..].iter().map(|e| e.0).collect(),
                None => self.after_by_id(Some(a)),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Sse {
    pub connected: bool,
    pub queue: Vec<Id>,
    pub seen: Vec<Id>,
    pub replay_end: Option<Id>,
    pub open: bool,
    pub cursor: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub now: u8,
    pub fs: Fs,
    pub node: Writer,
    pub rust: Writer,
    pub pro: Writer,
    pub idx: Index,
    pub sse: Sse,
    pub last_event: Option<Id>,
    pub delivered: Vec<Id>,
    pub disconnects_left: u8,
    pub restarts_left: u8,
    pub reconnected: bool,
    /// Writes that completed (the writer considers the line committed).
    pub written: Vec<Id>,
    pub settled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Tick,
    NodeGen(u8),
    NodeRefreshOpen,
    NodeWrite,
    ProGen(u8),
    ProOpen,
    ProWrite,
    ProWriteTail,
    RustBegin(u8),
    RustRotate,
    RustWrite,
    RustPoll,
    RustCrash,
    RustStart,
    SseDeliver,
    SseFollow,
    SseDisconnect,
    SseConnect,
    Settle,
}

/// Model switches. `current()` is the code as it is; the `fix_*` switches
/// are the proposed fixes (docs/verification.md, "Bugs found").
#[derive(Clone, Debug)]
pub struct Timeline {
    pub node_lines: u8,
    pub rust_lines: u8,
    pub pro_lines: u8,
    pub max_ms: u8,
    /// Rust rotates when the current file already holds this many lines.
    pub rust_rotate_at: u8,
    /// Node's own rotation (`LUCINA_TIMELINE_MAX_BYTES`), `None` = raised.
    pub node_rotate_at: Option<u8>,
    /// `cat >> file` may split one line into two writes.
    pub pro_split: bool,
    pub disconnects: u8,
    pub rust_restarts: u8,
    /// Fix A: SSE resume and follow by arrival order + `seen` from replay.
    pub fix_sse_arrival: bool,
    /// Fix B: after a rotation keep indexing the tail of `.1`.
    pub fix_watch_prev: bool,
    /// Fix C (the clean one): Node and the proactive engine append through
    /// the Rust store (one writer, ids ordered under the flock).
    pub fix_single_writer: bool,
    /// Fix D (web client): a reconnect that has no event id yet still
    /// catches up from the loaded page: the stream resyncs after a drop that
    /// came before its first item (implemented on branch `verify-fixes`,
    /// `web/src/api/timeline.ts`), or equivalently opens the EventSource with
    /// `?lastEventId=<newest loaded id>`.
    pub fix_query_id: bool,
}

impl Timeline {
    pub fn current() -> Self {
        Timeline {
            node_lines: 2,
            rust_lines: 1,
            pro_lines: 0,
            max_ms: 1,
            rust_rotate_at: 9,
            node_rotate_at: None,
            pro_split: false,
            disconnects: 1,
            rust_restarts: 0,
            fix_sse_arrival: false,
            fix_watch_prev: false,
            fix_single_writer: false,
            fix_query_id: false,
        }
    }

    fn quiescent(&self, s: &State) -> bool {
        s.node.left == 0
            && s.pro.left == 0
            && s.rust.left == 0
            && s.node.step == Step::Idle
            && s.pro.step == Step::Idle
            && s.rust.step == Step::Idle
    }

    // ---- the Rust store -------------------------------------------------

    fn load_all(&self, s: &mut State) {
        s.idx.entries.clear();
        s.idx.prev = None;
        for slot in (0..3).rev() {
            if let Some(ino) = s.fs.paths[slot] {
                let (lines, boundary) = parse(&s.fs.inodes[ino as usize], 0);
                for id in lines.into_iter().flatten() {
                    if !s.idx.has(id) {
                        s.idx.entries.push((id, ino));
                    }
                }
                if slot == 0 {
                    s.idx.cur = Some(ino);
                    s.idx.known = boundary;
                }
                if slot == 1 && self.fix_watch_prev {
                    s.idx.prev = Some((ino, boundary));
                }
            }
        }
        if s.fs.paths[0].is_none() {
            s.idx.cur = None;
            s.idx.known = 0;
        }
        s.idx.loaded = true;
    }

    /// `Inner::refresh`; returns the newly indexed ids (published live).
    fn refresh(&self, s: &mut State) -> Vec<Id> {
        if !s.idx.loaded {
            self.load_all(s);
            return Vec::new();
        }
        let mut added = Vec::new();
        if let Some((ino, known)) = s.idx.prev {
            let (lines, boundary) = parse(&s.fs.inodes[ino as usize], known);
            for id in lines.into_iter().flatten() {
                if !s.idx.has(id) {
                    s.idx.entries.push((id, ino));
                    added.push(id);
                }
            }
            s.idx.prev = Some((ino, boundary));
        }
        let Some(ino) = s.fs.paths[0] else {
            if s.idx.cur.is_some() || s.idx.known > 0 {
                self.load_all(s);
                return Vec::new();
            }
            return added;
        };
        if s.idx.cur.is_some_and(|c| c != ino) || s.fs.len(ino) < s.idx.known {
            // Rotated or truncated by another process: rebuild (not published).
            self.load_all(s);
            return Vec::new();
        }
        let (lines, boundary) = parse(&s.fs.inodes[ino as usize], s.idx.known);
        for id in lines.into_iter().flatten() {
            if !s.idx.has(id) {
                s.idx.entries.push((id, ino));
                added.push(id);
            }
        }
        s.idx.cur = Some(ino);
        s.idx.known = boundary;
        added
    }

    fn publish(&self, s: &mut State, ids: Vec<Id>) {
        if s.sse.connected {
            s.sse.queue.extend(ids);
        }
    }

    fn poll(&self, s: &mut State) {
        let added = self.refresh(s);
        self.publish(s, added);
    }

    // ---- the SSE stream -------------------------------------------------

    fn after(&self, s: &State, cursor: Option<Id>) -> Vec<Id> {
        if self.fix_sse_arrival {
            s.idx.after_by_arrival(cursor)
        } else {
            s.idx.after_by_id(cursor)
        }
    }

    fn send(&self, s: &mut State, id: Id) {
        let skip = set_has(&s.sse.seen, &id)
            || (!self.fix_sse_arrival && !s.sse.open && s.sse.replay_end.is_some_and(|e| id <= e));
        if skip {
            return;
        }
        set_insert(&mut s.delivered, id);
        s.last_event = Some(id);
        set_insert(&mut s.sse.seen, id);
        if self.fix_sse_arrival {
            // The cursor is the last item sent in arrival order.
            s.sse.cursor = Some(id);
        } else if s.sse.cursor.is_none_or(|c| id > c) {
            s.sse.cursor = Some(id);
        }
    }

    fn follow(&self, s: &mut State) {
        self.poll(s);
        let batch = self.after(s, s.sse.cursor);
        for id in batch {
            self.send(s, id);
        }
    }

    fn connect(&self, s: &mut State) {
        // EventSource sends Last-Event-ID once it has seen an event; the
        // initial URL may carry `?lastEventId=` (fix D).
        let resume = s
            .last_event
            .or(if self.fix_query_id { Some(OLD) } else { None });
        self.poll(s);
        s.sse = Sse {
            connected: true,
            queue: Vec::new(),
            seen: Vec::new(),
            replay_end: None,
            open: resume.is_none(),
            cursor: None,
        };
        match resume {
            Some(last) => {
                let items = self.after(s, Some(last));
                let mut cur = last;
                for id in items {
                    set_insert(&mut s.delivered, id);
                    s.last_event = Some(id);
                    if self.fix_sse_arrival {
                        set_insert(&mut s.sse.seen, id);
                    }
                    cur = id;
                }
                s.sse.cursor = Some(cur);
                s.sse.replay_end = Some(cur);
            }
            None => {
                let newest = if self.fix_sse_arrival {
                    s.idx.entries.last().map(|e| e.0)
                } else {
                    s.idx.max_id()
                };
                s.sse.cursor = newest;
                s.sse.replay_end = newest;
            }
        }
    }
}

impl Model for Timeline {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let (node_lines, rust_lines, pro_lines) = if self.fix_single_writer {
            (0, self.node_lines + self.rust_lines + self.pro_lines, 0)
        } else {
            (self.node_lines, self.rust_lines, self.pro_lines)
        };
        let w = |left| Writer {
            left,
            step: Step::Idle,
            ulid: Ulid { last: None },
            view: None,
        };
        let mut s = State {
            now: 0,
            fs: Fs {
                inodes: Vec::new(),
                paths: [None; 3],
                rotations: 0,
                empty_rotations: 0,
            },
            node: w(node_lines),
            rust: w(rust_lines),
            pro: w(pro_lines),
            idx: Index {
                up: true,
                loaded: false,
                cur: None,
                known: 0,
                prev: None,
                entries: Vec::new(),
            },
            sse: Sse {
                connected: false,
                queue: Vec::new(),
                seen: Vec::new(),
                replay_end: None,
                open: true,
                cursor: None,
            },
            last_event: None,
            delivered: Vec::new(),
            disconnects_left: self.disconnects,
            restarts_left: self.rust_restarts,
            reconnected: false,
            written: Vec::new(),
            settled: false,
        };
        // One line exists already; the app loaded it (`loadLatest`) and then
        // opened the stream.
        s.fs.inodes.push(vec![Chunk::Line(OLD)]);
        s.fs.paths[0] = Some(0);
        s.delivered.push(OLD);
        self.connect(&mut s);
        vec![s]
    }

    fn actions(&self, s: &State, a: &mut Vec<Action>) {
        if s.settled {
            return;
        }
        if s.now < self.max_ms {
            a.push(Action::Tick);
        }
        match s.node.step {
            Step::Idle if s.node.left > 0 => {
                a.push(Action::NodeGen(0));
                a.push(Action::NodeGen(1));
            }
            Step::Gen(_) => a.push(Action::NodeRefreshOpen),
            Step::Opened(..) => a.push(Action::NodeWrite),
            _ => {}
        }
        match s.pro.step {
            Step::Idle if s.pro.left > 0 => {
                a.push(Action::ProGen(0));
                a.push(Action::ProGen(1));
            }
            Step::Gen(_) => a.push(Action::ProOpen),
            Step::Opened(..) => a.push(Action::ProWrite),
            Step::Half(..) => a.push(Action::ProWriteTail),
            _ => {}
        }
        if s.idx.up {
            match s.rust.step {
                Step::Idle => {
                    if s.rust.left > 0 {
                        a.push(Action::RustBegin(0));
                        a.push(Action::RustBegin(1));
                    }
                    a.push(Action::RustPoll);
                    if s.restarts_left > 0 {
                        a.push(Action::RustCrash);
                    }
                    if s.sse.connected {
                        if !s.sse.queue.is_empty() {
                            a.push(Action::SseDeliver);
                        }
                        a.push(Action::SseFollow);
                        if s.disconnects_left > 0 {
                            a.push(Action::SseDisconnect);
                        }
                    } else {
                        a.push(Action::SseConnect);
                    }
                    if self.quiescent(s) && s.sse.connected && s.sse.queue.is_empty() {
                        a.push(Action::Settle);
                    }
                }
                Step::Locked(_, true, _) => a.push(Action::RustRotate),
                Step::Locked(_, false, _) => a.push(Action::RustWrite),
                _ => {}
            }
        } else {
            a.push(Action::RustStart);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        match action {
            Action::Tick => s.now += 1,
            Action::NodeGen(r) => {
                let id = s.node.ulid.next(s.now, r, W::Node);
                s.node.step = Step::Gen(id);
            }
            Action::NodeRefreshOpen => {
                let Step::Gen(id) = s.node.step else {
                    return None;
                };
                // refresh(): Node's own view of the current file.
                let cur = s.fs.paths[0];
                let view = match (cur, s.node.view) {
                    (Some(ino), _) => {
                        let (_, boundary) = parse(&s.fs.inodes[ino as usize], 0);
                        Some((ino, boundary))
                    }
                    (None, _) => None,
                };
                s.node.view = view;
                let lines = view.map_or(0, |(ino, b)| {
                    parse(&s.fs.inodes[ino as usize][..b], 0).0.len()
                });
                if let Some(k) = self.node_rotate_at {
                    if lines > 0 && lines as u8 >= k {
                        s.fs.rotate();
                        s.node.view = None;
                    }
                }
                let ino = s.fs.cur();
                let nl = s.fs.ends_partial(ino);
                s.node.step = Step::Opened(id, ino, nl);
            }
            Action::NodeWrite => {
                let Step::Opened(id, ino, nl) = s.node.step else {
                    return None;
                };
                if nl {
                    s.fs.inodes[ino as usize].push(Chunk::Nl);
                }
                s.fs.inodes[ino as usize].push(Chunk::Line(id));
                s.written.push(id);
                s.node.left -= 1;
                s.node.step = Step::Idle;
            }
            Action::ProGen(r) => {
                let id = s.pro.ulid.next(s.now, r, W::Pro);
                s.pro.step = Step::Gen(id);
            }
            Action::ProOpen => {
                let Step::Gen(id) = s.pro.step else {
                    return None;
                };
                let ino = s.fs.cur();
                s.pro.step = Step::Opened(id, ino, false);
            }
            Action::ProWrite => {
                let Step::Opened(id, ino, _) = s.pro.step else {
                    return None;
                };
                if self.pro_split {
                    s.fs.inodes[ino as usize].push(Chunk::Head(id));
                    s.pro.step = Step::Half(id, ino);
                } else {
                    s.fs.inodes[ino as usize].push(Chunk::Line(id));
                    s.written.push(id);
                    s.pro.left -= 1;
                    s.pro.step = Step::Idle;
                }
            }
            Action::ProWriteTail => {
                let Step::Half(id, ino) = s.pro.step else {
                    return None;
                };
                s.fs.inodes[ino as usize].push(Chunk::Tail(id));
                s.written.push(id);
                s.pro.left -= 1;
                s.pro.step = Step::Idle;
            }
            Action::RustBegin(r) => {
                // flock + refresh (published) + id past the newest + rotation check.
                self.poll(&mut s);
                let mut id = s.rust.ulid.next(s.now, r, W::Rust);
                if let Some(max) = s.idx.max_id() {
                    if id <= max {
                        id = Id {
                            ms: max.ms,
                            rank: max.rank + 1,
                            w: W::Rust,
                        };
                    }
                }
                let lines = s.idx.cur.map_or(0, |ino| {
                    parse(&s.fs.inodes[ino as usize][..s.idx.known], 0).0.len()
                });
                let rotate = s.idx.known > 0 && lines as u8 >= self.rust_rotate_at;
                let nl = s.idx.cur.is_some_and(|ino| s.fs.len(ino) > s.idx.known);
                s.rust.step = Step::Locked(id, rotate, nl);
            }
            Action::RustRotate => {
                let Step::Locked(id, true, _) = s.rust.step else {
                    return None;
                };
                let old = s.fs.paths[0];
                s.fs.rotate();
                let retained: Vec<u8> = s.fs.paths.iter().flatten().copied().collect();
                s.idx.entries.retain(|e| retained.contains(&e.1));
                s.idx.prev = if self.fix_watch_prev {
                    old.map(|ino| (ino, s.idx.known))
                } else {
                    None
                };
                s.idx.cur = None;
                s.idx.known = 0;
                s.rust.step = Step::Locked(id, false, false);
            }
            Action::RustWrite => {
                let Step::Locked(id, false, nl) = s.rust.step else {
                    return None;
                };
                let ino = s.fs.cur();
                if nl {
                    s.fs.inodes[ino as usize].push(Chunk::Nl);
                }
                s.fs.inodes[ino as usize].push(Chunk::Line(id));
                s.written.push(id);
                s.rust.left -= 1;
                s.rust.step = Step::Idle;
                self.poll(&mut s);
            }
            Action::RustPoll => self.poll(&mut s),
            Action::RustCrash => {
                s.idx.up = false;
                s.idx.loaded = false;
                s.idx.entries.clear();
                s.idx.prev = None;
                s.idx.cur = None;
                s.idx.known = 0;
                s.sse.connected = false;
                s.sse.queue.clear();
                s.restarts_left -= 1;
            }
            Action::RustStart => s.idx.up = true,
            Action::SseDeliver => {
                let id = s.sse.queue.remove(0);
                self.send(&mut s, id);
            }
            Action::SseFollow => self.follow(&mut s),
            Action::SseDisconnect => {
                s.sse.connected = false;
                s.sse.queue.clear();
                s.disconnects_left -= 1;
            }
            Action::SseConnect => {
                self.connect(&mut s);
                s.reconnected = true;
            }
            Action::Settle => {
                self.follow(&mut s);
                while !s.sse.queue.is_empty() {
                    let id = s.sse.queue.remove(0);
                    self.send(&mut s, id);
                }
                s.settled = true;
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("lines_whole", |_, s: &State| {
                s.fs.retained_lines().iter().all(Option::is_some)
            }),
            Property::always("ids_unique", |_, s: &State| {
                let v = s.fs.valid_ids();
                v.windows(2).all(|w| w[0] != w[1])
            }),
            Property::always("rotation_keeps_lines", |_, s: &State| {
                let ids = s.fs.valid_ids();
                s.fs.empty_rotations == 0 && s.written.iter().all(|w| set_has(&ids, w))
            }),
            Property::always("index_complete", |_, s: &State| {
                !s.settled || s.fs.valid_ids().iter().all(|id| s.idx.has(*id))
            }),
            Property::always("reader_never_skips", |_, s: &State| {
                !s.settled || s.fs.valid_ids().iter().all(|id| set_has(&s.delivered, id))
            }),
            Property::sometimes("rotated", |_, s: &State| s.fs.rotations > 0 && s.settled),
            Property::sometimes("resumed", |_, s: &State| s.reconnected && s.settled),
        ]
    }
}

pub const PROTOCOL: &str = "timeline";

/// The line already in the file when the model starts.
pub const OLD: Id = Id {
    ms: 0,
    rank: 0,
    w: W::Old,
};

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let depth = if deep { None } else { Some(40) };
    let mut out = Vec::new();
    // Isolate one cause per witness: the other fixes are on.
    let base = Timeline {
        fix_query_id: true,
        ..Timeline::current()
    };

    // T1: the phase-1 known weakness. Node and Rust in the same ms, the app
    // reconnects with Last-Event-ID: a Node line with a smaller id than one
    // the app already has is never replayed.
    let late_id = base.clone();
    out.push(Case::new(
        PROTOCOL,
        "T1 late_id_resume (current code)",
        late_id.clone(),
        depth,
        vec![
            ("lines_whole", Holds),
            ("ids_unique", Holds),
            ("rotation_keeps_lines", Holds),
            ("index_complete", Holds),
            ("reader_never_skips", Violated),
            ("resumed", Reached),
        ],
    ));
    out.push(Case::new(
        PROTOCOL,
        "T1 late_id_resume + fix A (arrival-order cursor)",
        Timeline {
            fix_sse_arrival: true,
            ..late_id.clone()
        },
        depth,
        vec![
            ("reader_never_skips", Holds),
            ("index_complete", Holds),
            ("resumed", Reached),
        ],
    ));

    // T2: rotation by Rust while Node has the file open: the Node line lands
    // in `.1` behind the Rust index and is never indexed or sent.
    let rot = Timeline {
        node_lines: 1,
        rust_lines: 2,
        rust_rotate_at: 2,
        disconnects: 0,
        ..base.clone()
    };
    out.push(Case::new(
        PROTOCOL,
        "T2 rotation_race (current code)",
        rot.clone(),
        depth,
        vec![
            ("lines_whole", Holds),
            ("rotation_keeps_lines", Holds),
            ("index_complete", Violated),
            ("reader_never_skips", Violated),
            ("rotated", Reached),
        ],
    ));
    out.push(Case::new(
        PROTOCOL,
        "T2 rotation_race + fix B (watch .1 tail)",
        Timeline {
            fix_watch_prev: true,
            ..rot.clone()
        },
        depth,
        vec![
            ("index_complete", Holds),
            ("reader_never_skips", Holds),
            ("rotated", Reached),
        ],
    ));

    // T3: the proactive engine's `docker exec … cat >>` split write.
    out.push(Case::new(
        PROTOCOL,
        "T3 docker_cat_split (proactive sink)",
        Timeline {
            node_lines: 1,
            rust_lines: 1,
            pro_lines: 1,
            pro_split: true,
            disconnects: 0,
            ..base.clone()
        },
        depth,
        vec![
            ("lines_whole", Violated),
            ("rotation_keeps_lines", Violated),
        ],
    ));

    // T4: Node rotation not disabled: Node reaches its own limit whenever
    // Rust has not appended for a while; two rotators race.
    out.push(Case::new(
        PROTOCOL,
        "T4 two_rotators (Node limit not raised)",
        Timeline {
            node_lines: 2,
            rust_lines: 1,
            rust_rotate_at: 2,
            node_rotate_at: Some(2),
            disconnects: 0,
            ..base.clone()
        },
        depth,
        vec![("rotation_keeps_lines", Violated)],
    ));

    // T5: the web client opens the stream without an id; a browser
    // auto-reconnect before the first event starts fresh and skips the gap.
    let gap = Timeline {
        fix_single_writer: true,
        fix_query_id: false,
        node_lines: 1,
        rust_lines: 1,
        ..Timeline::current()
    };
    out.push(Case::new(
        PROTOCOL,
        "T5 fresh_reconnect_gap (web EventSource)",
        gap.clone(),
        depth,
        vec![("reader_never_skips", Violated), ("resumed", Reached)],
    ));
    out.push(Case::new(
        PROTOCOL,
        "T5 fresh_reconnect_gap + fix D (resync)",
        Timeline {
            fix_query_id: true,
            ..gap
        },
        depth,
        vec![("reader_never_skips", Holds), ("resumed", Reached)],
    ));

    // T6/T7: every writer, rotation, a reconnect; the fixed protocol.
    let all = Timeline {
        node_lines: if deep { 2 } else { 1 },
        rust_lines: 2,
        pro_lines: 1,
        rust_rotate_at: 2,
        disconnects: 1,
        fix_sse_arrival: true,
        fix_watch_prev: true,
        fix_query_id: true,
        ..Timeline::current()
    };
    let all_expect = vec![
        ("lines_whole", Holds),
        ("ids_unique", Holds),
        ("rotation_keeps_lines", Holds),
        ("index_complete", Holds),
        ("reader_never_skips", Holds),
        ("rotated", Reached),
        ("resumed", Reached),
    ];
    out.push(Case::new(
        PROTOCOL,
        "T6 all_writers + fixes A, B, D",
        all.clone(),
        depth,
        all_expect.clone(),
    ));
    out.push(Case::new(
        PROTOCOL,
        "T7 all_writers + fixes C, D (single writer)",
        Timeline {
            fix_sse_arrival: false,
            fix_watch_prev: false,
            fix_single_writer: true,
            node_lines: 2,
            ..all
        },
        depth,
        all_expect,
    ));
    out
}
