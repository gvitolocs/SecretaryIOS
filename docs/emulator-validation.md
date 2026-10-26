# Lucina Ink reader: desktop emulator validation

Status on 2026-10-24: the native reader exists as tested Rust code (`crates/ink-kindle-hal`, `crates/ink-epub`,
`crates/ink-render`, `apps/desktop-preview`). **Nothing has run on the Kindle.** The ARMv7 build links, but a
binary that links has not been shown to start, draw or read touches on the Paperwhite 3.

## The emulator

`ink-preview` renders a book through `ink_kindle_hal::sim::SimDisplay`. This simulated display keeps two images.
The framebuffer is what the app draws. The panel is what an update has made visible, and it changes only inside
regions sent as updates, as on the EPDC. Each update is logged with its region, waveform (GC16/GL16/DU/A2) and
whether it was a full flash. The refresh policy turns every 7th partial page turn into a full GC16 flash: six
partial refreshes are allowed between flashes (`--full-every`, default 6).

```
ink-preview <book.epub|txt|md> --out DIR [--font PATH] [--font-bold PATH] [--font-italic PATH]
            [--pages N] [--size 1072x1448] [--font-px 40] [--format png|pgm] [--full-every 6]
            [--script "next,next,prev,font+,next,font-,goto:3,full,sleep,wake,idle 700"]
            [--splash PNG] [--sleep-image PNG]... [--screen-bg white|black] [--screen-text]
            [--idle SECS] [--state FILE]
```

The JSON report on stdout contains:
- open and pagination time;
- render and update time for each page;
- peak RSS (`VmHWM`);
- the update log;
- for `--script`: whether the reading anchor survived each font change, and the stale-pixel count after each
  step. A stale pixel was drawn but never made visible by an update.

The exit status is 2 when a check fails. A deterministic test book comes from
`cargo run --release -p ink-preview --example gen_synthetic -- OUT.txt BYTES`. It is English-like prose from a
fixed vocabulary with a `CHAPTER N` heading every 60 paragraphs and lines wrapped at 70 columns, like a
Gutenberg text file.

## System screens: splash, sleep and resume

Lucina Ink draws its own system screens while it runs (`crates/ink-render/src/screens.rs` and
`session.rs`). This is app-level only: no firmware or system-partition changes.

- **Splash.** Shown at start-up, before the book is opened and paginated, and replaced by the first page.
- **Sleep screen.** Shown on a power event or after the idle timeout (default 10 min, `--idle SECS`).
  - The reading anchor is written first, with an atomic write (temp file, `fsync`, `rename`) behind the
    `AnchorStore` trait. Only then is the picture drawn.
  - With several sleep pictures, sleep `n` shows picture `n mod count`.
- **Resume.** On wake, the same page is redrawn, with the same anchor.
- **Updates.** Splash, sleep screen and resume are each one full-screen GC16 flash.
- **Pictures.**
  - The default for both the splash and the sleep screen is `/mnt/us/screensaver/lucina.png`.
  - PNG in any colour type is accepted and converted to luminance: RGB with Rec. 601 weights, alpha
    composited over the background.
  - A picture larger than the panel is scaled down with a box filter. A smaller one is scaled up only by an
    integer factor, with nearest neighbour. The aspect ratio is kept and the picture is centred on white
    (or black) margins.
- **Quantisation.** The result is quantised to the 16 levels `k·17` that GC16 can show, and nothing is
  dithered.
  - The EPDC uses the top 4 bits of each pixel. Rounding in software avoids the darker cast that
    truncation would give, and the 16 levels survive `v >> 4` unchanged.
  - The owner's picture has 250 gray levels from anti-aliasing. After quantisation the sleep frame equals
    `quantize16(source)` pixel for pixel, with 16 levels.
- **Fallback.** A missing, unreadable or corrupt picture never crashes the app. A generated "Lucina Ink"
  text screen is shown instead, and the report says why.
- **Idle timeout.** It is a deadline, not a poll. The main loop (`session::run`) blocks in
  `Input::next_event` for exactly the time left. While asleep it waits with no timeout.
  - The evdev reader now also sleeps until the caller's deadline or a possible long-press, instead of
    waking every 50 ms.

