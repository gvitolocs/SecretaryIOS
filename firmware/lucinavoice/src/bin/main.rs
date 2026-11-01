#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! LucinaVoice firmware entry: wiring, then the main loop.
//!
//! | pin | use |
//! |---|---|
//! | GPIO0 (BOOT) or GPIO4 to GND | push to talk |
//! | GPIO2 | status LED (on: listening, blinking: pairing or waiting) |
//! | I2S pins | see `lucinavoice::audio` |
//!
//! A turn: press → TLS connect → beep → stream the mic (chunked s16le
//! 16 kHz) while held, max 30 s → release → read the answer head (logs
//! transcript and reply) → play the s16le 24 kHz body as it arrives.

use alloc::vec;
use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2s::master::{Channels, DataFormat, I2s, TdmConfig};
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::Uart;
use esp_hal::{dma_rx_stream_buffer, dma_tx_stream_buffer};
use esp_println::println;
use log::{error, info, warn};
use lucinavoice::audio::{Mic, SPEAKER_RATE, Speaker};
use lucinavoice::console::{self, COMMANDS, Command};
use lucinavoice::net::{self, Client, NetError};
use lucinavoice::store::Store;
use lucinavoice_core::http::{self, Body, SliceWriter};
use lucinavoice_core::settings::Settings;
use lucinavoice_core::{DEVICE_NAME, MIC_RATE, json};
use mbedtls_rs::Tls;
use static_cell::StaticCell;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Longest recording per turn.
const MAX_TURN: Duration = Duration::from_secs(30);

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static CELL: StaticCell<$t> = StaticCell::new();
        CELL.uninit().write($val)
    }};
}

struct Device {
    store: Store,
    settings: Settings,
    client: Option<Client>,
    speaker: Speaker,
    mic: Mic,
    led: Output<'static>,
    ptt: Input<'static>,
    ptt2: Input<'static>,
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 98768);
    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);
    info!("LucinaVoice {} starting", env!("CARGO_PKG_VERSION"));

    // Entropy for TLS: the hardware RNG, seeded from the ADC noise source.
    let _trng_source = mk_static!(
        TrngSource<'static>,
        TrngSource::new(peripherals.RNG, peripherals.ADC1)
    );
    let trng = mk_static!(Trng, Trng::try_new().expect("TRNG source enabled"));
    let seed = (trng.random() as u64) << 32 | trng.random() as u64;
    let tls = mk_static!(
        Tls<'static>,
        Tls::new(mk_static!(Trng, Trng::try_new().expect("TRNG"))).expect("one Tls")
    );

    let mut store =
        Store::new(esp_storage::FlashStorage::new(peripherals.FLASH)).expect("nvs partition");
    let settings = store.load();

    // Serial console on UART0 (shared with the log output).
    let uart = Uart::new(peripherals.UART0, Default::default())
        .expect("uart0")
        .with_rx(peripherals.GPIO3)
        .with_tx(peripherals.GPIO1)
        .into_async();
    let (uart_rx, _uart_tx) = uart.split();
    core::mem::drop(_uart_tx);
    spawner.spawn(console::console_task(uart_rx).expect("console task"));

    // Speaker: I2S0 out, 24 kHz stereo 16-bit.
    let i2s0 = I2s::new(
        peripherals.I2S0,
        peripherals.DMA_I2S0,
        TdmConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(SPEAKER_RATE))
            .with_data_format(DataFormat::Data16Channel16)
            .with_channels(Channels::STEREO),
    )
    .expect("i2s0")
    .into_async();
    let i2s_tx = i2s0
        .i2s_tx
        .with_bclk(peripherals.GPIO26)
        .with_ws(peripherals.GPIO25)
        .with_dout(peripherals.GPIO22)
        .build();
    let speaker = Speaker::new(i2s_tx, dma_tx_stream_buffer!(12 * 1024, 1024));

    // Microphone: I2S1 in, 16 kHz stereo 32-bit words.
    let i2s1 = I2s::new(
        peripherals.I2S1,
        peripherals.DMA_I2S1,
        TdmConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(MIC_RATE))
            .with_data_format(DataFormat::Data32Channel32)
            .with_channels(Channels::STEREO),
    )
    .expect("i2s1")
    .into_async();
    let i2s_rx = i2s1
        .i2s_rx
        .with_bclk(peripherals.GPIO32)
        .with_ws(peripherals.GPIO33)
        .with_din(peripherals.GPIO34)
        .build();
    let mic = Mic::new(i2s_rx, dma_rx_stream_buffer!(16 * 1024, 1024));

    let led = Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default());
    let ptt = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );
    let ptt2 = Input::new(
        peripherals.GPIO4,
        InputConfig::default().with_pull(Pull::Up),
    );

    // Wi-Fi, only once it is set up.
    let mut client = None;
    match net::station_config(&settings) {
        Some(config) if settings.has_wifi() => {
            let mut controller =
                esp_radio::wifi::WifiController::new(peripherals.WIFI, Default::default())
                    .expect("wifi controller");
            controller.set_config(&config).expect("station config");
            let (stack, runner) = embassy_net::new(
                esp_radio::wifi::Interface::station(),
                embassy_net::Config::dhcpv4(Default::default()),
                mk_static!(
                    embassy_net::StackResources<4>,
                    embassy_net::StackResources::new()
                ),
                seed,
            );
            spawner.spawn(net::wifi_task(controller).expect("wifi task"));
            spawner.spawn(net::net_task(runner).expect("net task"));
            info!("wifi: joining the saved network");
            client = Some(Client::new(stack, tls.reference()));
        }
        _ => println!("\r\nNo Wi-Fi set up yet: type `wifi`, then `pair`."),
    }
    if settings.has_wifi() && !settings.is_paired() {
        println!("\r\nNot paired with Lucina yet: type `pair` once Wi-Fi is up.");
    }

    let mut dev = Device {
        store,
        settings,
        client,
        speaker,
        mic,
        led,
        ptt,
        ptt2,
    };
    dev.speaker.beep(660, 60).await;
    info!("ready; free heap {} bytes", esp_alloc::HEAP.free());

    loop {
        match select(press(&mut dev.ptt, &mut dev.ptt2), COMMANDS.receive()).await {
            Either::First(()) => dev.turn(None).await,
            Either::Second(cmd) => dev.command(cmd).await,
        }
    }
}

