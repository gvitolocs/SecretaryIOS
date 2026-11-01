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
//! | GPIO2 | status LED (on: listening, pulsing: waiting for Lucina, blinking: pairing) |
//! | I2S pins | see `lucinavoice::audio` |
//!
//! A turn over the persistent link (`docs/lucinavoice-protocol.md`): press →
//! earcon at once, `start`, mic frames while held → release → `end` and an
//! "ack" blip → the answer plays as its frames arrive. Pressing again while
//! Lucina answers cancels that turn and starts a new one (barge-in). Without
//! the link (old server, link reconnecting) a turn falls back to one
//! `POST /v1/voice` request.

use alloc::vec;
use core::fmt::Write as _;
use core::sync::atomic::Ordering;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
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
use lucinavoice::link::{self, ENDPOINT, EVENTS, Event, FRAME, LINK_UP, OUT, Out, PcmBuf};
use lucinavoice::net::{self, Client, NetError};
use lucinavoice::player::{self, PLAY, Play};
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
/// Longest wait for the answer to finish after the end of speech.
const MAX_ANSWER: Duration = Duration::from_secs(60);

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static CELL: StaticCell<$t> = StaticCell::new();
        CELL.uninit().write($val)
    }};
}

struct Device {
    spawner: Spawner,
    store: Store,
    settings: Settings,
    client: Option<Client>,
    link_started: bool,
    mic: Mic,
    led: Output<'static>,
    ptt: Input<'static>,
    ptt2: Input<'static>,
}

/// How a turn over the link ended.
enum Outcome {
    Done,
    /// The button was pressed again while Lucina answered: start a new turn.
    BargeIn,
}

#[derive(Debug)]
#[allow(dead_code, reason = "the fields are read through Debug in the logs")]
enum TurnError {
    Net(NetError),
    LinkLost,
    Server(lucinavoice_core::settings::Field<40>),
    Timeout,
    Mic,
}

impl From<NetError> for TurnError {
    fn from(e: NetError) -> Self {
        TurnError::Net(e)
    }
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
    // Logged after the UART is set up: reconfiguring it cuts a line in flight.
    info!("LucinaVoice {} starting", env!("CARGO_PKG_VERSION"));