The emulator renders these screens with `ink-preview --sleep-image PNG --script "next,sleep,wake,next,idle 700"`.
- `splash.png` is the splash frame, and each sleep step also writes `sleep-frame.png`.
- The JSON report gives:
  - which picture was used, or why the fallback was shown;
  - the anchor saved before each sleep;
  - the wait the main loop would block for before an `idle` step (`main_loop_wait_ms`);
  - a check that each wake returned to the same page and anchor with one GC16 flash.

**Emulated:**
- the state machine (splash → reading ⇄ sleeping → resuming);
- power events, scripted in `ScriptedInput` or in the preview script;
- the idle deadline on an emulated clock;
- the order "save the anchor, then draw";
- picture loading, fitting and quantisation;
- the updates issued.

**Needs the device:**
- **Real power-button and suspend events.** On the Kindle they come from `powerd`. `kindle::PowerdEvents`
  is a documented stub that always fails: the LIPC event names and their timing have not been inspected,
  so none are used as working code.
- **The stock framework's own screensaver fighting ours.** When the device suspends, the Amazon framework
  draws its own screensaver (ads or the stock pictures) on the same panel. It is not known yet whether
  Lucina Ink's frame survives, gets overdrawn, or can be drawn after the framework's. Suppressing the stock
  screensaver without changing the system partition is still open.
- **Suspend timing.** Whether there is time between the power event and the actual suspend to write the
  anchor and finish a GC16 flash (hundreds of ms). The same goes for whether the EPDC finishes the update
  before the clocks stop, and what the panel shows after a long suspend.
- **Real idle detection beside the stock UI.** The stock UI has its own idle timer and may suspend first.

**Not changed by this.** The stock Kindle boot splash (bootloader and framework start-up) and the Amazon UI's
own lock screen and screensaver are not touched. Lucina Ink's splash, sleep and resume screens apply only
while Lucina Ink is running.

## Benchmarks (nezopt, x86_64)

| | |
|---|---|
| Host | nezopt: **AMD Ryzen 5 7600 6-Core Processor** (`/proc/cpuinfo`), 12 threads, 61 GiB RAM, Linux 6.8.0-142 |
| Toolchain | rustc 1.99.0, `--release` (opt-level 3, thin LTO, 1 codegen unit), single-threaded code |
| Input | `gen_synthetic` 2,000,014 bytes → 1,990,904 characters, 75 chapters (TXT importer) |
| Layout | Liberation Serif Regular 40 px, 1072×1448, default margins, justified |
| Runs | 3 × `--pages 200 --format pgm`, plus one `--script` run with 4 font changes |

| Measure | Result |
|---|---|
| Open (read and decode the TXT, build blocks) | 17–29 ms (29 ms on the first, cold run) |
| Paginate the whole book (1,822 pages) | 83–89 ms |
| Open + paginate | ≈ 100–120 ms |
| Page turn: render + diff + framebuffer copy + update, 199 turns | median 0.71–0.76 ms, mean 0.76–0.81 ms, max 1.6–3.0 ms |
| of which rasterising the page | median 0.46–0.48 ms, p95 0.68–0.80 ms |
| Font change (re-paginate 1.8–2.2 k pages + full refresh) | 84–92 ms |
| Peak RSS after opening and turning 200 pages | 33.5 MB (`VmHWM`; `/usr/bin/time -v`: 32.8 MB for 1 page) |
| Peak RSS after 4 font changes | 47 MB |
| Updates per page turn | 2 partial GL16 regions: the text block (≈ 928×1219) and the page number (≈ 11×18); 73% of the panel area on average |
| Full flashes | 1 at open, 1 per font change, and 1 promoted flash every 7th page turn (29 over 199 turns plus open) |

Time to write the PGM files is not included. All timings are CPU time on this host.

**Caveats.**
- E Ink timing and power are **not emulated**. On the device a GL16 page update takes hundreds of milliseconds and
  a GC16 flash longer still. That time depends on the waveform, the temperature and the update queue, and the
  emulator models none of it.
