//! Hardware abstraction for the Lucina Ink reader on a Kindle Paperwhite 3 (i.MX6 SoloLite EPDC).
//!
//! * [`Display`], [`Input`], [`Power`] and [`Frontlight`] are the traits the reader is written against.
//! * [`sim`] is an in-memory E Ink emulator: an 8-bit framebuffer plus a separate "panel" image that only
//!   changes when an update is issued, an update log and the ghosting/full-refresh policy.
//! * `kindle` (feature `kindle`, Linux only) talks to `/dev/fb0` with the mxcfb ioctls and reads touch from
//!   evdev. It is compiled and type-checked but has **not** been run on hardware.
//!
//! Gesture recognition ([`input::GestureRecognizer`]) and the evdev multitouch decoder ([`evdev`]) are pure
//! logic, so they are tested on the desktop with synthetic event sequences.
pub mod display;
pub mod evdev;
pub mod geom;
pub mod input;
pub mod power;
pub mod sim;

#[cfg(all(feature = "kindle", target_os = "linux"))]
pub mod kindle;

pub use display::{Display, FrameMut, RefreshPolicy, UpdateRecord, WaveformMode};
pub use geom::{GrayImage, Point, Rect, Size};
pub use input::{
    Direction, Gesture, GestureConfig, GestureRecognizer, Input, InputEvent, PowerEvent,
    TouchEvent, Wait,
};
pub use power::{Frontlight, Power};

/// Panel of the Kindle Paperwhite 3 (KOReader device table / community docs; not yet measured on the device).
pub const PW3_SIZE: Size = Size { w: 1072, h: 1448 };
pub const PW3_DPI: u32 = 300;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("region {0:?} is outside the {1:?} panel")]
    OutOfBounds(Rect, Size),
    #[error("{0}")]
    Device(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
