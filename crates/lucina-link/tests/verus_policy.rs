//! Differential test: `lucina_link::policy` against plain Rust copies of the executable functions proven in
//! `verification/verus/link_policy.rs`, over an exhaustive grid of kinds, traffic shapes, boundary sizes, route caps
//! and health, with every candidate list of up to 3 routes.

use lucina_link::policy::{self, Candidate, Health, Media, RouteKind, Traffic};

mod verus_copy {
    // Same shapes and bodies as the Verus file (exec fns only).
    #[derive(Clone, Copy)]
    pub enum Kind {
        Lan,
        Tailnet,
        Public,
        Cloudflare,
        SshJump,
    }
    #[derive(Clone, Copy)]
    pub enum Media {
        None,
        Image,
        Audio,
        Video,
        Other,
    }
    #[derive(Clone, Copy)]
    pub enum Traffic {
        Api { body: u64 },
        Shell,
        Bulk { bytes: u64, media: Media },
    }
    pub const CLOUDFLARE_BODY_CAP: u64 = 100_000_000;
    pub const CLOUDFLARE_BULK_MAX: u64 = 67_108_864;

    pub fn body(t: &Traffic) -> u64 {
        match t {
            Traffic::Api { body } => *body,
            Traffic::Shell => 0,
            Traffic::Bulk { bytes, .. } => *bytes,
        }
    }

    pub fn rank(k: &Kind, t: &Traffic) -> Option<u64> {
        let shell = matches!(t, Traffic::Shell);
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

    pub fn allowed(k: &Kind, t: &Traffic, max_body: Option<u64>) -> bool {
        if rank(k, t).is_none() {
            return false;
        }
        let b = body(t);
        if let Some(m) = max_body {
            if b > m {
                return false;
            }
        }
        match k {
            Kind::Cloudflare => {
                let video = matches!(
                    t,
                    Traffic::Bulk {
                        media: Media::Video,
                        ..
                    }
                );
                let bulk = matches!(t, Traffic::Bulk { .. });
                !video && b <= CLOUDFLARE_BODY_CAP && (!bulk || b <= CLOUDFLARE_BULK_MAX)
            }
            _ => true,
        }
    }

    pub struct Cand {
        pub kind: Kind,
        pub max_body: Option<u64>,
        pub up: bool,
        pub rtt: u32,
    }

    pub fn select(cs: &[Cand], t: &Traffic) -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut i = 0;
        while i < cs.len() {
            if cs[i].up && allowed(&cs[i].kind, t, cs[i].max_body) {
                let ri = rank(&cs[i].kind, t).unwrap_or(0);
                match best {
                    None => best = Some(i),
                    Some(b) => {
                        let rb = rank(&cs[b].kind, t).unwrap_or(0);
                        if ri < rb || (ri == rb && cs[i].rtt < cs[b].rtt) {
                            best = Some(i);
                        }
                    }
                }
            }
            i += 1;
        }
        best
    }
}

const KINDS: [RouteKind; 5] = [
    RouteKind::Lan,
    RouteKind::Tailnet,
    RouteKind::Public,
    RouteKind::Cloudflare,
    RouteKind::SshJump,
];
const MEDIA: [Media; 5] = [
    Media::None,
    Media::Image,
    Media::Audio,
    Media::Video,
    Media::Other,
];

fn vk(k: RouteKind) -> verus_copy::Kind {
    match k {
        RouteKind::Lan => verus_copy::Kind::Lan,
        RouteKind::Tailnet => verus_copy::Kind::Tailnet,
        RouteKind::Public => verus_copy::Kind::Public,
        RouteKind::Cloudflare => verus_copy::Kind::Cloudflare,
        RouteKind::SshJump => verus_copy::Kind::SshJump,
    }
}

fn vm(m: Media) -> verus_copy::Media {
    match m {
        Media::None => verus_copy::Media::None,
        Media::Image => verus_copy::Media::Image,
        Media::Audio => verus_copy::Media::Audio,
        Media::Video => verus_copy::Media::Video,
        Media::Other => verus_copy::Media::Other,
    }
}

