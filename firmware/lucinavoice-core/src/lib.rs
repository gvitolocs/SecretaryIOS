//! LucinaVoice logic that needs no hardware, so it is tested on the host
//! (`cargo test` in this directory) and used by the ESP32 firmware.
//!
//! | module | |
//! |---|---|
//! | [`settings`] | the Wi-Fi + device-token record kept in flash |
//! | [`http`] | HTTP/1.1 request heads, response-head parsing, chunked framing |
//! | [`json`] | string fields out of the server's small JSON replies |
//! | [`pcm`] | INMP441 words to 16-bit PCM, mono to stereo frames |
//! | [`ws`] | WebSocket client: upgrade, masked frames, streaming frame parser |

#![no_std]

pub mod http;
pub mod json;
pub mod pcm;
pub mod settings;
pub mod ws;

/// The only origin the device talks to.
pub const HOST: &str = "getsecretary.tech";
/// Microphone sample rate sent to `POST /v1/voice`.
pub const MIC_RATE: u32 = 16_000;
/// Name the device asks to be paired as.
pub const DEVICE_NAME: &str = "LucinaVoice";
