//! Lucina media library: the iPhone photo backup against lucina-media behind p1
//! (`ios/Secretary/Features/Gallery/PhotoBackup.swift`, `crates/lucina-media/src/http.rs`; the
//! pure kernels are `crates/lucina-media/src/protocol.rs`, proved in
//! `verification/verus/media_upload.rs`). TLA+ twin: `formal/media/MediaBackup.tla`.
//!
//! Two devices back up: device 0 has photos 0 and 1, device 1 has photo 0 too (the same bytes,
//! e.g. AirDropped). Each photo is `SIZE` bytes, its hash is its number. Per photo the device
//! sends POST /uploads (create), PUT /uploads/{id}?offset=known (the rest of the file), and
//! POST /finish. The environment:
//!
//! - a response can be lost (background URLSession cut-off) and a PUT can arrive truncated; the
//!   device then resends the same request from what it last heard (`known`), so the server sees
//!   overlapping bytes;
//! - lucina-media can restart (losing whatever request is between its steps);
//! - p1 lets `limit` requests per device through per window (every request costs one: the
//!   forward_auth to /v1/avatar is what counts), then answers 429 until the window ticks.
//!
//! Server steps are atomic except finish, which is three steps as in `ingest::ingest_file` and
//! `http::finish_upload`: decide (`known_for` + `ingest_decision` under the catalog lock; a
//! duplicate's spool file is removed here), insert (`store::place_new` moves the file, then
//! `insert_item` under the lock again), mark (`UPDATE uploads SET finished = 1`). A restart
//! can fall between any two (a client hang-up could too before the finish moved into one
//! blocking task; a process crash still can).
//!
//! The fixes, one switch each:
//! - `gone_410`: a finish whose spool file is gone answers 410 and the client starts the asset
//!   again (before: the hash of the missing file failed, 500 on every retry);
//! - `pause_on_429`: the client waits for the window after a 429 (before: retried at once);
//! - `recheck_ingest` (M3, fixed in cc5e663): the insert step decides again under the lock, so a
//!   concurrent finish of the same bytes becomes a duplicate instead of a second item.

use stateright::{Model, Property};

use crate::{Case, Expect};

