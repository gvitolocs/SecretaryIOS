// Lucina Link route policy kernel (which route carries which traffic, and which healthy route wins).
//
// Production (branch lucina-link):
//   allowed   <-> lucina_link::policy::allowed / denial      (crates/lucina-link/src/policy.rs)
//   rank      <-> lucina_link::policy::rank (position in policy::preference)
//   select    <-> lucina_link::policy::select, used by probe::probe_service and link::Link::route
//   Cand.up   <-> policy::Health::Up { rtt_ms }  (Down, Unknown and policy-skipped routes are not up)
// Differential test: crates/lucina-link/tests/verus_policy.rs (exhaustive grid, plain Rust copies of the exec fns).
//
// Proven:
//   L1 video_never_cloudflare: a selected route is never Cloudflare for a Bulk Video transfer.
//   L2 cloudflare_respects_cap: a selected Cloudflare route carries at most CLOUDFLARE_BODY_CAP bytes, and a
//      non-video Bulk transfer through it at most CLOUDFLARE_BULK_MAX.
//   L3 selected_is_allowed_and_up (select's postcondition).
//   L4 selected_is_best: no other usable candidate has a better rank, or the same rank and a lower rtt.
//   L5 none_only_when_nothing_usable: select returns None iff no candidate is allowed and up.
//   L6 ssh_jump_only_for_shell: a selected SshJump route only ever carries a shell.
use vstd::prelude::*;

