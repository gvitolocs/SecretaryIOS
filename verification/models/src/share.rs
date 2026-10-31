//! Share to Lucina (`POST /v1/share`).
//!
//! Code (branch `lucina-rs` unless noted): `lucina-server/src/share/mod.rs`
//! (`authenticate`, `receive`, `ensure_user`, `existing_reply`, `recover`,
//! `spawn`, `process`); the p1 Caddy `forward_auth` + `X-Share-Gateway`;
//! iOS (branch `share-ios`) `ios/Extensions/ShareShared/*`,
//! `Features/Share/ShareUploads.swift`, `Features/Chat/ChatShares.swift`
//! (`ShareReconcile.accept`) and `Chat/Timeline.swift` (`merge`); web
//! `web/src/state/chat.ts` (`deliverShare`, `matchItem`, `adopt`).
//!
//! Processes:
//!
//! - clients: the chat composer (share 0, an album of `album_files` files,
//!   background `URLSession`, a Retry with the same `shareId`) and one share
//!   extension (share 1), either with an App Group (background upload: it
//!   survives the extension being killed, the app retries the same body on
//!   launch) or without one (today: a foreground upload inside the extension;
//!   a kill loses the body, the owner shares again with a NEW `shareId`, share
//!   2). An attacker sends one request without the owner's auth, through p1
//!   (`forward_auth` answers 401) or straight at lucina-rs (no
//!   `X-Share-Gateway`: 401);
//! - `receive` under the `ingest` mutex, split where a crash leaves a
//!   different disk state: (1) lookup `status.json` (found → 202 `duplicate`,
//!   nothing new), (2) move the staged files and ingest them into
//!   lucina-media, (3) write `status.json` (processing, the prepared user item
//!   with a fixed id), (4) `ensure_user`, spawn, 202. Steps 1-2 contain
//!   `.await`s: a client disconnect (Caddy cancels the upstream request)
//!   drops the handler there. After step 3 the rest is synchronous;
//! - one worker (`Semaphore::new(1)`, FIFO) runs `process`: `ensure_user`,
//!   `existing_reply` (the current code scans the newest 500 items, the model
//!   the newest `window`; `None` = every item), reading each part, ONE turn
//!   writing ONE Lucina item (`replyTo` = the user item), then `status.json`
//!   done. An error before the turn writes one Lucina error item;
//! - voice notes (branch `voice-notes`): with `voice` the composer's share is
//!   one audio part flagged `meta.files[0].voice` with an empty caption. The
//!   device may have put a `transcript` in the meta (one initial state with,
//!   one without); without it `process` runs whisper, which may fail. The
//!   share is answered as if the transcript were typed (one Lucina item), and
//!   a failed transcription gets one "couldn't hear it" Lucina item instead,
//!   which `existing_reply` must find after a crash like any reply (it carries
//!   `replyTo` = the user item; `unheard_reply_to: false` is the ablation).
//!   With `LUCINA_PLANNER=tools` (`tools_planner`) a voice-only share runs
//!   `chat::tools_turn`, whose `on_tool` writes a Lucina progress item
//!   (`replyTo` = the user item) per tool call; for a share turn it writes
//!   nothing (`voice_tool_items: false`; `true` is the ablation), so there is
//!   still one Lucina item. A progress item would also be what
//!   `existing_reply` finds after a crash;
//! - restart: `recover` (under the mutex) respawns every `status.json` with
//!   status processing. It is spawned at router build, so a request can take
//!   the mutex before it (the same share is then queued twice);
//! - other timeline writers (Node, other turns) append items;
//! - the composer's chat view: an optimistic local bubble, the server's user
//!   item from the timeline stream (possibly before the 202), the 202's
//!   `userItemId`.

use stateright::{Model, Property};

use crate::{set_has, set_insert, Case, Expect};