/// Waits for either push-to-talk button (20 ms debounce).
async fn press(a: &mut Input<'static>, b: &mut Input<'static>) {
    loop {
        select(a.wait_for_low(), b.wait_for_low()).await;
        Timer::after(Duration::from_millis(20)).await;
        if a.is_low() || b.is_low() {
            return;
        }
    }
}

impl Device {
    fn held(&self) -> bool {
        self.ptt.is_low() || self.ptt2.is_low()
    }

    async fn wait_release(&mut self) {
        while self.held() {
            Timer::after(Duration::from_millis(20)).await;
        }
    }

    async fn error_tone(&mut self) {
        self.speaker.beep(330, 120).await;
        Timer::after(Duration::from_millis(60)).await;
        self.speaker.beep(220, 180).await;
    }

    /// One push-to-talk turn; failures are logged and signalled with a tone.
    /// One turn: while a button is held, or for `fixed` (the `talk` command).
    async fn turn(&mut self, fixed: Option<Duration>) {
        let ready = self.client.as_ref().is_some_and(Client::is_up) && self.settings.is_paired();
        if !ready {
            warn!(
                "turn: not ready (wifi up: {}, paired: {})",
                self.client.as_ref().is_some_and(Client::is_up),
                self.settings.is_paired()
            );
            self.error_tone().await;
            self.wait_release().await;
            return;
        }
        let started = Instant::now();
        match self.voice_turn(fixed).await {
            Ok(()) => info!("turn: done in {} ms", started.elapsed().as_millis()),
            Err(e) => {
                error!("turn: {e:?}");
                self.led.set_low();
                self.error_tone().await;
            }
        }
        self.led.set_low();
        self.wait_release().await;
    }

