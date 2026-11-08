//! LucinaVoice: a push-to-talk speaker for Lucina on an ESP32 (fits an old
//! Echo shell). Hold the button, speak, let go: the audio goes to
//! `POST /v1/voice` on getsecretary.tech and Lucina's spoken answer plays.
//!
//! | module | |
//! |---|---|
//! | [`store`] | settings (Wi-Fi, device token) in the `nvs` partition |
//! | [`audio`] | I2S microphone (INMP441) and speaker amplifier (MAX98357A) |
//! | [`net`] | Wi-Fi, DNS, TLS to the server, HTTP requests |
//! | [`player`] | the speaker task: earcons and streamed answers |
//! | [`link`] | the persistent WebSocket to `/v1/voice/ws` |
//! | [`telemetry`] | device logs, specs, network and power, sent over the link |
//! | [`console`] | serial setup commands: `wifi`, `pair`, `status`, `forget` |
//!
//! Hardware-free logic (HTTP framing, settings records, PCM) lives in the
//! host-tested `lucinavoice-core` crate next to this one.

#![no_std]

extern crate alloc;

pub mod audio;
pub mod console;
pub mod link;
pub mod net;
pub mod player;
pub mod store;
pub mod telemetry;