/// Share ids: 0 composer, 1 extension, 2 the extension's re-share, 3 attacker.
pub const SHARES: usize = 4;
pub const COMPOSER_SHARE: u8 = 0;
pub const EXT_SHARE: u8 = 1;
pub const RESHARE: u8 = 2;
pub const ATTACKER_SHARE: u8 = 3;
/// The attacker's "client" index (no client record).
pub const ATTACKER: u8 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// iOS chat composer (`ChatStore.sendShare`, source `ios-chat`).
    Composer,
    /// Share extension with an App Group: background `URLSession`.
    ExtBackground,
    /// Share extension without an App Group: foreground upload in the extension.
    ExtForeground,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    Ready,
    InFlight,
    /// A 202 arrived (new or `duplicate`).
    Accepted,
    /// No 202 (lost, 5xx, connection reset): retry with the same body.
    Failed,
    /// The foreground extension was killed: the body is gone.
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Client {
    pub kind: Kind,
    pub share: u8,
    pub phase: Phase,
    pub killed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Req {
    pub client: u8,
    pub share: u8,
    /// Owner device token / cookie (+CSRF).
    pub owner: bool,
    /// Sent straight at lucina-rs, not through p1's Caddy.
    pub direct: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Item {
    User(u8),
    /// Lucina's answer or error item (`replyTo` = the share's user item).
    Lucina(u8),
    /// A voice note's "couldn't hear it" item.
    Unheard(u8),
    /// A tools-planner progress item (`on_tool`: role lucina, `replyTo` =
    /// the user item).
    Tool(u8),
    Other,
}

/// Where a voice note's transcript came from in a `process` run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Heard {
    /// `meta.files[0].transcript` from the device.
    Device,
    /// whisper on the server.
    Server,
    /// whisper failed (or timed out): no transcript.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Processing,
    Done,
    Error,
}

/// `receive` past the lookup, holding the `ingest` mutex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rx {
    /// Lookup found no `status.json`.
    New,
    /// Staged files moved into the share dir and ingested into lucina-media.
    Ingested,
    /// `status.json` written.
    Saved,
}

/// The worker's current `process` run (in memory: lost on a crash).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Proc {
    pub share: u8,
    pub read: u8,
    /// The Lucina item (answer or error) is written; `status.json` next.
    pub written: Option<Status>,
    /// A voice note's transcript, once its part is read.
    pub heard: Option<Heard>,
    /// The tools planner ran a tool in this turn.
    pub tooled: bool,
}

/// The composer share's bubbles in the chat view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct View {
    /// The optimistic `local-<shareId>` bubble.
    pub local: bool,
    /// A bubble carrying the server's user item id.
    pub server: bool,
    /// The timeline stream delivered the server's user item.
    pub delivered: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub clients: Vec<Client>,
    pub net: Vec<Req>,
    pub rx: Option<(Req, Rx)>,
    pub status: [Option<Status>; SHARES],
    /// Shares whose files are in lucina-media (ingest dedups by content).
    pub media: Vec<u8>,
    pub timeline: Vec<Item>,
    /// Spawned `process` tasks waiting for the worker permit.
    pub work: Vec<u8>,
    pub proc: Option<Proc>,
    pub up: bool,
    pub recovered: bool,
    pub view: View,
    /// The voice note's body carries an on-device transcript.
    pub on_device: bool,
    /// Ghost: how the voice note was heard by the run that answered it.
    pub voice_heard: Option<Heard>,
    /// Ghost: the run that answered the voice note ran a tool first.
    pub voice_tooled: bool,
    /// Requests sent per share id (ghost).
    pub sent: [u8; SHARES],
    pub crashes: u8,
    pub net_faults: u8,
    pub append_faults: u8,
    pub proc_faults: u8,
    pub others: u8,
    pub reshares: u8,
    pub attacks: u8,
    /// Ghosts: a retry was answered `duplicate`; shares respawned by
    /// `recover`; an unauthenticated request was refused.
    pub dup_hit: bool,
    pub revived: Vec<u8>,
    pub attack_rejected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// A client sends the body (first time or a retry with the same shareId).
    Send(u8),
    /// The owner shares again from the extension: a new shareId.
    Reshare(u8),
    /// iOS kills the share extension.
    Kill(u8),
    Attack {
        direct: bool,
    },
    /// The network loses a request (the client sees an error).
    DropRequest(Req),
    /// The request reaches the gateway / lucina-rs; `receive` takes the
    /// mutex and looks up `status.json`.
    Deliver(Req),
    RxIngest,
    RxSave,
    /// `ensure_user`, spawn, 202.
    RxFinish,
    /// The same, but the 202 is lost on the way back.
    RxFinishLost,
    /// `ensure_user` fails (timeline I/O): 500.
    RxAppendFails,
    /// `process`: `ensure_user` + `existing_reply`.
    ProcBegin,
    ProcRead,
    /// A voice note's part is read: its transcript from the device, from
    /// whisper, or none.
    ProcHear(Heard),
    /// The tools planner runs a tool for a heard voice note (`on_tool`).
    ProcTool,
    /// The Lucina turn writes its item (a voice note without a transcript:
    /// the "couldn't hear it" item).
    ProcTurn,
    /// An error before the turn: one Lucina error item.
    ProcFail,
    /// `status.json` done / error.
    ProcSave,
    OtherAppend,
    Crash,
    Restart,
    Recover,
    /// The chat's timeline stream delivers the composer share's user item.
    TimelineDeliver,
}