pub const SIZE: u8 = 2;
pub const PHOTOS: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Upload {
    pub dev: u8,
    pub photo: u8,
    pub received: u8,
    /// The spool file is there (moved into the store, or deleted as a duplicate, on finish).
    pub file: bool,
    pub finished: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Item {
    pub photo: u8,
    /// The size of the object in the store.
    pub bytes: u8,
    /// The phone asset that made it (the `by_asset` key, with `photo`).
    pub dev: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Create,
    Put,
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Insert,
    Mark,
}

/// A finish between its server steps; `ok`: its response will arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Finishing {
    pub upload: u8,
    pub ok: bool,
    pub phase: Phase,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Device {
    /// Photos still to back up; the first is the current one.
    pub queue: Vec<u8>,
    pub stage: Stage,
    pub upload: Option<u8>,
    /// The offset the device last heard from the server.
    pub known: u8,
    /// Waiting for the window after a 429.
    pub paused: bool,
    /// Requests (and 429s) in this window.
    pub sent: u8,
    pub rejected: u8,
    pub finishing: Option<Finishing>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub uploads: Vec<Upload>,
    pub items: Vec<Item>,
    pub devices: Vec<Device>,
    pub ticks: u8,
    pub losses: u8,
    pub restarts: u8,
    pub budget: u8,
    pub resumed: bool,
    pub rate_limited: bool,
    pub dedupe_hit: bool,
    /// A finish met a lost upload and got 500 (the client can only send it again).
    pub dead_end: bool,
    /// A finish met a lost upload, got 410 and the client started the asset again.
    pub restarted_after_410: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Delivery {
    Ok,
    /// The server did the work, the response never arrived.
    Lost,
    /// A PUT cut after this many bytes (no response).
    Cut(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Request(u8, Delivery),
    /// The device's finish takes its next server step.
    FinishStep(u8),
    Restart,
    Tick,
}

#[derive(Clone, Debug)]
pub struct MediaBackup {
    pub gone_410: bool,
    pub pause_on_429: bool,
    pub recheck_ingest: bool,
    pub limit: u8,
    pub ticks: u8,
    pub losses: u8,
    pub restarts: u8,
    pub budget: u8,
}

pub enum Chunk {
    Append { skip: u8, new_offset: u8 },
    AlreadyHave,
    Rejected,
}

/// `protocol::accept_chunk` (proved in `verification/verus/media_upload.rs`).
pub fn accept_chunk(received: u8, finished: bool, offset: u8, len: u8) -> Chunk {
    let end = offset + len;
    if finished || end > SIZE || offset > received {
        return Chunk::Rejected;
    }
    if end <= received && len > 0 {
        return Chunk::AlreadyHave;
    }
    Chunk::Append {
        skip: received - offset,
        new_offset: end.max(received),
    }
}

impl MediaBackup {
    fn can_send(s: &State, d: usize) -> bool {
        let dv = &s.devices[d];
        s.budget > 0 && !dv.paused && !dv.queue.is_empty() && dv.finishing.is_none()
    }

    fn photo_done(dv: &mut Device) {
        dv.queue.remove(0);
        dv.stage = Stage::Create;
        dv.upload = None;
        dv.known = 0;
    }

    fn request(&self, s: &mut State, d: usize, delivery: Delivery) {
        s.budget -= 1;
        match delivery {
            Delivery::Ok => {}
            _ => s.losses -= 1,
        }
        let dv = &mut s.devices[d];
        dv.sent += 1;
        if dv.sent > self.limit {
            // 429 from p1; the request never reaches lucina-media.
            dv.rejected += 1;
            s.rate_limited = true;
            if self.pause_on_429 {
                dv.paused = true;
            }
            return;
        }
        let ok = delivery == Delivery::Ok;
        let photo = dv.queue[0];
        match dv.stage {
            Stage::Create => {
                let i = s.uploads.len() as u8;
                s.uploads.push(Upload {
                    dev: d as u8,
                    photo,
                    received: 0,
                    file: true,
                    finished: false,
                });
                if ok {
                    dv.upload = Some(i);
                    dv.known = 0;
                    dv.stage = Stage::Put;
                }
            }
            Stage::Put => {
                let u = &mut s.uploads[dv.upload.unwrap() as usize];
                let len = match delivery {
                    Delivery::Cut(n) => n,
                    _ => SIZE - dv.known,
                };
                match accept_chunk(u.received, u.finished, dv.known, len) {
                    Chunk::Append { skip, new_offset } => {
                        s.resumed |= skip > 0;
                        u.received = new_offset;
                    }
                    Chunk::AlreadyHave => s.resumed = true,
                    // The device only sends from an offset it heard, and offsets only grow.
                    Chunk::Rejected => unreachable!("chunk rejected: {u:?} at {}", dv.known),
                }
                if ok {
                    dv.known = u.received;
                    if u.received == SIZE {
                        dv.stage = Stage::Finish;
                    }
                }
            }
            Stage::Finish => {
                let i = dv.upload.unwrap();
                let u = s.uploads[i as usize].clone();
                if u.finished {
                    // Finish again (the first answer was lost): the same item.
                    if ok {
                        Self::photo_done(dv);
                    }
                } else if !u.file {
                    if self.gone_410 {
                        // 410 "upload lost; start again".
                        if ok {
                            dv.upload = None;
                            dv.stage = Stage::Create;
                            s.restarted_after_410 = true;
                        }
                    } else {
                        // 500: the device skips the asset and the next pass sends the same
                        // finish again.
                        s.dead_end = true;
                    }
                } else if u.received < SIZE {
                    if ok {
                        dv.known = u.received;
                        dv.stage = Stage::Put;
                    }
                } else {
                    // check_finish passed: ingest_decision on the whole file.
                    let phase = match Self::known(s, d as u8, photo) {
                        Some(other_device) => {
                            // Duplicate: the spool file is removed, nothing inserted.
                            s.dedupe_hit |= other_device;
                            s.uploads[i as usize].file = false;
                            Phase::Mark
                        }
                        None => Phase::Insert,
                    };
                    s.devices[d].finishing = Some(Finishing {
                        upload: i,
                        ok,
                        phase,
                    });
                }
            }
        }
    }
}

impl MediaBackup {
    /// `ingest_decision` says Duplicate: `Some(true)` when the bytes came from the other device.
    fn known(s: &State, d: u8, photo: u8) -> Option<bool> {
        if s.items.iter().any(|it| it.photo == photo && it.dev == d) {
            return Some(false);
        }
        s.items
            .iter()
            .find(|it| it.photo == photo)
            .map(|it| it.dev != d)
    }

    fn finish_step(&self, s: &mut State, d: usize) {
        let f = s.devices[d].finishing.unwrap();
        let i = f.upload as usize;
        match f.phase {
            Phase::Insert => {
                let u = s.uploads[i].clone();
                let dup = if self.recheck_ingest {
                    Self::known(s, d as u8, u.photo)
                } else {
                    None
                };
                match dup {
                    Some(other_device) => s.dedupe_hit |= other_device,
                    None => s.items.push(Item {
                        photo: u.photo,
                        bytes: u.received,
                        dev: d as u8,
                    }),
                }
                s.uploads[i].file = false;
                s.devices[d].finishing = Some(Finishing {
                    phase: Phase::Mark,
                    ..f
                });
            }
            Phase::Mark => {
                s.uploads[i].finished = true;
                let dv = &mut s.devices[d];
                dv.finishing = None;
                if f.ok {
                    Self::photo_done(dv);
                }
            }
        }
    }
}

impl Model for MediaBackup {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let device = |queue: Vec<u8>| Device {
            queue,
            stage: Stage::Create,
            upload: None,
            known: 0,
            paused: false,
            sent: 0,
            rejected: 0,
            finishing: None,
        };
        vec![State {
            uploads: Vec::new(),
            items: Vec::new(),
            devices: vec![device(vec![0, 1]), device(vec![0])],
            ticks: self.ticks,
            losses: self.losses,
            restarts: self.restarts,
            budget: self.budget,
            resumed: false,
            rate_limited: false,
            dedupe_hit: false,
            dead_end: false,
            restarted_after_410: false,
        }]
    }

    fn actions(&self, s: &State, a: &mut Vec<Action>) {
        for d in 0..s.devices.len() {
            if !Self::can_send(s, d) {
                continue;
            }
            let d8 = d as u8;
            a.push(Action::Request(d8, Delivery::Ok));
            if s.losses > 0 {
                a.push(Action::Request(d8, Delivery::Lost));
                let dv = &s.devices[d];
                if dv.stage == Stage::Put {
                    for n in 1..SIZE - dv.known {
                        a.push(Action::Request(d8, Delivery::Cut(n)));
                    }
                }
            }
        }
        for (d, dv) in s.devices.iter().enumerate() {
            if dv.finishing.is_some() {
                a.push(Action::FinishStep(d as u8));
            }
        }
        if s.restarts > 0 {
            a.push(Action::Restart);
        }
        if s.ticks > 0 && s.devices.iter().any(|d| d.sent > 0) {
            a.push(Action::Tick);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        match action {
            Action::Request(d, delivery) => self.request(&mut s, d as usize, delivery),
            Action::FinishStep(d) => self.finish_step(&mut s, d as usize),
            Action::Restart => {
                s.restarts -= 1;
                // A finish between its steps is lost; the device sees a transport error and
                // sends the finish again.
                for d in &mut s.devices {
                    d.finishing = None;
                }
            }
            Action::Tick => {
                s.ticks -= 1;
                for d in &mut s.devices {
                    d.sent = 0;
                    d.rejected = 0;
                    d.paused = false;
                }
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("stored_once", |_, s: &State| {
                (0..PHOTOS).all(|p| s.items.iter().filter(|it| it.photo == p).count() <= 1)
            }),
            Property::always("no_partial_object", |_, s: &State| {
                s.items.iter().all(|it| it.bytes == SIZE)
            }),
            Property::always("no_dead_end_finish", |_, s: &State| !s.dead_end),
            Property::always("no_request_storm", |m: &MediaBackup, s: &State| {
                s.devices.iter().all(|d| d.sent <= m.limit + 1)
            }),
            Property::always("done_means_backed_up", |_, s: &State| {
                s.devices.iter().any(|d| !d.queue.is_empty())
                    || (0..PHOTOS).all(|p| s.items.iter().any(|it| it.photo == p))
            }),
            Property::sometimes("resumed", |_, s: &State| s.resumed),
            Property::sometimes("rate_limited", |_, s: &State| s.rate_limited),
            Property::sometimes("dedupe_hit", |_, s: &State| s.dedupe_hit),
            Property::sometimes("lost_upload_restarted", |_, s: &State| {
                s.restarted_after_410
            }),
            Property::sometimes("all_backed_up", |_, s: &State| {
                s.devices.iter().all(|d| d.queue.is_empty())
            }),
        ]
    }
}

pub const PROTOCOL: &str = "media-backup";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let model = |gone_410, pause_on_429, recheck_ingest| MediaBackup {
        gone_410,
        pause_on_429,
        recheck_ingest,
        limit: 3,
        ticks: 2,
        losses: if deep { 3 } else { 2 },
        restarts: 1,
        budget: if deep { 18 } else { 15 },
    };
    let expect = |once: Expect, dead_end: Expect, storm: Expect| {
        let mut v = vec![
            ("stored_once", once),
            ("no_partial_object", Holds),
            ("no_dead_end_finish", dead_end),
            ("no_request_storm", storm),
            ("done_means_backed_up", Holds),
            ("resumed", Reached),
            ("rate_limited", Reached),
            ("dedupe_hit", Reached),
            ("all_backed_up", Reached),
        ];
        if dead_end == Holds {
            v.push(("lost_upload_restarted", Reached));
        }
        v
    };
    vec![
        Case::new(
            PROTOCOL,
            "M before fixes (500 on a lost upload, retry on 429)",
            model(false, false, false),
            None,
            expect(Violated, Violated, Violated),
        ),
        Case::new(
            PROTOCOL,
            "M before fix M3 (ingest decided once)",
            model(true, true, false),
            None,
            expect(Violated, Holds, Holds),
        ),
        Case::new(
            PROTOCOL,
            "M current (M3 fixed: ingest decides again under the lock)",
            model(true, true, true),
            None,
            expect(Holds, Holds, Holds),
        ),
    ]
}
