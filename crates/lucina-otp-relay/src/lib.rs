//! Lucina's OTP relay (`docs/lucina-android.md`, "Relay protocol").
//!
//! Runs on p1 as `lucina-otp-relay`, listening on [`http::LISTEN`] behind
//! Caddy at `https://getsecretary.tech/v1/phone/otp/*`. The phone posts each
//! code it sees to `/incoming`; the owner's Lucina takes it once from `/otp`.
//! Codes live only in memory and for at most two minutes.
//!
//! - [`store`]: the pure state machine (window, pending codes, seen event
//!   ids) with an injected clock; `tests/model.rs` model-checks it with
//!   Stateright.
//! - [`http`]: tokens, routes, the `paused` file and the `tiny_http` loop.
//!
//! Never log codes, tokens or senders.

pub mod http;
pub mod store;

pub use http::{Config, Relay};
pub use store::{Limits, Refusal, Store};