#[derive(Clone, Debug)]
pub struct Share {
    /// Extension variants explored (one initial state each).
    pub ext_kinds: Vec<Kind>,
    pub album_files: u8,
    /// Requests per share id (1 + retries).
    pub max_sends: u8,
    pub crashes: u8,
    pub net_faults: u8,
    pub append_faults: u8,
    pub proc_faults: u8,
    pub others: u8,
    pub reshares: u8,
    pub attacks: u8,
    /// `existing_reply` scans the newest `window` items (`Some(500)` in the
    /// code today); `None` scans every item (the fix under way).
    pub window: Option<u8>,
    /// The chat view also matches the server item by `meta.shareId`
    /// (iOS `Timeline.merge`, `ShareReconcile.accept`); `false` = only by
    /// the 202's `userItemId` (old iOS, web `matchItem` today).
    pub reconcile_by_share_id: bool,
    /// lucina-rs checks `X-Share-Gateway`. The model lets the attacker reach
    /// lucina-rs directly (a superset of the deployment, where it listens on
    /// loopback / p1 only), so this check alone must stop it.
    pub key_check: bool,
    /// Fix: `receive` spawns `process` even when `ensure_user` failed
    /// (process starts with `ensure_user`); today it returns 500 first.
    pub fix_spawn_on_append_error: bool,
    /// The composer's share is one voice note instead of the album.
    pub voice: bool,
    /// The "couldn't hear it" item carries `replyTo` (so `existing_reply`
    /// finds it); `false` is the ablation.
    pub unheard_reply_to: bool,
    /// `LUCINA_PLANNER=tools`: a voice-only share runs the tool loop.
    pub tools_planner: bool,
    /// `on_tool` writes progress items for a share turn too (the ablation;
    /// the code returns early when the turn carries `reply_meta`).
    pub voice_tool_items: bool,
}

fn count(t: &[Item], x: Item) -> usize {
    t.iter().filter(|i| **i == x).count()
}

/// Lucina items for a share: answers, errors, "couldn't hear it" and tool
/// progress items.
fn answers(t: &[Item], x: u8) -> usize {
    count(t, Item::Lucina(x)) + count(t, Item::Unheard(x)) + count(t, Item::Tool(x))
}

impl Share {
    fn files(&self, share: u8) -> u8 {
        if share == COMPOSER_SHARE && !self.voice {
            self.album_files
        } else {
            1
        }
    }

    fn is_voice(&self, share: u8) -> bool {
        self.voice && share == COMPOSER_SHARE
    }

    fn ensure_user(s: &mut State, share: u8) {
        if !s.timeline.contains(&Item::User(share)) {
            s.timeline.push(Item::User(share));
        }
    }

    fn existing_reply(&self, s: &State, share: u8) -> bool {
        let n = self.window.map_or(s.timeline.len(), usize::from);
        s.timeline.iter().rev().take(n).any(|i| {
            *i == Item::Lucina(share)
                || *i == Item::Tool(share)
                || (self.unheard_reply_to && *i == Item::Unheard(share))
        })
    }

    /// A response reaches the client that is still waiting for it.
    fn respond(&self, s: &mut State, client: u8, ok: bool, dup: bool) {
        let Some(c) = s.clients.get_mut(client as usize) else {
            return; // the attacker
        };
        if c.phase != Phase::InFlight {
            return; // a killed foreground extension: nobody listens
        }
        c.phase = if ok { Phase::Accepted } else { Phase::Failed };
        if ok && dup {
            s.dup_hit = true;
        }
        if ok && c.kind == Kind::Composer {
            // ShareReconcile.accept: the timeline already has the server's
            // copy (by userItemId, or by shareId) → drop the local bubble;
            // else the local bubble becomes the server item (id = userItemId).
            s.view.local = false;
            s.view.server = true;
        }
    }

    fn quiet(s: &State) -> bool {
        s.up && s.recovered
            && s.rx.is_none()
            && s.work.is_empty()
            && s.proc.is_none()
            && s.net.is_empty()
    }

