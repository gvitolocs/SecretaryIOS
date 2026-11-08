// Reply-route kernel (secretary-server notifications: where a Lucina item goes —
// "a reply goes where he asked", docs/notifications.md, "Replies and the device
// he asked on").
//
// Production (crates/secretary-server/src/notify/reply_route.rs):
//   Kind / Origin / Route   <-> the enums of the same name
//   route_spec              <-> the table in reply_route.rs's doc comment
//   route                   <-> reply_route::route
//   grace_push              <-> reply_route::grace_push
//   origin_spec / origin    <-> Turn::origin, with the notifier's device flags folded
//              in: test = meta.test || is_test_source(source) || test_device(device),
//              device = a device is known, push_device = it holds a push
//              subscription, present = its app is in front, source = a source is
//              known, channel_delivers = delivers_itself(channel)
// Differential test: crates/secretary-server/tests/verus_reply_route.rs (a plain-Rust
// copy compared against production over every (kind, origin) pair and 20 000 random
// turns).
//
// Proven:
//   R1 test_never_pushes: test traffic is silent for every kind.
//   R2 asked_device_shows_it: a message or shopping reply to a client that gets no
//      pushes waits in grace; to a self-delivering channel or an in-front push device
//      it is silent.
//   R3 nothing_lost: a silent route arises only for test traffic, an agent's message,
//      or a message/shopping reply on a self-delivering channel or an in-front push
//      device — nothing else is ever dropped.
//   R4 reminders_and_confirms: reminders and device confirms push now, unless they are
//      test traffic.
//   R5 proactive_and_unknown_now: Lucina on her own (and legacy items) always push.
//   R6 away_iphone_now: a push device whose app is not in front always gets the push.
//   G1 grace_only_when_gone: after the grace period the push goes only when the asking
//      client is gone (no open stream or socket, no request since the reply).
//   O1 agent_wins / O2 test_beats_device: the origin precedence — agent beats
//      everything, test beats the device flags.
//
// No Stateright model: pure decisions over immutable turn marks. (Holding the
// grace reply and re-pushing it when the client is gone is the notifier's protocol,
// not covered here.)
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    Message,
    Shopping,
    Reminder,
    DeviceConfirm,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Origin {
    Proactive,
    Agent,
    Test,
    Channel,
    PushDevice { present: bool },
    OtherDevice,
    Unknown,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Route {
    Silent,
    Now,
    Grace,
}

/// The table in reply_route.rs's doc comment. (As in production, the
/// Reminder/DeviceConfirm row beats the Agent, Channel, PushDevice and
/// OtherDevice columns.)
pub open spec fn route_spec(k: Kind, o: Origin) -> Route {
    if o == Origin::Test {
        Route::Silent
    } else if o == Origin::Proactive || o == Origin::Unknown {
        Route::Now
    } else if k == Kind::Reminder || k == Kind::DeviceConfirm {
        Route::Now
    } else if o == Origin::Agent {
        if k == Kind::Message {
            Route::Silent
        } else {
            Route::Now
        }
    } else if o == Origin::Channel {
        Route::Silent
    } else if o == (Origin::PushDevice { present: true }) {
        Route::Silent
    } else if o == (Origin::PushDevice { present: false }) {
        Route::Now
    } else {
        // Origin::OtherDevice
        Route::Grace
    }
}

pub fn route(k: Kind, o: Origin) -> (r: Route)
    ensures
        r == route_spec(k, o),
{
    match o {
        Origin::Test => Route::Silent,
        Origin::Proactive | Origin::Unknown => Route::Now,
        _ if matches!(k, Kind::Reminder | Kind::DeviceConfirm) => Route::Now,
        Origin::Agent => {
            if matches!(k, Kind::Message) {
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

pub open spec fn grace_push_spec(connected: bool, active: bool) -> bool {
    !connected && !active
}

pub fn grace_push(connected: bool, active: bool) -> (b: bool)
    ensures
        b == grace_push_spec(connected, active),
{
    !connected && !active
}

pub open spec fn origin_spec(
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
        Origin::Agent
    } else if test {
        Origin::Test
    } else if device {
        if push_device {
            Origin::PushDevice { present }
        } else {
            Origin::OtherDevice
        }
    } else if !reply && !source {
        Origin::Proactive
    } else if channel_delivers {
        Origin::Channel
    } else {
        Origin::Unknown
    }
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
) -> (o: Origin)
    ensures
        o == origin_spec(agent, test, device, push_device, present, reply, source, channel_delivers),
{
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

// ---------------------------------------------------------------- lemmas

/// R1.
pub proof fn test_never_pushes(k: Kind)
    ensures
        route_spec(k, Origin::Test) == Route::Silent,
{
}

/// R2.
pub proof fn asked_device_shows_it(k: Kind)
    requires
        k == Kind::Message || k == Kind::Shopping,
    ensures
        route_spec(k, Origin::OtherDevice) == Route::Grace,
        route_spec(k, Origin::PushDevice { present: true }) == Route::Silent,
        route_spec(k, Origin::Channel) == Route::Silent,
{
}

/// R3.
pub proof fn nothing_lost(k: Kind, o: Origin)
    ensures
        route_spec(k, o) == Route::Silent ==>
            o == Origin::Test
            || (o == Origin::Agent && k == Kind::Message)
            || ((o == Origin::Channel || o == Origin::PushDevice { present: true })
                && (k == Kind::Message || k == Kind::Shopping)),
{
    match o {
        Origin::Test => {
        }
        Origin::Proactive | Origin::Unknown => {
        }
        Origin::Agent => {
        }
        Origin::Channel => {
        }
        Origin::PushDevice { present } => {
        }
        Origin::OtherDevice => {
        }
    }
}

/// R4.
pub proof fn reminders_and_confirms(k: Kind, o: Origin)
    requires
        (k == Kind::Reminder || k == Kind::DeviceConfirm) && o != Origin::Test,
    ensures
        route_spec(k, o) == Route::Now,
{
}

/// R5.
pub proof fn proactive_and_unknown_now(k: Kind, o: Origin)
    requires
        o == Origin::Proactive || o == Origin::Unknown,
    ensures
        route_spec(k, o) == Route::Now,
{
}

/// R6.
pub proof fn away_iphone_now(k: Kind)
    ensures
        route_spec(k, Origin::PushDevice { present: false }) == Route::Now,
{
}

/// G1.
pub proof fn grace_only_when_gone(connected: bool, active: bool)
    ensures
        grace_push_spec(connected, active) == (!connected && !active),
{
}

/// O1.
pub proof fn agent_wins(
    test: bool,
    device: bool,
    push_device: bool,
    present: bool,
    reply: bool,
    source: bool,
    channel_delivers: bool,
)
    ensures
        origin_spec(true, test, device, push_device, present, reply, source, channel_delivers)
            == Origin::Agent,
{
}

/// O2.
pub proof fn test_beats_device(
    test: bool,
    device: bool,
    push_device: bool,
    present: bool,
    reply: bool,
    source: bool,
    channel_delivers: bool,
)
    requires
        test,
    ensures
        origin_spec(false, test, device, push_device, present, reply, source, channel_delivers)
            == Origin::Test,
{
}

} // verus!

fn main() {}
