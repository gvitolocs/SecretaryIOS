//! Lucina Prompter (docs/prompter.md): talking points on screen, crossed out
//! while the owner says them.
//!
//! This crate is the decision half, shared by lucina-rs (which holds the Jev
//! key) and lucina-memo (which streams the speech to text):
//!
//! - [`matcher`]: a fast local word matcher (English, Italian, Danish). It
//!   catches the obvious hits on its own and is the whole judge when Jev is out.
//! - [`judge`]: one call per transcript window. Jev answers a yes/no question
//!   per nearby point; the matcher fills in, and any Jev failure falls back to
//!   the matcher (fail open), never to "nothing happened".
//!
//! Only transcript text reaches Jev, and only through the keyed route.
pub mod judge;
pub mod matcher;

pub use judge::{
    judge, Candidate, Decider, JevUse, JudgeRequest, JudgeResponse, Kind, Source, Verdict, DONE_P,
    STARTED_P,
};
pub use matcher::score;