    // Speaker: I2S0 out, 24 kHz stereo 16-bit, owned by the player task.
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
    spawner.spawn(player::player_task(speaker).expect("player task"));

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
            // Modem sleep delays incoming packets to the next DTIM beacon: off, for latency.
            if let Err(e) = controller.set_power_saving(esp_radio::wifi::PowerSaveMode::None) {
                warn!("wifi: could not turn power saving off: {e:?}");
            }
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
        spawner,
        store,
        settings,
        client,
        link_started: false,
        mic,
        led,
        ptt,
        ptt2,
    };
    dev.start_link();
    PLAY.send(Play::Beep { freq: 660, ms: 60 }).await;
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

    /// Opens the persistent link once Wi-Fi and pairing are set up.
    fn start_link(&mut self) {
        if self.link_started || !self.settings.is_paired() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        match link::link_task(client, self.settings.token.clone()) {
            Ok(token) => {
                self.spawner.spawn(token);
                self.link_started = true;
            }
            Err(e) => warn!("link: could not start: {e:?}"),
        }
    }

    /// One turn: while a button is held, or for `fixed` (the `talk` command).
    async fn turn(&mut self, fixed: Option<Duration>) {
        let ready = self.client.as_ref().is_some_and(Client::is_up) && self.settings.is_paired();
        if !ready {
            warn!(
                "turn: not ready (wifi up: {}, paired: {})",
                self.client.as_ref().is_some_and(Client::is_up),
                self.settings.is_paired()
            );
            player::error().await;
            self.wait_release().await;
            return;
        }
        loop {
            let started = Instant::now();
            let result = if LINK_UP.load(Ordering::Acquire) {
                self.link_turn(fixed).await
            } else {
                info!("turn: link down, using POST /v1/voice");
                self.http_turn(fixed).await.map(|()| Outcome::Done)
            };
            self.led.set_low();
            match result {
                Ok(Outcome::Done) => info!("turn: done in {} ms", started.elapsed().as_millis()),
                Ok(Outcome::BargeIn) => {
                    info!("turn: barge-in");
                    continue;
                }
                Err(e) => {
                    match &e {
                        TurnError::Server(code) => error!("turn: server error {}", code.as_str()),
                        _ => error!("turn: {e:?}"),
                    }
                    PLAY.send(Play::Flush).await;
                    player::error().await;
                }
            }
            break;
        }
        self.wait_release().await;
    }

    /// A turn over the persistent link.
    async fn link_turn(&mut self, fixed: Option<Duration>) -> Result<Outcome, TurnError> {
        ENDPOINT.store(false, Ordering::Release);
        while let Ok(stale) = EVENTS.try_receive() {
            if let Event::Down = stale {
                return Err(TurnError::LinkLost);
            }
        }
        player::listening().await;
        OUT.send(Out::Start).await;
        self.led.set_high();

        let mut sent = 0usize;
        {
            let mut rec = self.mic.start().ok_or(TurnError::Mic)?;
            let until = Instant::now() + fixed.unwrap_or(MAX_TURN);
            while (fixed.is_some() || self.ptt.is_low() || self.ptt2.is_low())
                && Instant::now() < until
                && !ENDPOINT.load(Ordering::Acquire)
            {
                if !LINK_UP.load(Ordering::Acquire) {
                    return Err(TurnError::LinkLost);
                }
                let mut frame = [0u8; FRAME];
                let n = match rec.read(&mut frame).await {
                    Ok(n) => n,
                    Err(_) => {
                        warn!("turn: mic overrun, cutting the turn short");
                        break;
                    }
                };
                if n > 0 {
                    OUT.send(Out::Audio(frame, n)).await;
                    sent += n;
                }
            }
        }
        OUT.send(Out::End).await;
        let end_of_speech = Instant::now();
        self.led.set_low();
        player::ack().await;
        info!(
            "turn: sent {} ms of audio",
            sent as u64 * 1000 / (MIC_RATE as u64 * 2)
        );

        // Wait for the answer, pulsing the LED; a new press cancels it.
        let deadline = end_of_speech + MAX_ANSWER;
        let mut pulse = false;
        let mut first_audio = None;
        loop {
            if Instant::now() > deadline {
                OUT.send(Out::Cancel).await;
                return Err(TurnError::Timeout);
            }
            match select3(
                EVENTS.receive(),
                press(&mut self.ptt, &mut self.ptt2),
                Timer::after(Duration::from_millis(250)),
            )
            .await
            {
                Either3::First(event) => match event {
                    Event::Listening | Event::Up => {}
                    Event::Transcript(t) => info!("turn: heard {:?}", t.as_str()),
                    Event::AudioStart => {
                        let ms = end_of_speech.elapsed().as_millis();
                        first_audio = Some(ms);
                        info!("turn: first audio {ms} ms after the end of speech");
                        self.led.set_low();
                    }
                    Event::Reply(r) => info!("turn: Lucina says {:?}", r.as_str()),
                    Event::Done(ms) => {
                        info!(
                            "turn: done; device first audio {:?} ms; server {}",
                            first_audio,
                            ms.as_str()
                        );
                        return Ok(Outcome::Done);
                    }
                    Event::Error(code) => return Err(TurnError::Server(code)),
                    Event::Down => return Err(TurnError::LinkLost),
                },
                Either3::Second(()) => {
                    OUT.send(Out::Cancel).await;
                    PLAY.send(Play::Flush).await;
                    return Ok(Outcome::BargeIn);
                }
                Either3::Third(()) => {
                    if first_audio.is_none() {
                        pulse = !pulse;
                        self.led
                            .set_level(if pulse { Level::High } else { Level::Low });
                    }
                }
            }
        }
    }

    /// The fallback: one `POST /v1/voice` request (TLS handshake included).
    async fn http_turn(&mut self, fixed: Option<Duration>) -> Result<(), TurnError> {
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
        session.write_all(head).await.map_err(NetError::from)?;

        player::listening().await;
        self.led.set_high();
        let mut sent = 0usize;
        {
            let mut rec = self.mic.start().ok_or(TurnError::Mic)?;
            let until = Instant::now() + fixed.unwrap_or(MAX_TURN);
            let mut pcm = [0u8; 1024];
            let mut size = [0u8; 18];
            while (fixed.is_some() || self.ptt.is_low() || self.ptt2.is_low())
                && Instant::now() < until
            {
                let n = match rec.read(&mut pcm).await {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 {
                    continue;
                }
                session
                    .write_all(http::chunk_size_line(n, &mut size))
                    .await
                    .map_err(NetError::from)?;
                session.write_all(&pcm[..n]).await.map_err(NetError::from)?;
                session.write_all(b"\r\n").await.map_err(NetError::from)?;
                sent += n;
            }
        }
        session
            .write_all(http::LAST_CHUNK)
            .await
            .map_err(NetError::from)?;
        session.flush().await.map_err(NetError::from)?;
        let end_of_speech = Instant::now();
        self.led.set_low();
        player::ack().await;
        info!(
            "turn: sent {} ms of audio",
            sent as u64 * 1000 / (MIC_RATE as u64 * 2)
        );

        let mut buf = vec![0u8; 2048];
        let (head, mut body) = net::read_head(&mut session, &mut buf).await?;
        info!(
            "turn: HTTP {} after {} ms, heard {:?}",
            head.status,
            end_of_speech.elapsed().as_millis(),
            head.transcript.as_str()
        );
        if head.status != 200 {
            let msg = net::read_small_body(&mut session, &mut body, &mut buf, 300).await;
            warn!(
                "turn: server said {}",
                core::str::from_utf8(&msg).unwrap_or("?")
            );
            return Err(TurnError::Net(NetError::Http));
        }
        let mut out = PcmBuf::default();
        let mut first = true;
        while let Some(chunk) = body.next(&mut session, &mut buf).await? {
            if first && !chunk.is_empty() {
                first = false;
                info!(
                    "turn: first audio {} ms after the end of speech",
                    end_of_speech.elapsed().as_millis()
                );
            }
            out.push(chunk).await;
        }
        out.send().await;
        PLAY.send(Play::Stop).await;
        let _ = session.close().await;
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
            "wifi: {} ({}), address: {:?}, paired: {}, link: {}, free heap: {} bytes, firmware {}",
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
            if LINK_UP.load(Ordering::Acquire) {
                "up"
            } else {
                "down"
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
                    PLAY.send(Play::Beep { freq: 990, ms: 80 }).await;
                    self.start_link();
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
