//! The owner's private reader plugin (Plugins → Reader): the HTTP around `nclient-core`, the
//! hard child-safety filter (`safety`), an image proxy with a disk cache, and per-owner
//! favorites and reading progress. Opt-in and locked on the devices; never in Lucina's
//! proactive messages, widgets, notifications or Spotlight (docs/nclient.md).

pub mod cache;
pub mod http;
pub mod safety;
pub mod saves;
pub mod service;
pub mod store;
pub mod tags;
pub mod upstream;
