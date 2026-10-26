//! Read-only hardware probe for the future on-device validation (Phase 12). Not run yet.
//!
//! `ink-kindle-probe [--fb /dev/fb0] [--input /dev/input/eventN] [--gestures SECONDS]`
//!
//! Prints the framebuffer geometry (FBIOGET_*SCREENINFO) and the touch device's name and axis ranges.
//! With `--gestures` it reads touches (without grabbing the device, so the stock UI keeps working) and
//! prints recognised gestures. It never maps or draws to the framebuffer.
#[cfg(target_os = "linux")]
fn main() {
    use ink_kindle_hal::kindle::{fb_info, input_device_info, EvdevInput};
    use ink_kindle_hal::{GestureConfig, Input, PW3_SIZE};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let fb = opt("--fb").unwrap_or_else(|| "/dev/fb0".into());
    match fb_info(&fb) {
        Ok(i) => println!(
            "fb {fb}: id={:?} {}x{} virtual {}x{} offset {},{} bpp={} grayscale={} rotate={} line_length={} smem_len={}",
            i.id,
            i.var.xres,
            i.var.yres,
            i.var.xres_virtual,
            i.var.yres_virtual,
            i.var.xoffset,
            i.var.yoffset,
            i.var.bits_per_pixel,
            i.var.grayscale,
            i.var.rotate,
            i.fix.line_length,
            i.fix.smem_len
        ),
        Err(e) => println!("fb {fb}: {e}"),
    }
    let inputs: Vec<String> = match opt("--input") {
        Some(p) => vec![p],
        None => (0..8).map(|n| format!("/dev/input/event{n}")).collect(),
    };
    for p in &inputs {
        match input_device_info(p) {
            Ok((name, axes)) => {
                println!("input {p}: {name:?}");
                for (abs, a) in axes {
                    println!(
                        "  abs 0x{abs:02x}: min={} max={} res={}",
                        a.minimum, a.maximum, a.resolution
                    );
                }
            }
            Err(e) => println!("input {p}: {e}"),
        }
    }
    if let (Some(secs), Some(p)) = (opt("--gestures"), opt("--input")) {
        let secs: u32 = secs.parse().unwrap_or(10);
        let mut input = match EvdevInput::open(&p, PW3_SIZE, GestureConfig::default()) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("{p}: {e}");
                std::process::exit(1);
            }
        };
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs.into());
        while std::time::Instant::now() < end {
            match input.next_gesture(Some(250)) {
                Ok(Some(g)) => println!("{g:?}"),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("{e}");
                    break;
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ink-kindle-probe only runs on Linux (the Kindle)");
}