fn vt(t: Traffic) -> verus_copy::Traffic {
    match t {
        Traffic::Api { body_bytes } => verus_copy::Traffic::Api { body: body_bytes },
        Traffic::Shell => verus_copy::Traffic::Shell,
        Traffic::Bulk { bytes, media } => verus_copy::Traffic::Bulk {
            bytes,
            media: vm(media),
        },
    }
}

fn sizes() -> Vec<u64> {
    let cap = policy::CLOUDFLARE_BODY_CAP;
    let bulk = policy::CLOUDFLARE_BULK_MAX;
    vec![
        0,
        1,
        bulk - 1,
        bulk,
        bulk + 1,
        cap - 1,
        cap,
        cap + 1,
        u64::MAX,
    ]
}

fn traffics() -> Vec<Traffic> {
    let mut v = vec![Traffic::Shell];
    for s in sizes() {
        v.push(Traffic::Api { body_bytes: s });
        for m in MEDIA {
            v.push(Traffic::Bulk { bytes: s, media: m });
        }
    }
    v
}

const MAX_BODIES: [Option<u64>; 3] = [None, Some(1000), Some(200_000_000)];

#[test]
fn constants_match() {
    assert_eq!(policy::CLOUDFLARE_BODY_CAP, verus_copy::CLOUDFLARE_BODY_CAP);
    assert_eq!(policy::CLOUDFLARE_BULK_MAX, verus_copy::CLOUDFLARE_BULK_MAX);
}

#[test]
fn allowed_and_rank_agree() {
    let mut n = 0;
    for t in traffics() {
        for k in KINDS {
            assert_eq!(
                policy::rank(k, t).map(|r| r as u64),
                verus_copy::rank(&vk(k), &vt(t)),
                "{k:?} {t:?}"
            );
            for mb in MAX_BODIES {
                assert_eq!(
                    policy::allowed(k, t, mb),
                    verus_copy::allowed(&vk(k), &vt(t), mb),
                    "{k:?} {t:?} {mb:?}"
                );
                n += 1;
            }
        }
    }
    assert!(n > 500);
}

#[test]
fn select_agrees_on_every_list_up_to_three() {
    // One candidate shape = kind × max_body × (down | up 5 ms | up 9 ms).
    let mut shapes = Vec::new();
    for k in KINDS {
        for mb in [None, Some(1000)] {
            for h in [
                Health::Down,
                Health::Up { rtt_ms: 5 },
                Health::Up { rtt_ms: 9 },
            ] {
                shapes.push(Candidate {
                    kind: k,
                    max_body: mb,
                    health: h,
                });
            }
        }
    }
    let to_v = |c: &Candidate| verus_copy::Cand {
        kind: vk(c.kind),
        max_body: c.max_body,
        up: matches!(c.health, Health::Up { .. }),
        rtt: match c.health {
            Health::Up { rtt_ms } => rtt_ms,
            _ => 0,
        },
    };
    let traffic_sample = [
        Traffic::Shell,
        Traffic::Api { body_bytes: 0 },
        Traffic::Api { body_bytes: 5000 },
        Traffic::Api {
            body_bytes: policy::CLOUDFLARE_BODY_CAP + 1,
        },
        Traffic::Bulk {
            bytes: 10,
            media: Media::Video,
        },
        Traffic::Bulk {
            bytes: policy::CLOUDFLARE_BULK_MAX + 1,
            media: Media::Image,
        },
        Traffic::Bulk {
            bytes: 10,
            media: Media::Image,
        },
    ];
    let mut n = 0u64;
    for t in traffic_sample {
        let mut lists: Vec<Vec<Candidate>> = vec![vec![]];
        for _ in 0..3 {
            let mut next = Vec::new();
            for l in &lists {
                for s in &shapes {
                    let mut l2 = l.clone();
                    l2.push(*s);
                    next.push(l2);
                }
            }
            for l in &next {
                let v: Vec<verus_copy::Cand> = l.iter().map(to_v).collect();
                assert_eq!(
                    policy::select(l, t),
                    verus_copy::select(&v, &vt(t)),
                    "{t:?} {l:?}"
                );
                n += 1;
            }
            lists = next;
        }
        assert_eq!(policy::select(&[], t), None);
    }
    assert!(n > 100_000, "{n}");
}
