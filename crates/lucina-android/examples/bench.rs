//! Parse-time benchmark for a typical OTP SMS (synthetic).
//!
//! `cargo run --release -p lucina-android --example bench`
//!
//! Prints the first call (which compiles the regexes) and the steady state,
//! for `parse_otp` alone and for `Bridge::on_sms` with a non-OTP SMS (the
//! path every ordinary message takes: no disk, no network).

use std::hint::black_box;
use std::time::Instant;

use lucina_android::Bridge;
use lucina_phone_core::otp::parse_otp;

const SENDER: &str = "Bank";
const OTP_SMS: &str =
    "Your verification code is 731942. It expires in 5 minutes. Do not share it with anyone.";
const PLAIN_SMS: &str =
    "Hi! Your parcel 731942 left the depot and will arrive tomorrow between 12:30 and 14:30.";
const ROUNDS: u32 = 20_000;

fn steady(f: impl Fn()) -> f64 {
    let start = Instant::now();
    for _ in 0..ROUNDS {
        f();
    }
    start.elapsed().as_secs_f64() * 1e6 / f64::from(ROUNDS)
}

fn main() {
    let start = Instant::now();
    let code = parse_otp(black_box(SENDER), black_box(OTP_SMS));
    let first = start.elapsed();
    assert!(code.is_some());
    println!(
        "parse_otp first call (incl. regex init): {:.3} ms",
        first.as_secs_f64() * 1e3
    );
    let otp = steady(|| {
        black_box(parse_otp(black_box(SENDER), black_box(OTP_SMS)));
    });
    let plain = steady(|| {
        black_box(parse_otp(black_box(SENDER), black_box(PLAIN_SMS)));
    });
    println!("parse_otp steady, OTP SMS: {otp:.2} µs");
    println!("parse_otp steady, plain SMS: {plain:.2} µs");

    let dir = std::env::temp_dir().join(format!("lucina-android-bench-{}", std::process::id()));
    let start = Instant::now();
    let mut bridge = Bridge::open(&dir, &[7; 32], None).expect("open");
    println!(
        "Bridge::open (new dir): {:.3} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    let start = Instant::now();
    for _ in 0..ROUNDS {
        black_box(bridge.on_sms(SENDER, black_box(PLAIN_SMS), 0));
    }
    let on_sms = start.elapsed().as_secs_f64() * 1e6 / f64::from(ROUNDS);
    println!("Bridge::on_sms steady, plain SMS: {on_sms:.2} µs");
    let _ = std::fs::remove_dir_all(&dir);
}
