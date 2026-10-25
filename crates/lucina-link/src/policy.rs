//! The route policy, with no IO: which kinds of route may carry which traffic, and which healthy route wins.
//!
//! `verification/verus/link_policy.rs` proves the same rules (L1-L6); `tests/verus_policy.rs` checks that this
//! module and the Verus executable functions agree on an exhaustive grid.

use serde::{Deserialize, Serialize};

/// The rungs of the route ladder, best first for most traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    /// Direct on the home LAN.
    Lan,
    /// Tailscale (direct WireGuard when both ends can see each other).
    Tailnet,
    /// The service's own public HTTPS endpoint (getsecretary.tech on p1: DNS-only, no body cap).
    Public,
    /// A Cloudflare Tunnel hostname behind Access (service token). Light traffic and shells only.
    Cloudflare,
    /// SSH ProxyJump through Oracle (the reverse tunnel). Last resort for shells.
    SshJump,
}

impl RouteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteKind::Lan => "lan",
            RouteKind::Tailnet => "tailnet",
            RouteKind::Public => "public",
            RouteKind::Cloudflare => "cloudflare",
            RouteKind::SshJump => "ssh_jump",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Media {
    None,
    Image,
    Audio,
    Video,
    Other,
}

/// What the caller is about to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "class")]
pub enum Traffic {
    /// A request/response API call; `body_bytes` is the request body.
    Api { body_bytes: u64 },
    /// An interactive shell (SSH).
    Shell,
    /// A transfer: uploads, downloads, media sync.
    Bulk { bytes: u64, media: Media },
}

impl Traffic {
    /// The body the route has to carry (0 for a shell).
    pub fn body(self) -> u64 {
        match self {
            Traffic::Api { body_bytes } => body_bytes,
            Traffic::Shell => 0,
            Traffic::Bulk { bytes, .. } => bytes,
        }
    }
}

/// Cloudflare free plan: requests with a larger body are refused.
pub const CLOUDFLARE_BODY_CAP: u64 = 100_000_000;
/// Our own margin for non-video transfers through Cloudflare.
pub const CLOUDFLARE_BULK_MAX: u64 = 64 * 1024 * 1024;

const API_ORDER: &[RouteKind] = &[
    RouteKind::Lan,
    RouteKind::Tailnet,
    RouteKind::Public,
    RouteKind::Cloudflare,
];
// Cloudflare before the Oracle jump: the reverse tunnel drops connections.
const SHELL_ORDER: &[RouteKind] = &[
    RouteKind::Lan,
    RouteKind::Tailnet,
    RouteKind::Cloudflare,
    RouteKind::SshJump,
];
const BULK_ORDER: &[RouteKind] = API_ORDER;

/// Route kinds that may carry this traffic, best first.
pub fn preference(traffic: Traffic) -> &'static [RouteKind] {
    match traffic {
        Traffic::Api { .. } => API_ORDER,
        Traffic::Shell => SHELL_ORDER,
        Traffic::Bulk { .. } => BULK_ORDER,
    }
}

/// Position in the preference list (0 = best), or `None` when the kind never carries this traffic.
pub fn rank(kind: RouteKind, traffic: Traffic) -> Option<usize> {
    preference(traffic).iter().position(|k| *k == kind)
}

/// Why a route may not carry this traffic, or `None` when it may.
pub fn denial(
    kind: RouteKind,
    traffic: Traffic,
    route_max_body: Option<u64>,
) -> Option<&'static str> {
    if rank(kind, traffic).is_none() {
        return Some(match kind {
            RouteKind::SshJump => "ssh_jump carries shells only",
            _ => "this kind of route does not carry this traffic",
        });
    }
    if let Some(max) = route_max_body {
        if traffic.body() > max {
            return Some("body larger than the route's max_body_bytes");
        }
    }
    if kind == RouteKind::Cloudflare {
        match traffic {
            Traffic::Bulk {
                media: Media::Video,
                ..
            } => return Some("video never goes through Cloudflare"),
            _ if traffic.body() > CLOUDFLARE_BODY_CAP => {
                return Some("body over Cloudflare's 100 MB cap")
            }
            Traffic::Bulk { bytes, .. } if bytes > CLOUDFLARE_BULK_MAX => {
                return Some("bulk over 64 MiB stays off Cloudflare")
            }
            _ => {}
        }
    }
    None
}

pub fn allowed(kind: RouteKind, traffic: Traffic, route_max_body: Option<u64>) -> bool {
    denial(kind, traffic, route_max_body).is_none()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Up { rtt_ms: u32 },
    Down,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub kind: RouteKind,
    pub max_body: Option<u64>,
    pub health: Health,
}

