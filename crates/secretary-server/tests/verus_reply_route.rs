//! Differential test: `notify::reply_route::{route, grace_push, Turn::origin}`
//! against a plain-Rust copy of the Verus kernel `verification/verus/reply_route.rs`.
//! Every (kind, origin) pair of the table is compared, `grace_push` on all four
//! inputs, and 20 000 random turns (`Turn::origin` with the notifier's device
//! closures) against the kernel's `origin` with the flags folded in: a test
//! device or a test source makes the origin `Test`, just like production.

use secretary_server::notify::reply_route::{
    delivers_itself, grace_push, is_test_source, route, Kind, Origin, Route, Turn,
};

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        Message,
        Shopping,
        Reminder,
        DeviceConfirm,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Origin {
        Proactive,
        Agent,
        Test,
        Channel,
        PushDevice { present: bool },
        OtherDevice,
        Unknown,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Route {
        Silent,
        Now,
        Grace,
    }

    pub fn route(k: Kind, o: Origin) -> Route {
        match o {
            Origin::Test => Route::Silent,
            Origin::Proactive | Origin::Unknown => Route::Now,
            _ if k == Kind::Reminder || k == Kind::DeviceConfirm => Route::Now,
            Origin::Agent => {
                if k == Kind::Message {
                    Route::Silent
                } else {
                    Route::Now
                }
            }
            Origin::Channel => Route::Silent,
            Origin::PushDevice { present } => {
                if present {
                    Route::Silent
                } else {
                    Route::Now
                }
            }
            Origin::OtherDevice => Route::Grace,
        }
    }

    pub fn grace_push(connected: bool, active: bool) -> bool {
        !connected && !active
    }

    pub fn origin(
        agent: bool,
        test: bool,
        device: bool,
        push_device: bool,
        present: bool,
        reply: bool,
        source: bool,
        channel_delivers: bool,
    ) -> Origin {
        if agent {
            return Origin::Agent;
        }
        if test {
            return Origin::Test;
        }
        if device {
            return if push_device {
                Origin::PushDevice { present }
            } else {
                Origin::OtherDevice
            };
        }
        if !reply && !source {
            return Origin::Proactive;
        }
        if channel_delivers {
            return Origin::Channel;
        }
        Origin::Unknown
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn bool(&mut self) -> bool {
        self.below(2) == 0
    }
}

fn k_kind(k: Kind) -> kernel::Kind {
    match k {
        Kind::Message => kernel::Kind::Message,
        Kind::Shopping => kernel::Kind::Shopping,
        Kind::Reminder => kernel::Kind::Reminder,
        Kind::DeviceConfirm => kernel::Kind::DeviceConfirm,
    }
}

fn k_origin(o: Origin) -> kernel::Origin {
    match o {
        Origin::Proactive => kernel::Origin::Proactive,
        Origin::Agent => kernel::Origin::Agent,
        Origin::Test => kernel::Origin::Test,
        Origin::Channel => kernel::Origin::Channel,
        Origin::PushDevice { present } => kernel::Origin::PushDevice { present },
        Origin::OtherDevice => kernel::Origin::OtherDevice,
        Origin::Unknown => kernel::Origin::Unknown,
    }
}

fn k_route(r: Route) -> kernel::Route {
    match r {
        Route::Silent => kernel::Route::Silent,
        Route::Now => kernel::Route::Now,
        Route::Grace => kernel::Route::Grace,
    }
}

const KINDS: [Kind; 4] = [
    Kind::Message,
    Kind::Shopping,
    Kind::Reminder,
    Kind::DeviceConfirm,
];
const ORIGINS: [Origin; 8] = [
    Origin::Proactive,
    Origin::Agent,
    Origin::Test,
    Origin::Channel,
    Origin::PushDevice { present: true },
    Origin::PushDevice { present: false },
    Origin::OtherDevice,
    Origin::Unknown,
];

#[test]
fn the_whole_table_agrees_with_the_kernel() {
    for k in KINDS {
        for o in ORIGINS {
            assert_eq!(
                kernel::route(k_kind(k), k_origin(o)),
                k_route(route(k, o)),
                "{k:?} x {o:?}"
            );
        }
    }

    // All four inputs.
    for (connected, active) in [(false, false), (false, true), (true, false), (true, true)] {
        assert_eq!(
            kernel::grace_push(connected, active),
            grace_push(connected, active),
            "grace_push({connected}, {active})"
        );
    }
}

#[test]
fn random_turns_agree_with_the_kernel() {
    let mut rng = Rng(0x5eed_b01c);
    let devices: &[&str] = &["", "dev_iphone", "dev_dsi", "dev_probe"];
    let sources: &[&str] = &["", "fortuna-chat", "ios-chat", "probe", "test-x", "juno-chat"];
    let channels: &[&str] = &["web", "ios", "telegram", "alexa", "voice", "discord", "other"];
    let mut seen = [false; 8];
    for i in 0..20_000 {
        let device = match devices[rng.below(devices.len() as u64) as usize] {
            "" => None,
            d => Some(d.to_owned()),
        };
        let source = match sources[rng.below(sources.len() as u64) as usize] {
            "" => None,
            s => Some(s.to_owned()),
        };
        let channel = channels[rng.below(channels.len() as u64) as usize].to_owned();
        let reply = rng.bool();
        let test = rng.bool();
        let agent = rng.bool();
        let present = rng.bool();
        let turn = Turn {
            reply,
            device,
            source,
            test,
            agent,
            channel,
        };
        let got = turn.origin(
            |d| d == "dev_probe",
            |d| d == "dev_iphone",
            |_d| present,
        );
        let want = kernel::origin(
            agent,
            test
                || turn.source.as_deref().is_some_and(is_test_source)
                || turn.device.as_deref() == Some("dev_probe"),
            turn.device.is_some(),
            turn.device.as_deref() == Some("dev_iphone"),
            present,
            reply,
            turn.source.is_some(),
            delivers_itself(&turn.channel),
        );
        match want {
            kernel::Origin::Proactive => seen[0] = true,
            kernel::Origin::Agent => seen[1] = true,
            kernel::Origin::Test => seen[2] = true,
            kernel::Origin::Channel => seen[3] = true,
            kernel::Origin::PushDevice { present: true } => seen[4] = true,
            kernel::Origin::PushDevice { present: false } => seen[5] = true,
            kernel::Origin::OtherDevice => seen[6] = true,
            kernel::Origin::Unknown => seen[7] = true,
        }
        assert!(k_origin(got) == want, "turn #{i}: {turn:?}");
    }
    assert!(seen.iter().all(|&s| s), "not vacuous: every origin was reached: {seen:?}");
}