    async fn voice_turn(&mut self, fixed: Option<Duration>) -> Result<(), NetError> {
        let client = self.client.clone().ok_or(NetError::Connect)?;
        let mut rx = vec![0u8; 16 * 1024];
        let mut tx = vec![0u8; 8 * 1024];
        let mut session = client.connect(&mut rx, &mut tx).await?;

        let mut head = [0u8; 512];
        let ct = "audio/x-s16le;rate=16000";
        let head = http::request_head(
            &mut head,
            "POST",
            "/v1/voice",
            Some(self.settings.token.as_str()),
            Some(ct),
            Body::Chunked,
        )
        .ok_or(NetError::TooLong)?;
        session.write_all(head).await?;

        // Listening.
        self.speaker.beep(880, 70).await;
        self.led.set_high();
        let mut sent = 0usize;
        let mut overrun = false;
        {
            let Some(mut rec) = self.mic.start() else {
                return Err(NetError::Http);
            };
            let until = Instant::now() + fixed.unwrap_or(MAX_TURN);
            let mut pcm = [0u8; 1024];
            let mut size = [0u8; 18];
            while (fixed.is_some() || self.ptt.is_low() || self.ptt2.is_low())
                && Instant::now() < until
            {
                let n = match rec.read(&mut pcm).await {
                    Ok(n) => n,
                    Err(_) => {
                        overrun = true;
                        break;
                    }
                };
                if n == 0 {
                    continue;
                }
                session
                    .write_all(http::chunk_size_line(n, &mut size))
                    .await?;
                session.write_all(&pcm[..n]).await?;
                session.write_all(b"\r\n").await?;
                sent += n;
            }
        }
        session.write_all(http::LAST_CHUNK).await?;
        session.flush().await?;
        self.led.set_low();
        info!(
            "turn: sent {} ms of audio{}",
            sent as u64 * 1000 / (MIC_RATE as u64 * 2),
            if overrun {
                " (mic overrun, cut short)"
            } else {
                ""
            }
        );

        // The answer.
        let mut buf = vec![0u8; 2048];
        let (head, mut body) = net::read_head(&mut session, &mut buf).await?;
        info!(
            "turn: HTTP {} heard {:?}",
            head.status,
            head.transcript.as_str()
        );
        if head.status != 200 {
            let msg = net::read_small_body(&mut session, &mut body, &mut buf, 300).await;
            warn!(
                "turn: server said {}",
                core::str::from_utf8(&msg).unwrap_or("?")
            );
            return Err(NetError::Http);
        }
        info!("turn: Lucina says {:?}", head.reply.as_str());
        let mut playback = self.speaker.start();
        let mut carry: Option<u8> = None;
        let mut played = 0usize;
        while let Some(chunk) = body.next(&mut session, &mut buf).await? {
            let mut chunk = chunk;
            if let Some(lo) = carry.take()
                && let Some((&hi, rest)) = chunk.split_first()
            {
                playback.push(&[lo, hi]).await;
                chunk = rest;
                played += 2;
            }
            let even = chunk.len() & !1;
            playback.push(&chunk[..even]).await;
            played += even;
            if even < chunk.len() {
                carry = Some(chunk[even]);
            }
        }
        let restarts = playback.restarts;
        playback.finish().await;
        let _ = session.close().await;
        info!(
            "turn: played {} ms, {} speaker restarts",
            played as u64 * 1000 / (SPEAKER_RATE as u64 * 2),
            restarts
        );
        Ok(())
    }

