//! voice-probe: measures voice latency against Secretary.
//!
//! - `ws`: the voice socket (docs/lucinavoice-protocol.md, sections 2 and 10)
//! - `share`: the old path, POST /v1/share + polling (docs/share.md)

pub mod ima;
pub mod pace;
pub mod resample;
pub mod share;
pub mod turn;
pub mod wav;
pub mod ws;
