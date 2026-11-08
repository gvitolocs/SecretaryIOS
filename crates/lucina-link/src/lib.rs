//! Lucina Link: one connectivity layer for the owner's services.
//!
//! A client asks "how do I reach service X for this kind of traffic right now?" and gets the best working route
//! from the ladder LAN → tailnet → public → Cloudflare Access → Oracle SSH jump, under a size and media policy that
//! keeps large transfers and video off Cloudflare. See `docs/lucina-link.md`.

pub mod config;
pub mod link;
pub mod policy;
pub mod probe;
pub mod secrets;

pub use config::{Config, Route, Service, Target};
pub use link::{Chosen, Link, LinkError};
pub use policy::{Media, RouteKind, Traffic};
pub use probe::{probe_service, Outcome, ServiceReport};
pub use secrets::AccessToken;
