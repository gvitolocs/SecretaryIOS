//! `lucina-otp-relay`: serves the OTP relay on 127.0.0.1:18866.
//!
//! Environment: `LUCINA_OTP_STATE_DIR` (default `/var/lib/lucina-otp`, holds
//! `owner.token`, `phone.token` and `paused`) and `LUCINA_OTP_CONTINUOUS`.

use std::process::ExitCode;

use lucina_otp_relay::http::{self, Config};

fn main() -> ExitCode {
    match http::run(&Config::from_env(), http::LISTEN) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lucina-otp-relay: {error}");
            ExitCode::FAILURE
        }
    }
}
