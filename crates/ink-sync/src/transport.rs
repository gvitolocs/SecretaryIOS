//! The IO shell: wire types, the pure request handler, the [`Transport`]
//! trait, the [`Syncer`] driver (outbox + machine + transport), an in-memory
//! server and a flaky transport for tests (drops, duplicates, reorders,
//! partitions).
use crate::client::{Backoff, ClientMachine, Step};
use crate::op::{valid_device, Op, OpId, OpKind};
use crate::outbox::{Outbox, OutboxError};
use crate::state::{apply_mut, Conflict, SyncState};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// At most this many ops per request.
pub const MAX_OPS_PER_REQUEST: usize = 500;

/// `POST /v1/library/sync` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncRequest {
    pub device: String,
    pub ops: Vec<Op>,
}

/// `POST /v1/library/sync` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResponse {
    /// Every op the server has recorded (fresh, duplicate, superseded,
    /// protected, rejected or tombstoned). Deferred ops are missing.
    pub acked: Vec<OpId>,
    /// Conflicts this request produced.
    pub conflicts: Vec<Conflict>,
    pub server_clock: u64,
}

/// Checks the envelope: the device id and that every op belongs to it.
pub fn check_request(req: &SyncRequest) -> Result<(), String> {
    if !valid_device(&req.device) {
        return Err("invalid device id".into());
    }
    if req.ops.len() > MAX_OPS_PER_REQUEST {
        return Err(format!("at most {MAX_OPS_PER_REQUEST} ops per request"));
    }
    if req.ops.iter().any(|o| o.op_id.device != req.device) {
        return Err("every opId must belong to the request's device".into());
    }
    Ok(())
}

/// The server side of one request, pure: apply every op in order.
pub fn serve(state: &mut SyncState, req: &SyncRequest) -> Result<SyncResponse, String> {
    check_request(req)?;
    let mut acked = Vec::new();
    let mut conflicts = Vec::new();
    for op in &req.ops {
        let o = apply_mut(state, op);
        if o.verdict.acked() {
            acked.push(op.op_id.clone());
        }
        conflicts.extend(o.conflict);
    }
    Ok(SyncResponse {
        acked,
        conflicts,
        server_clock: state.clock,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// No answer (connection lost, timeout): the server may or may not have
    /// applied the request.
    #[error("network: {0}")]
    Network(String),
    /// The server refused the request (4xx/5xx).
    #[error("http {0}: {1}")]
    Status(u16, String),
}

pub trait Transport {
    fn send(&mut self, req: &SyncRequest) -> Result<SyncResponse, TransportError>;
}

/// An in-memory server shared by several devices.
#[derive(Debug, Clone, Default)]
pub struct MemServer(pub Arc<Mutex<SyncState>>);

impl MemServer {
    pub fn state(&self) -> SyncState {
        self.0.lock().expect("server lock").clone()
    }
}

impl Transport for MemServer {
    fn send(&mut self, req: &SyncRequest) -> Result<SyncResponse, TransportError> {
        let mut s = self.0.lock().expect("server lock");
        serve(&mut s, req).map_err(|e| TransportError::Status(400, e))
    }
}

/// SplitMix64: a small deterministic generator for jitter and fault injection.
#[derive(Debug, Clone)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// True with probability `percent`/100.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }
}

/// Fault percentages of a [`FlakyTransport`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Faults {
    /// The request is lost before the server sees it.
    pub drop_request: u64,
    /// The server applies the request, the response is lost.
    pub drop_response: u64,
    /// The server receives the request twice.
    pub duplicate: u64,
    /// The request is held back and delivered later, after newer ones
    /// (the client sees a timeout).
    pub delay: u64,
    /// A partition starts: the next 1–5 sends fail.
    pub partition: u64,
}

/// Wraps a transport with injected faults.
pub struct FlakyTransport<T> {
    pub inner: T,
    pub rng: SplitMix64,
    pub faults: Faults,
    partition_left: u32,
    delayed: Vec<SyncRequest>,
    /// Whether faults are injected (turn off to let a test settle).
    pub enabled: bool,
}