- The PW3's single Cortex-A9 core at 1 GHz (512 MB RAM) is **much slower** than a Zen 4 desktop core. Expect at
  least an order of magnitude slower everywhere: opening and paginating a 2 MB book will probably take seconds,
  not ~100 ms. The real numbers must be measured on the device.
- Pagination is eager: the whole book is paginated on open and on every font change. If on-device timings are
  too slow, paginate lazily (the current chapter first) or cache the pagination per style.

## What the emulator validates

- Parsing DRM-free EPUB 2/3, TXT and Markdown into the block model:
  - EPUB 3 nav and EPUB 2 NCX TOCs are read.
  - DRM is refused (`rights.xml`, or `encryption.xml` beyond font obfuscation).
  - ZIP caps hold: entry count, per-entry size, total expanded size, compression ratio.
  - Malformed ZIPs are refused.
- Layout and pagination:
  - deterministic (same input, same pages and the same pixels);
  - justification is hyphen-free and no line overflows;
  - overlong words are split;
  - headings are kept with the next line;
  - pages tile each chapter contiguously.
- Reading-position anchors survive every font size in `FONT_SIZES`. Font+ then font− returns to the same page;
  the reader keeps a sticky anchor, so nothing drifts.
- Dirty regions are minimal. Each update rectangle is tight (each edge touches a changed pixel), nothing outside
  the changed pixels is refreshed, an unchanged page issues no update, and no drawn pixel is left invisible.
- Refresh-policy accounting: one page turn counts as one partial refresh, and a full flash comes every N.
- Gesture recognition (tap, long-press, swipe, two-finger cancel) from synthetic touch sequences, including
  decoding of evdev multitouch protocol B and single-touch events.
- The mxcfb and framebuffer ABI layouts. Struct sizes and ioctl numbers are checked at compile time: 72-byte
  `mxcfb_update_data`, `MXCFB_SEND_UPDATE = 0x4048462e`, `MXCFB_WAIT_FOR_UPDATE_COMPLETE = 0xc008462f`. The
  backend builds for `armv7-unknown-linux-musleabihf`.

## What only hardware can validate

- That the static ARMv7 binary starts on firmware 5.16.2.1.1, and how to launch it from a KUAL extension while the
  stock UI keeps working.
- The real framebuffer geometry: `xres`/`yres`, `line_length` padding, `yoffset`, rotation, and whether it is
  8 bpp. `ink-kindle-probe` prints these read-only.
- Whether the PW3 kernel accepts this `mxcfb_update_data` layout and these waveform numbers, and how REAGL
  compares with GL16 for page turns.
- Real ghosting and how often a full flash is needed; update latency; collisions in the update queue.
- The touch device: which `/dev/input/eventN`, the axis ranges, the orientation (swap/mirror), and whether
  `EVIOCGRAB` is needed or safe beside the stock UI.
- Frontlight and battery sysfs paths (only the `Power` and `Frontlight` traits exist), suspend and screensaver
  interplay (see "System screens" above).
- Speed and memory on the Cortex-A9, glyph-cache growth, and font rendering quality on the 300 dpi panel.

## Cross-compilation

```
rustup target add armv7-unknown-linux-musleabihf
cargo build --release --target armv7-unknown-linux-musleabihf -p ink-preview -p ink-kindle-hal --features ink-kindle-hal/kindle
```

`.cargo/config.toml` sets `linker = "rust-lld"` for that target. The binary is linked statically by the
`rust-lld` that ships with rustup, using musl's self-contained CRT. It needs no cross gcc, no Zig, no sudo and
does not depend on the Kindle's old glibc.

`file` reports:
```
ink-preview:      ELF 32-bit LSB executable, ARM, EABI5 version 1 (SYSV), statically linked, stripped
ink-kindle-probe: ELF 32-bit LSB executable, ARM, EABI5 version 1 (SYSV), statically linked, stripped
```
`readelf -A`: `Tag_CPU_arch: v7`, `Tag_FP_arch: VFPv3-D16`, `Tag_ABI_VFP_args: VFP registers` (hard-float).
nezopt has no ARM user-mode emulation, so neither binary has been executed even under qemu.