verus! {

pub enum Kind {
    Lan,
    Tailnet,
    Public,
    Cloudflare,
    SshJump,
}

pub enum Media {
    None,
    Image,
    Audio,
    Video,
    Other,
}

pub enum Traffic {
    Api { body: u64 },
    Shell,
    Bulk { bytes: u64, media: Media },
}

pub const CLOUDFLARE_BODY_CAP: u64 = 100_000_000;

pub const CLOUDFLARE_BULK_MAX: u64 = 67_108_864;

pub open spec fn body_spec(t: Traffic) -> u64 {
    match t {
        Traffic::Api { body } => body,
        Traffic::Shell => 0,
        Traffic::Bulk { bytes, .. } => bytes,
    }
}

pub open spec fn is_shell(t: Traffic) -> bool {
    match t {
        Traffic::Shell => true,
        _ => false,
    }
}

pub open spec fn is_video(t: Traffic) -> bool {
    match t {
        Traffic::Bulk { media, .. } => match media {
            Media::Video => true,
            _ => false,
        },
        _ => false,
    }
}

pub open spec fn is_bulk(t: Traffic) -> bool {
    match t {
        Traffic::Bulk { .. } => true,
        _ => false,
    }
}

pub open spec fn is_cloudflare(k: Kind) -> bool {
    match k {
        Kind::Cloudflare => true,
        _ => false,
    }
}

pub open spec fn is_ssh_jump(k: Kind) -> bool {
    match k {
        Kind::SshJump => true,
        _ => false,
    }
}

/// Preference: Api/Bulk = Lan, Tailnet, Public, Cloudflare; Shell = Lan, Tailnet, Cloudflare, SshJump.
pub open spec fn rank_spec(k: Kind, t: Traffic) -> Option<u64> {
    if is_shell(t) {
        match k {
            Kind::Lan => Some(0),
            Kind::Tailnet => Some(1),
            Kind::Cloudflare => Some(2),
            Kind::SshJump => Some(3),
            Kind::Public => None,
        }
    } else {
        match k {
            Kind::Lan => Some(0),
            Kind::Tailnet => Some(1),
            Kind::Public => Some(2),
            Kind::Cloudflare => Some(3),
            Kind::SshJump => None,
        }
    }
}

pub open spec fn cloudflare_ok(t: Traffic) -> bool {
    &&& !is_video(t)
    &&& body_spec(t) <= CLOUDFLARE_BODY_CAP
    &&& (is_bulk(t) ==> body_spec(t) <= CLOUDFLARE_BULK_MAX)
}

pub open spec fn allowed_spec(k: Kind, t: Traffic, max_body: Option<u64>) -> bool {
    &&& rank_spec(k, t) is Some
    &&& (max_body is Some ==> body_spec(t) <= max_body->0)
    &&& (is_cloudflare(k) ==> cloudflare_ok(t))
}

pub fn body(t: &Traffic) -> (r: u64)
    ensures
        r == body_spec(*t),
{
    match t {
        Traffic::Api { body } => *body,
        Traffic::Shell => 0,
        Traffic::Bulk { bytes, .. } => *bytes,
    }
}

pub fn rank(k: &Kind, t: &Traffic) -> (r: Option<u64>)
    ensures
        r == rank_spec(*k, *t),
{
    let shell = match t {
        Traffic::Shell => true,
        _ => false,
    };
    if shell {
        match k {
            Kind::Lan => Some(0),
            Kind::Tailnet => Some(1),
            Kind::Cloudflare => Some(2),
            Kind::SshJump => Some(3),
            Kind::Public => None,
        }
    } else {
        match k {
            Kind::Lan => Some(0),
            Kind::Tailnet => Some(1),
            Kind::Public => Some(2),
            Kind::Cloudflare => Some(3),
            Kind::SshJump => None,
        }
    }
}

pub fn allowed(k: &Kind, t: &Traffic, max_body: Option<u64>) -> (r: bool)
    ensures
        r == allowed_spec(*k, *t, max_body),
{
    if rank(k, t).is_none() {
        return false;
    }
    let b = body(t);
    match max_body {
        Some(m) => {
            if b > m {
                return false;
            }
        },
        None => {},
    }
    match k {
        Kind::Cloudflare => {
            let video = match t {
                Traffic::Bulk { media, .. } => match media {
                    Media::Video => true,
                    _ => false,
                },
                _ => false,
            };
            let bulk = match t {
                Traffic::Bulk { .. } => true,
                _ => false,
            };
            !video && b <= CLOUDFLARE_BODY_CAP && (!bulk || b <= CLOUDFLARE_BULK_MAX)
        },
        _ => true,
    }
}

pub struct Cand {
    pub kind: Kind,
    pub max_body: Option<u64>,
    pub up: bool,
    pub rtt: u32,
}

pub open spec fn usable(c: Cand, t: Traffic) -> bool {
    c.up && allowed_spec(c.kind, t, c.max_body)
}

pub open spec fn rank_of(c: Cand, t: Traffic) -> u64 {
    rank_spec(c.kind, t)->0
}

/// `a` beats `b`: a better rank, or the same rank and a lower rtt.
pub open spec fn better(a: Cand, b: Cand, t: Traffic) -> bool {
    rank_of(a, t) < rank_of(b, t) || (rank_of(a, t) == rank_of(b, t) && a.rtt < b.rtt)
}

pub fn select(cs: &Vec<Cand>, t: &Traffic) -> (r: Option<usize>)
    ensures
        // L3, L4
        r is Some ==> {
            &&& r->0 < cs.len()
            &&& usable(cs@[r->0 as int], *t)
            &&& forall|j: int| 0 <= j < cs.len() && usable(#[trigger] cs@[j], *t) ==> !better(cs@[j], cs@[r->0 as int], *t)
        },
        // L5
        r is None <==> forall|j: int| 0 <= j < cs.len() ==> !usable(#[trigger] cs@[j], *t),
{
    let mut best: Option<usize> = None;
    let mut i: usize = 0;
    while i < cs.len()
        invariant
            0 <= i <= cs.len(),
            best is Some ==> {
                &&& best->0 < i
                &&& usable(cs@[best->0 as int], *t)
                &&& forall|j: int| 0 <= j < i && usable(#[trigger] cs@[j], *t) ==> !better(cs@[j], cs@[best->0 as int], *t)
            },
            best is None ==> forall|j: int| 0 <= j < i ==> !usable(#[trigger] cs@[j], *t),
        decreases cs.len() - i,
    {
        if cs[i].up && allowed(&cs[i].kind, t, cs[i].max_body) {
            let ri = match rank(&cs[i].kind, t) {
                Some(x) => x,
                None => 0,
            };
            match best {
                None => {
                    best = Some(i);
                },
                Some(b) => {
                    let rb = match rank(&cs[b].kind, t) {
                        Some(x) => x,
                        None => 0,
                    };
                    if ri < rb || (ri == rb && cs[i].rtt < cs[b].rtt) {
                        best = Some(i);
                    }
                },
            }
        }
        i = i + 1;
    }
    best
}

// L1
proof fn video_never_cloudflare(c: Cand, t: Traffic)
    requires
        usable(c, t),
    ensures
        is_video(t) ==> !is_cloudflare(c.kind),
{
}

// L2
proof fn cloudflare_respects_cap(c: Cand, t: Traffic)
    requires
        usable(c, t),
        is_cloudflare(c.kind),
    ensures
        body_spec(t) <= CLOUDFLARE_BODY_CAP,
        is_bulk(t) && !is_video(t) ==> body_spec(t) <= CLOUDFLARE_BULK_MAX,
{
}

// L6
proof fn ssh_jump_only_for_shell(c: Cand, t: Traffic)
    requires
        usable(c, t),
        is_ssh_jump(c.kind),
    ensures
        is_shell(t),
{
}

/// L1, L2 and L6 hold for whatever select returns.
proof fn selected_route_obeys_policy(cs: Seq<Cand>, t: Traffic, r: int)
    requires
        0 <= r < cs.len(),
        usable(cs[r], t),
    ensures
        is_video(t) ==> !is_cloudflare(cs[r].kind),
        is_cloudflare(cs[r].kind) ==> body_spec(t) <= CLOUDFLARE_BODY_CAP,
        is_ssh_jump(cs[r].kind) ==> is_shell(t),
{
}

fn main() {
}

} // verus!