    async fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Wifi { ssid, password } => {
                self.settings.ssid = ssid;
                self.settings.password = password;
                match self.store.save(&self.settings) {
                    Ok(()) => {
                        println!("Saved. Restarting to join the network...");
                        Timer::after(Duration::from_millis(300)).await;
                        esp_hal::system::software_reset();
                    }
                    Err(_) => println!("Could not save the settings to flash."),
                }
            }
            Command::Pair => self.pair().await,
            Command::Talk => self.turn(Some(Duration::from_secs(3))).await,
            Command::Status => self.status(),
            Command::Forget => {
                let _ = self.store.erase();
                println!("Wi-Fi and pairing erased. Restarting...");
                Timer::after(Duration::from_millis(300)).await;
                esp_hal::system::software_reset();
            }
            Command::Reboot => esp_hal::system::software_reset(),
        }
        esp_println::print!("> ");
    }

    fn status(&self) {
        let up = self.client.as_ref().is_some_and(Client::is_up);
        let addr = self
            .client
            .as_ref()
            .and_then(|c| c.stack.config_v4())
            .map(|c| c.address);
        println!(
            "wifi: {} ({}), address: {:?}, paired: {}, free heap: {} bytes, firmware {}",
            if self.settings.has_wifi() {
                self.settings.ssid.as_str()
            } else {
                "not set"
            },
            if up { "up" } else { "down" },
            addr,
            if self.settings.is_paired() {
                "yes"
            } else {
                "no"
            },
            esp_alloc::HEAP.free(),
            env!("CARGO_PKG_VERSION"),
        );
    }

    /// Device pairing through QR sign-in: show the 6-digit code, wait for
    /// the owner to approve it in the Lucina app, keep the device token.
    async fn pair(&mut self) {
        let Some(client) = self.client.clone().filter(Client::is_up) else {
            println!("Wi-Fi is not up yet: set it with `wifi` and wait for `wifi: connected`.");
            return;
        };
        let mut body = [0u8; 1024];
        let request = alloc::format!("{{\"device\":{{\"name\":\"{DEVICE_NAME}\"}}}}");
        let (status, n) = match client
            .exchange("POST", "/v1/session/qr", None, Some(&request), &mut body)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("Pairing request failed: {e:?}");
                return;
            }
        };
        let text = core::str::from_utf8(&body[..n]).unwrap_or("");
        let (Some(code), Some(poll)) = (
            json::str_field(text, "shortCode"),
            json::str_field(text, "pollToken"),
        ) else {
            println!("Pairing request refused (HTTP {status}).");
            return;
        };
        let mut poll_path = [0u8; 200];
        let mut w = SliceWriter::new(&mut poll_path);
        if write!(w, "/v1/session/qr/poll?t={poll}").is_err() {
            println!("Pairing token too long.");
            return;
        }
        let poll_path = alloc::string::String::from(core::str::from_utf8(w.bytes()).unwrap_or(""));
        let expires = json::u64_field(text, "expiresIn").unwrap_or(120);
        println!("\r\n==============================");
        println!(
            "  Pairing code: {} {}",
            &code[..code.len().min(3)],
            &code[code.len().min(3)..]
        );
        println!("  Approve it in the Lucina app (Account > Scan QR code, type the code).");
        println!("  Valid for {expires} s.");
        println!("==============================\r\n");

        let deadline = Instant::now() + Duration::from_secs(expires + 5);
        let mut blink = false;
        while Instant::now() < deadline {
            for _ in 0..6 {
                blink = !blink;
                self.led
                    .set_level(if blink { Level::High } else { Level::Low });
                Timer::after(Duration::from_millis(250)).await;
            }
            let (status, n) = match client
                .exchange("GET", &poll_path, None, None, &mut body)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!("pair: poll failed: {e:?}");
                    continue;
                }
            };
            let text = core::str::from_utf8(&body[..n]).unwrap_or("");
            match json::str_field(text, "status") {
                Some("pending") => continue,
                Some("approved") => {
                    let Some(token) = json::str_field(text, "token") else {
                        println!(
                            "Approved, but the server sent no device token (is the server updated?)."
                        );
                        break;
                    };
                    if !self.settings.token.set(token) {
                        println!("Device token too long.");
                        break;
                    }
                    match self.store.save(&self.settings) {
                        Ok(()) => println!("Paired with Lucina. Hold the button and speak."),
                        Err(_) => println!("Paired, but saving to flash failed."),
                    }
                    self.led.set_low();
                    self.speaker.beep(990, 80).await;
                    return;
                }
                other => {
                    println!(
                        "Pairing ended: {} (HTTP {status}).",
                        other.unwrap_or("unexpected reply")
                    );
                    break;
                }
            }
        }
        self.led.set_low();
        println!("Pairing did not complete. Type `pair` to try again.");
    }
}