impl<T: Transport> FlakyTransport<T> {
    pub fn new(inner: T, seed: u64, faults: Faults) -> Self {
        FlakyTransport {
            inner,
            rng: SplitMix64(seed),
            faults,
            partition_left: 0,
            delayed: Vec::new(),
            enabled: true,
        }
    }

    /// Delivers every held-back request (their responses are lost).
    pub fn flush_delayed(&mut self) {
        for req in std::mem::take(&mut self.delayed) {
            let _ = self.inner.send(&req);
        }
    }
}

impl<T: Transport> Transport for FlakyTransport<T> {
    fn send(&mut self, req: &SyncRequest) -> Result<SyncResponse, TransportError> {
        let lost = || Err(TransportError::Network("injected".into()));
        if !self.enabled {
            return self.inner.send(req);
        }
        // Reorder: an older held-back request may land first.
        if !self.delayed.is_empty() && self.rng.chance(50) {
            let i = (self.rng.next_u64() % self.delayed.len() as u64) as usize;
            let old = self.delayed.remove(i);
            let _ = self.inner.send(&old);
        }
        if self.partition_left > 0 {
            self.partition_left -= 1;
            return lost();
        }
        if self.rng.chance(self.faults.partition) {
            self.partition_left = 1 + (self.rng.next_u64() % 5) as u32;
            return lost();
        }
        if self.rng.chance(self.faults.drop_request) {
            return lost();
        }
        if self.rng.chance(self.faults.delay) {
            self.delayed.push(req.clone());
            return lost();
        }
        let resp = self.inner.send(req);
        if self.rng.chance(self.faults.duplicate) {
            let _ = self.inner.send(req);
        }
        if self.rng.chance(self.faults.drop_response) {
            return lost();
        }
        resp
    }
}

/// The device-side driver: durable outbox, pure machine, a transport.
pub struct Syncer<T> {
    pub outbox: Outbox,
    pub machine: ClientMachine,
    pub transport: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progressed {
    /// A request went out; `acked` ops were acknowledged (durably).
    Sent {
        acked: usize,
    },
    /// The request failed; retry after the backoff.
    Failed,
    Wait(u64),
    Idle,
}

impl<T: Transport> Syncer<T> {
    pub fn open(
        path: impl AsRef<std::path::Path>,
        device: &str,
        transport: T,
        max_batch: usize,
        backoff: Backoff,
    ) -> Result<Self, OutboxError> {
        Ok(Syncer {
            outbox: Outbox::open(path, device)?,
            machine: ClientMachine::new(max_batch, backoff),
            transport,
        })
    }

    /// Local-first: the op is durable before this returns; no network I/O.
    pub fn record(&mut self, at: impl Into<String>, kind: OpKind) -> std::io::Result<Op> {
        self.outbox.record(at, kind)
    }

    /// One step of the loop at time `now` (ms); `rand` feeds the jitter.
    pub fn step(&mut self, now: u64, rand: u64) -> std::io::Result<Progressed> {
        let pending: Vec<OpId> = self
            .outbox
            .pending()
            .iter()
            .map(|o| o.op_id.clone())
            .collect();
        match self.machine.next(now, &pending) {
            Step::Idle => Ok(Progressed::Idle),
            Step::WaitUntil(t) => Ok(Progressed::Wait(t)),
            Step::InFlight => Ok(Progressed::Wait(now)),
            Step::Send(batch) => {
                let req = SyncRequest {
                    device: self.outbox.device().to_owned(),
                    ops: self
                        .outbox
                        .pending()
                        .iter()
                        .filter(|o| batch.contains(&o.op_id))
                        .cloned()
                        .collect(),
                };
                match self.transport.send(&req) {
                    Ok(resp) => {
                        // Durable before the machine moves on.
                        let acked = self.outbox.ack(&resp.acked)?;
                        self.machine.on_response(now, &resp.acked, rand);
                        Ok(Progressed::Sent { acked })
                    }
                    Err(_) => {
                        self.machine.on_failure(now, rand);
                        Ok(Progressed::Failed)
                    }
                }
            }
        }
    }
}
