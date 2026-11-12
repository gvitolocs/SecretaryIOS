//! Lucina's parcel tracker. The owner gives a tracking number; Lucina follows the parcel through
//! the local Universal Parcel Scraper ([`client`]), checks it once a day, every hour once it is
//! close to him ([`policy`]), and tells him about changes ([`schedule`]) in plain English
//! ([`text`]). The tracked list lives in one JSON file ([`store`]).
//!
//! Everything but [`client`] and [`store`] is pure: time is always an argument.

pub mod client;
pub mod policy;
pub mod schedule;
pub mod service;
pub mod store;
pub mod text;
pub mod view;

pub use client::{Detection, Scraper, Track, TrackError};
pub use policy::{Cadence, Event, Home, Stage, Status};
pub use schedule::{tick, MessageKind, OwnerMessage, ScheduleCfg};
pub use service::{Added, Service};
pub use store::{valid_id, AddResult, Parcel, ParcelState, Store};
pub use view::{next_check, ordered, view};