/// The best candidate that is allowed and up: best rank first, then lowest rtt, then the earlier index.
pub fn select(candidates: &[Candidate], traffic: Traffic) -> Option<usize> {
    let mut best: Option<(usize, usize, u32)> = None;
    for (i, c) in candidates.iter().enumerate() {
        let Health::Up { rtt_ms } = c.health else {
            continue;
        };
        if !allowed(c.kind, traffic, c.max_body) {
            continue;
        }
        let Some(r) = rank(c.kind, traffic) else {
            continue;
        };
        let better = match best {
            None => true,
            Some((_, br, brtt)) => r < br || (r == br && rtt_ms < brtt),
        };
        if better {
            best = Some((i, r, rtt_ms));
        }
    }
    best.map(|(i, _, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(kind: RouteKind, rtt_ms: u32) -> Candidate {
        Candidate {
            kind,
            max_body: None,
            health: Health::Up { rtt_ms },
        }
    }
    fn down(kind: RouteKind) -> Candidate {
        Candidate {
            kind,
            max_body: None,
            health: Health::Down,
        }
    }
    const MB: u64 = 1_000_000;

    #[test]
    fn video_never_via_cloudflare() {
        let t = Traffic::Bulk {
            bytes: 10 * MB,
            media: Media::Video,
        };
        assert!(!allowed(RouteKind::Cloudflare, t, None));
        assert_eq!(select(&[up(RouteKind::Cloudflare, 5)], t), None);
        assert!(allowed(RouteKind::Tailnet, t, None));
    }

    #[test]
    fn api_over_cap_not_via_cloudflare() {
        let t = Traffic::Api {
            body_bytes: 150 * MB,
        };
        assert!(!allowed(RouteKind::Cloudflare, t, None));
        assert!(allowed(RouteKind::Public, t, None));
        let at_cap = Traffic::Api {
            body_bytes: CLOUDFLARE_BODY_CAP,
        };
        assert!(allowed(RouteKind::Cloudflare, at_cap, None));
    }

    #[test]
    fn bulk_through_cloudflare_only_under_margin() {
        let small = Traffic::Bulk {
            bytes: CLOUDFLARE_BULK_MAX,
            media: Media::Image,
        };
        let big = Traffic::Bulk {
            bytes: CLOUDFLARE_BULK_MAX + 1,
            media: Media::Image,
        };
        assert!(allowed(RouteKind::Cloudflare, small, None));
        assert!(!allowed(RouteKind::Cloudflare, big, None));
    }

    #[test]
    fn shell_picks_cloudflare_when_lan_and_tailnet_are_down() {
        let c = [
            down(RouteKind::Lan),
            down(RouteKind::Tailnet),
            up(RouteKind::SshJump, 20),
            up(RouteKind::Cloudflare, 80),
        ];
        assert_eq!(select(&c, Traffic::Shell), Some(3));
    }

    #[test]
    fn lan_wins_when_up() {
        let c = [
            up(RouteKind::Cloudflare, 1),
            up(RouteKind::Tailnet, 2),
            up(RouteKind::Lan, 30),
        ];
        assert_eq!(select(&c, Traffic::Shell), Some(2));
        assert_eq!(select(&c, Traffic::Api { body_bytes: 0 }), Some(2));
    }

    #[test]
    fn ssh_jump_only_for_shell() {
        assert!(allowed(RouteKind::SshJump, Traffic::Shell, None));
        assert!(!allowed(
            RouteKind::SshJump,
            Traffic::Api { body_bytes: 0 },
            None
        ));
        assert!(!allowed(
            RouteKind::SshJump,
            Traffic::Bulk {
                bytes: 1,
                media: Media::None
            },
            None
        ));
        assert!(!allowed(RouteKind::Public, Traffic::Shell, None));
    }

    #[test]
    fn route_max_body_is_respected() {
        let t = Traffic::Api { body_bytes: 2000 };
        assert!(!allowed(RouteKind::Public, t, Some(1999)));
        assert!(allowed(RouteKind::Public, t, Some(2000)));
    }

    #[test]
    fn rtt_breaks_ties_within_a_kind_then_earlier_index() {
        let c = [
            up(RouteKind::Tailnet, 40),
            up(RouteKind::Tailnet, 10),
            up(RouteKind::Tailnet, 10),
        ];
        assert_eq!(select(&c, Traffic::Shell), Some(1));
    }

    #[test]
    fn nothing_usable_is_none() {
        let c = [down(RouteKind::Lan), up(RouteKind::Public, 3)];
        assert_eq!(select(&c, Traffic::Shell), None);
        assert_eq!(select(&[], Traffic::Shell), None);
    }
}