    fn owner_shares() -> [u8; 3] {
        [COMPOSER_SHARE, EXT_SHARE, RESHARE]
    }
}

impl Model for Share {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let composer = Client {
            kind: Kind::Composer,
            share: COMPOSER_SHARE,
            phase: Phase::Ready,
            killed: false,
        };
        let mut variants: Vec<Vec<Client>> = self
            .ext_kinds
            .iter()
            .map(|&kind| {
                let ext = Client {
                    kind,
                    share: EXT_SHARE,
                    phase: Phase::Ready,
                    killed: false,
                };
                vec![composer, ext]
            })
            .collect();
        if variants.is_empty() {
            variants.push(vec![composer]);
        }
        let devices: &[bool] = if self.voice { &[false, true] } else { &[false] };
        variants
            .into_iter()
            .flat_map(|clients| devices.iter().map(move |&d| (clients.clone(), d)))
            .map(|(clients, on_device)| State {
                clients,
                net: Vec::new(),
                rx: None,
                status: [None; SHARES],
                media: Vec::new(),
                timeline: Vec::new(),
                work: Vec::new(),
                proc: None,
                up: true,
                recovered: true,
                view: View {
                    local: false,
                    server: false,
                    delivered: false,
                },
                on_device,
                voice_heard: None,
                voice_tooled: false,
                sent: [0; SHARES],
                crashes: self.crashes,
                net_faults: self.net_faults,
                append_faults: self.append_faults,
                proc_faults: self.proc_faults,
                others: self.others,
                reshares: self.reshares,
                attacks: self.attacks,
                dup_hit: false,
                revived: Vec::new(),
                attack_rejected: false,
            })
            .collect()
    }

    fn actions(&self, s: &State, a: &mut Vec<Action>) {
        for (i, c) in s.clients.iter().enumerate() {
            let i = i as u8;
            let retry = c.phase == Phase::Failed
                && c.kind != Kind::ExtForeground
                && s.sent[c.share as usize] < self.max_sends;
            if c.phase == Phase::Ready || retry {
                a.push(Action::Send(i));
            }
            if c.kind == Kind::ExtForeground
                && matches!(c.phase, Phase::Failed | Phase::Lost)
                && s.reshares > 0
                && c.share != RESHARE
            {
                a.push(Action::Reshare(i));
            }
            if c.kind != Kind::Composer && c.phase == Phase::InFlight && !c.killed {
                a.push(Action::Kill(i));
            }
        }
        if s.attacks > 0 {
            a.push(Action::Attack { direct: false });
            a.push(Action::Attack { direct: true });
        }
        for r in &s.net {
            if s.rx.is_none() {
                a.push(Action::Deliver(*r));
            }
            if s.net_faults > 0 && r.owner {
                a.push(Action::DropRequest(*r));
            }
        }
        match s.rx {
            Some((_, Rx::New)) => a.push(Action::RxIngest),
            Some((_, Rx::Ingested)) => a.push(Action::RxSave),
            Some((_, Rx::Saved)) => {
                a.push(Action::RxFinish);
                if s.net_faults > 0 {
                    a.push(Action::RxFinishLost);
                }
                if s.append_faults > 0 {
                    a.push(Action::RxAppendFails);
                }
            }
            None => {}
        }
        if s.up {
            match s.proc {
                None if !s.work.is_empty() => a.push(Action::ProcBegin),
                None => {}
                Some(p) => match p.written {
                    None => {
                        if p.read < self.files(p.share) {
                            if !self.is_voice(p.share) {
                                a.push(Action::ProcRead);
                            } else if s.on_device {
                                a.push(Action::ProcHear(Heard::Device));
                            } else {
                                a.push(Action::ProcHear(Heard::Server));
                                a.push(Action::ProcHear(Heard::Failed));
                            }
                        } else {
                            a.push(Action::ProcTurn);
                            if self.tools_planner
                                && self.is_voice(p.share)
                                && !p.tooled
                                && matches!(p.heard, Some(Heard::Device | Heard::Server))
                            {
                                a.push(Action::ProcTool);
                            }
                        }
                        if s.proc_faults > 0 {
                            a.push(Action::ProcFail);
                        }
                    }
                    Some(_) => a.push(Action::ProcSave),
                },
            }
            if !s.recovered && s.rx.is_none() {
                a.push(Action::Recover);
            }
            if s.crashes > 0 {
                a.push(Action::Crash);
            }
        } else {
            a.push(Action::Restart);
        }
        if s.others > 0 {
            a.push(Action::OtherAppend);
        }
        if !s.view.delivered && s.timeline.contains(&Item::User(COMPOSER_SHARE)) {
            a.push(Action::TimelineDeliver);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        match action {
            Action::Send(i) => {
                let c = &mut s.clients[i as usize];
                if c.kind == Kind::Composer && c.phase == Phase::Ready {
                    s.view.local = true; // the optimistic bubble
                }
                c.phase = Phase::InFlight;
                let share = c.share;
                s.sent[share as usize] += 1;
                set_insert(
                    &mut s.net,
                    Req {
                        client: i,
                        share,
                        owner: true,
                        direct: false,
                    },
                );
            }
            Action::Reshare(i) => {
                s.reshares -= 1;
                let c = &mut s.clients[i as usize];
                c.share = RESHARE;
                c.phase = Phase::InFlight;
                s.sent[RESHARE as usize] += 1;
                set_insert(
                    &mut s.net,
                    Req {
                        client: i,
                        share: RESHARE,
                        owner: true,
                        direct: false,
                    },
                );
            }
            Action::Kill(i) => {
                let c = &mut s.clients[i as usize];
                c.killed = true;
                if c.kind == Kind::ExtForeground {
                    // The body lives in the extension's private caches.
                    c.phase = Phase::Lost;
                    s.net.retain(|r| r.client != i);
                    // Caddy cancels the upstream request: the handler is
                    // dropped at its next `.await` (only before status.json).
                    if let Some((r, Rx::New | Rx::Ingested)) = s.rx {
                        if r.client == i {
                            s.rx = None;
                        }
                    }
                }
                // ExtBackground: the URLSession upload goes on; the app gets
                // the result (or retries the same body on launch).
            }
            Action::Attack { direct } => {
                s.attacks -= 1;
                set_insert(
                    &mut s.net,
                    Req {
                        client: ATTACKER,
                        share: ATTACKER_SHARE,
                        owner: false,
                        direct,
                    },
                );
            }
            Action::DropRequest(r) => {
                s.net_faults -= 1;
                s.net.retain(|x| *x != r);
                self.respond(&mut s, r.client, false, false);
            }
            Action::Deliver(r) => {
                s.net.retain(|x| *x != r);
                if !r.owner {
                    // Through p1: forward_auth refuses (401), lucina-rs never
                    // sees it. Direct: no X-Share-Gateway → `authenticate` 401.
                    if !r.direct || self.key_check {
                        s.attack_rejected = true;
                        return Some(s);
                    }
                }
                if !s.up {
                    self.respond(&mut s, r.client, false, false); // 502
                } else if s.status[r.share as usize].is_some() {
                    self.respond(&mut s, r.client, true, true); // 202 duplicate
                } else {
                    s.rx = Some((r, Rx::New));
                }
            }
            Action::RxIngest => {
                let (r, _) = s.rx?;
                set_insert(&mut s.media, r.share);
                s.rx = Some((r, Rx::Ingested));
            }
            Action::RxSave => {
                let (r, _) = s.rx?;
                s.status[r.share as usize] = Some(Status::Processing);
                s.rx = Some((r, Rx::Saved));
            }
            Action::RxFinish | Action::RxFinishLost => {
                let (r, _) = s.rx.take()?;
                Share::ensure_user(&mut s, r.share);
                s.work.push(r.share);
                let ok = action == Action::RxFinish;
                if !ok {
                    s.net_faults -= 1;
                }
                self.respond(&mut s, r.client, ok, false);
            }
            Action::RxAppendFails => {
                let (r, _) = s.rx.take()?;
                s.append_faults -= 1;
                if self.fix_spawn_on_append_error {
                    s.work.push(r.share);
                }
                self.respond(&mut s, r.client, false, false); // 500
            }
            Action::ProcBegin => {
                let share = s.work.remove(0);
                Share::ensure_user(&mut s, share);
                if self.existing_reply(&s, share) {
                    s.status[share as usize] = Some(Status::Done);
                } else {
                    s.proc = Some(Proc {
                        share,
                        read: 0,
                        written: None,
                        heard: None,
                        tooled: false,
                    });
                }
            }
            Action::ProcRead => {
                s.proc.as_mut()?.read += 1;
            }
            Action::ProcHear(h) => {
                let p = s.proc.as_mut()?;
                p.read += 1;
                p.heard = Some(h);
            }
            Action::ProcTool => {
                let p = s.proc.as_mut()?;
                p.tooled = true;
                let share = p.share;
                if self.voice_tool_items {
                    s.timeline.push(Item::Tool(share));
                }
            }
            Action::ProcTurn | Action::ProcFail => {
                let p = s.proc.as_mut()?;
                let share = p.share;
                p.written = Some(if action == Action::ProcTurn {
                    Status::Done
                } else {
                    Status::Error
                });
                let heard = p.heard;
                let tooled = p.tooled;
                if action == Action::ProcFail {
                    s.proc_faults -= 1;
                    s.timeline.push(Item::Lucina(share));
                } else if self.is_voice(share) {
                    // Answered as if the transcript were typed, or "couldn't hear it".
                    s.voice_heard = heard;
                    s.voice_tooled = tooled;
                    s.timeline.push(if heard == Some(Heard::Failed) {
                        Item::Unheard(share)
                    } else {
                        Item::Lucina(share)
                    });
                } else {
                    s.timeline.push(Item::Lucina(share));
                }
            }
            Action::ProcSave => {
                let p = s.proc.take()?;
                s.status[p.share as usize] = p.written;
            }
            Action::OtherAppend => {
                s.others -= 1;
                s.timeline.push(Item::Other);
            }
            Action::Crash => {
                s.crashes -= 1;
                if let Some((r, _)) = s.rx.take() {
                    self.respond(&mut s, r.client, false, false); // connection reset
                }
                s.work.clear();
                s.proc = None;
                s.up = false;
                s.recovered = false;
            }
            Action::Restart => s.up = true,
            Action::Recover => {
                s.recovered = true;
                for share in 0..SHARES as u8 {
                    if s.status[share as usize] == Some(Status::Processing) {
                        s.work.push(share);
                        set_insert(&mut s.revived, share);
                    }
                }
            }
            Action::TimelineDeliver => {
                s.view.delivered = true;
                s.view.server = true;
                if self.reconcile_by_share_id {
                    s.view.local = false; // Timeline.merge: same meta.shareId
                }
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // SH1: one user item per accepted share.
            Property::always("sh1_one_user_item", |_, s: &State| {
                Share::owner_shares().iter().all(|&x| {
                    let n = count(&s.timeline, Item::User(x));
                    n <= 1
                        && (!matches!(s.status[x as usize], Some(Status::Done | Status::Error))
                            || n == 1)
                })
            }),
            // SH2: Lucina answers each share at most once, exactly once when done.
            Property::always("sh2_answered_once", |_, s: &State| {
                Share::owner_shares().iter().all(|&x| {
                    let n = answers(&s.timeline, x);
                    n <= 1
                        && (!matches!(s.status[x as usize], Some(Status::Done | Status::Error))
                            || n == 1)
                })
            }),
            // SH3: a retried shareId has still one user item and one answer.
            Property::always("sh3_retry_idempotent", |_, s: &State| {
                Share::owner_shares().iter().all(|&x| {
                    s.sent[x as usize] < 2
                        || (count(&s.timeline, Item::User(x)) <= 1 && answers(&s.timeline, x) <= 1)
                })
            }),
            // SH4: nothing ingested, stored or answered without owner auth.
            Property::always("sh4_owner_only", |_, s: &State| {
                let x = ATTACKER_SHARE;
                !set_has(&s.media, &x)
                    && s.status[x as usize].is_none()
                    && !s
                        .timeline
                        .iter()
                        .any(|i| matches!(i, Item::User(y) | Item::Lucina(y) | Item::Unheard(y) | Item::Tool(y) if *y == x))
            }),
            // SH5: the chat view shows the composer's share once.
            Property::always("sh5_one_bubble", |_, s: &State| {
                u8::from(s.view.local) + u8::from(s.view.server) <= 1
            }),
            // SH6: once nothing is in flight, no accepted share is left in
            // `processing` (it would wait for an unrelated restart).
            Property::always("sh6_accepted_completes", |_, s: &State| {
                !Share::quiet(s) || s.status.iter().all(|x| *x != Some(Status::Processing))
            }),
            // SH7: a voice note gets exactly one Lucina item once it is done,
            // whether its transcript came from the device, from whisper or
            // nowhere; it is "couldn't hear it" exactly when the answering run
            // had no transcript.
            Property::always("sh7_voice_one_item", |m: &Share, s: &State| {
                let x = COMPOSER_SHARE;
                let n = answers(&s.timeline, x);
                !m.voice
                    || (n <= 1
                        && (!matches!(s.status[x as usize], Some(Status::Done | Status::Error))
                            || n == 1)
                        && (count(&s.timeline, Item::Unheard(x)) == 1)
                            == (s.voice_heard == Some(Heard::Failed)))
            }),
            Property::sometimes("retry_hit_duplicate", |_, s: &State| s.dup_hit),
            Property::sometimes("background_kill_delivered", |_, s: &State| {
                s.clients.iter().any(|c| {
                    c.kind == Kind::ExtBackground && c.killed && c.phase == Phase::Accepted
                })
            }),
            Property::sometimes("foreground_kill_lost", |_, s: &State| {
                s.clients.iter().any(|c| {
                    c.kind == Kind::ExtForeground
                        && c.phase == Phase::Lost
                        && s.status[c.share as usize].is_none()
                        && s.rx.is_none_or(|(r, _)| r.share != c.share)
                })
            }),
            Property::sometimes("crash_recovery_completed", |_, s: &State| {
                s.revived
                    .iter()
                    .any(|&x| s.status[x as usize] == Some(Status::Done))
            }),
            Property::sometimes("album_answered_once", |m: &Share, s: &State| {
                let x = COMPOSER_SHARE;
                !m.voice
                    && m.album_files >= 2
                    && s.status[x as usize] == Some(Status::Done)
                    && count(&s.timeline, Item::User(x)) == 1
                    && count(&s.timeline, Item::Lucina(x)) == 1
            }),
            Property::sometimes("unauthenticated_refused", |_, s: &State| s.attack_rejected),
            Property::sometimes("voice_note_answered", |m: &Share, s: &State| {
                let x = COMPOSER_SHARE;
                m.voice
                    && matches!(s.voice_heard, Some(Heard::Device | Heard::Server))
                    && s.status[x as usize] == Some(Status::Done)
                    && count(&s.timeline, Item::User(x)) == 1
                    && answers(&s.timeline, x) == 1
            }),
            Property::sometimes("voice_device_answered", |m: &Share, s: &State| {
                m.voice
                    && s.voice_heard == Some(Heard::Device)
                    && s.status[COMPOSER_SHARE as usize] == Some(Status::Done)
                    && answers(&s.timeline, COMPOSER_SHARE) == 1
            }),
            Property::sometimes("voice_server_answered", |m: &Share, s: &State| {
                m.voice
                    && s.voice_heard == Some(Heard::Server)
                    && s.status[COMPOSER_SHARE as usize] == Some(Status::Done)
                    && answers(&s.timeline, COMPOSER_SHARE) == 1
            }),
            Property::sometimes("voice_tool_answered", |m: &Share, s: &State| {
                let x = COMPOSER_SHARE;
                m.tools_planner
                    && s.voice_tooled
                    && s.status[x as usize] == Some(Status::Done)
                    && count(&s.timeline, Item::User(x)) == 1
                    && answers(&s.timeline, x) == 1
            }),
            Property::sometimes("voice_unheard_answered", |m: &Share, s: &State| {
                let x = COMPOSER_SHARE;
                m.voice
                    && s.status[x as usize] == Some(Status::Done)
                    && count(&s.timeline, Item::User(x)) == 1
                    && answers(&s.timeline, x) == 1
                    && count(&s.timeline, Item::Unheard(x)) == 1
            }),
        ]
    }
}

pub const PROTOCOL: &str = "share";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    // The composer's album with every server-side fault and other writers.
    let solo = Share {
        ext_kinds: Vec::new(),
        album_files: 2,
        max_sends: 2,
        crashes: 1,
        net_faults: 1,
        append_faults: 1,
        proc_faults: 1,
        others: if deep { 2 } else { 1 },
        reshares: 0,
        attacks: 1,
        window: None,
        reconcile_by_share_id: true,
        key_check: true,
        fix_spawn_on_append_error: true,
        voice: false,
        unheard_reply_to: true,
        tools_planner: false,
        voice_tool_items: false,
    };
    // The composer next to one share extension (with / without App Group);
    // the append and process faults are covered by `solo`.
    let pair = |kind: Kind| Share {
        ext_kinds: vec![kind],
        append_faults: 0,
        proc_faults: if deep { 1 } else { 0 },
        others: 0,
        reshares: 1,
        attacks: 0,
        ..solo.clone()
    };
    let invariants =
        |sh2: Expect, sh3: Expect, sh4: Expect, sh5: Expect, sh6: Expect, sh7: Expect| {
            vec![
                ("sh1_one_user_item", Holds),
                ("sh2_answered_once", sh2),
                ("sh3_retry_idempotent", sh3),
                ("sh4_owner_only", sh4),
                ("sh5_one_bubble", sh5),
                ("sh6_accepted_completes", sh6),
                ("sh7_voice_one_item", sh7),
            ]
        };
    let all_hold = || invariants(Holds, Holds, Holds, Holds, Holds, Holds);
    // The composer sends one voice note (transcript on the device, from
    // whisper, or none) with every server-side fault and the attacker.
    let voice = Share {
        voice: true,
        ..solo.clone()
    };
    vec![
        Case::new(
            PROTOCOL,
            "SH fixed: composer album, crash, faults, other writers, attacker",
            solo.clone(),
            None,
            [
                all_hold(),
                vec![
                    ("retry_hit_duplicate", Reached),
                    ("crash_recovery_completed", Reached),
                    ("album_answered_once", Reached),
                    ("unauthenticated_refused", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH fixed: composer + extension with App Group (background)",
            pair(Kind::ExtBackground),
            None,
            [
                all_hold(),
                vec![
                    ("retry_hit_duplicate", Reached),
                    ("background_kill_delivered", Reached),
                    ("crash_recovery_completed", Reached),
                    ("album_answered_once", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH fixed: composer + extension without App Group (today)",
            pair(Kind::ExtForeground),
            None,
            [
                all_hold(),
                vec![
                    ("foreground_kill_lost", Reached),
                    ("retry_hit_duplicate", Reached),
                    ("crash_recovery_completed", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH window bug: recovery scans the newest N items",
            Share {
                window: Some(1),
                ..solo.clone()
            },
            None,
            [
                invariants(Violated, Violated, Holds, Holds, Holds, Holds),
                vec![
                    ("retry_hit_duplicate", Reached),
                    ("crash_recovery_completed", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH reconcile bug: chat matches only the 202's userItemId",
            Share {
                reconcile_by_share_id: false,
                ..solo.clone()
            },
            None,
            [
                invariants(Holds, Holds, Holds, Violated, Holds, Holds),
                vec![
                    ("retry_hit_duplicate", Reached),
                    ("album_answered_once", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH stuck bug: ensure_user error in receive (500, no spawn)",
            Share {
                fix_spawn_on_append_error: false,
                ..solo.clone()
            },
            None,
            [
                invariants(Holds, Holds, Holds, Holds, Violated, Holds),
                vec![
                    ("retry_hit_duplicate", Reached),
                    ("crash_recovery_completed", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH ablation: lucina-rs without the X-Share-Gateway check",
            Share {
                key_check: false,
                ..solo
            },
            None,
            [
                invariants(Holds, Holds, Violated, Holds, Holds, Holds),
                vec![("unauthenticated_refused", Reached)],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH fixed: voice note (device / whisper / no transcript), faults",
            voice.clone(),
            None,
            [
                all_hold(),
                vec![
                    ("retry_hit_duplicate", Reached),
                    ("crash_recovery_completed", Reached),
                    ("voice_note_answered", Reached),
                    ("voice_device_answered", Reached),
                    ("voice_server_answered", Reached),
                    ("voice_unheard_answered", Reached),
                    ("unauthenticated_refused", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH ablation: \"couldn't hear it\" item without replyTo",
            Share {
                unheard_reply_to: false,
                ..voice.clone()
            },
            None,
            [
                invariants(Violated, Violated, Holds, Holds, Holds, Violated),
                vec![
                    ("voice_note_answered", Reached),
                    ("voice_device_answered", Reached),
                    ("voice_server_answered", Reached),
                    ("voice_unheard_answered", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH fixed: voice note with the tools planner (no progress items)",
            Share {
                tools_planner: true,
                ..voice.clone()
            },
            None,
            [
                all_hold(),
                vec![
                    ("voice_note_answered", Reached),
                    ("voice_tool_answered", Reached),
                    ("voice_unheard_answered", Reached),
                    ("crash_recovery_completed", Reached),
                ],
            ]
            .concat(),
        ),
        Case::new(
            PROTOCOL,
            "SH ablation: tool progress items on a voice note",
            Share {
                tools_planner: true,
                voice_tool_items: true,
                ..voice
            },
            None,
            [
                invariants(Violated, Violated, Holds, Holds, Holds, Violated),
                vec![("voice_unheard_answered", Reached)],
            ]
            .concat(),
        ),
    ]
}
