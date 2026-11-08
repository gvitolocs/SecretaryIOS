//! Device logs and specs, sent to the server over the voice link
//! (`docs/lucinavoice-protocol.md` §4).
//!
//! - Events are formatted to compact JSON when they happen and wait in a RAM
//!   ring ([`SLOTS`] × [`SLOT`] bytes; the oldest are dropped and counted).
//! - The link writer sends `hello` (specs) on connect, then a `telemetry`
//!   batch after each turn ([`kick`]) and every [`STATS_EVERY`], with a
//!   `stats` event: no extra radio wake-ups (power save is off anyway).
//! - Never recorded: audio, transcripts, the Wi-Fi password, the token.
//!
//! Power: measured when an INA219/INA226 answers at 0x40 on I2C (SDA GPIO21,
//! SCL GPIO18, 0.1 Ω shunt on the 5 V input), else estimated from a
//! duty-cycle model of the ESP32-WROOM-32 at 3.3 V (see [`PowerModel`]).

use core::cell::RefCell;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant};
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use lucinavoice_core::http::SliceWriter;

/// Bytes per stored event.
pub const SLOT: usize = 256;
/// Events kept while the link is down.
pub const SLOTS: usize = 40;
/// A `stats` event and a batch this often while the link is up.
pub const STATS_EVERY: Duration = Duration::from_secs(300);

/// TLS bytes written to and read from the network (all connections).
pub static UP_BYTES: AtomicU32 = AtomicU32::new(0);
pub static DOWN_BYTES: AtomicU32 = AtomicU32::new(0);
/// TCP writes (a proxy for packets, for the airtime estimate).
pub static UP_WRITES: AtomicU32 = AtomicU32::new(0);
/// Milliseconds the speaker and the mic were active.
pub static PLAY_MS: AtomicU32 = AtomicU32::new(0);
pub static REC_MS: AtomicU32 = AtomicU32::new(0);
/// Last Wi-Fi RSSI (dBm), 0 when unknown.
pub static RSSI: AtomicI32 = AtomicI32::new(0);

/// Wakes the link writer to send a batch now.
pub static KICK: Signal<CriticalSectionRawMutex, ()> = Signal::new();

pub fn kick() {
    KICK.signal(());
}

struct Ring {
    slots: [[u8; SLOT]; SLOTS],
    lens: [u16; SLOTS],
    head: usize,
    count: usize,
    dropped: u32,
}

static RING: Mutex<CriticalSectionRawMutex, RefCell<Ring>> = Mutex::new(RefCell::new(Ring {
    slots: [[0; SLOT]; SLOTS],
    lens: [0; SLOTS],
    head: 0,
    count: 0,
    dropped: 0,
}));

/// Records one event: `fields` writes the JSON members after `"t"` and `"k"`
/// (each starting with a comma). An event that does not fit its slot is
/// stored without its fields.
pub fn record(kind: &str, fields: impl FnOnce(&mut SliceWriter<'_>) -> core::fmt::Result) {
    let mut buf = [0u8; SLOT];
    let t = Instant::now().as_millis();
    let len = {
        let mut w = SliceWriter::new(&mut buf);
        let ok = write!(w, "{{\"t\":{t},\"k\":\"{kind}\"").is_ok()
            && fields(&mut w).is_ok()
            && w.write_str("}").is_ok();
        if ok { w.bytes().len() } else { 0 }
    };
    let len = if len == 0 {
        let mut w = SliceWriter::new(&mut buf);
        let _ = write!(w, "{{\"t\":{t},\"k\":\"{kind}\",\"cut\":true}}");
        w.bytes().len()
    } else {
        len
    };
    RING.lock(|r| {
        let mut r = r.borrow_mut();
        let idx = (r.head + r.count) % SLOTS;
        if r.count == SLOTS {
            r.head = (r.head + 1) % SLOTS;
            r.dropped += 1;
        } else {
            r.count += 1;
        }
        r.slots[idx][..len].copy_from_slice(&buf[..len]);
        r.lens[idx] = len as u16;
    });
}

/// Writes a `telemetry` frame with up to 32 queued events into `out` and
/// removes them from the ring. `None` when nothing is queued.
pub fn batch_frame(out: &mut [u8]) -> Option<usize> {
    RING.lock(|r| {
        let mut r = r.borrow_mut();
        if r.count == 0 {
            return None;
        }
        let mut w = SliceWriter::new(out);
        let now = Instant::now().as_millis();
        write!(
            w,
            "{{\"type\":\"telemetry\",\"now\":{now},\"dropped\":{},\"events\":[",
            r.dropped
        )
        .ok()?;
        let mut taken = 0;
        while r.count > 0 && taken < 32 {
            let idx = r.head;
            let len = r.lens[idx] as usize;
            // Room for this event, a comma and the closing "]}".
            if w.bytes().len() + len + 3 > w.capacity() {
                break;
            }
            let ev = core::str::from_utf8(&r.slots[idx][..len]).unwrap_or("{}");
            let sep = if taken > 0 { "," } else { "" };
            if write!(w, "{sep}{ev}").is_err() {
                break;
            }
            r.head = (r.head + 1) % SLOTS;
            r.count -= 1;
            taken += 1;
        }
        w.write_str("]}").ok()?;
        r.dropped = 0;
        Some(w.bytes().len())
    })
}

/// Static facts about the device, set once at boot.
#[derive(Clone)]
pub struct Specs {
    pub flash_bytes: u32,
    pub heap_total: u32,
    pub ina: Option<Ina>,
    pub reset: lucinavoice_core::settings::Field<32>,
    pub boot_seq: u32,
}

/// The `hello` frame.
pub fn hello_frame(specs: &Specs, out: &mut [u8]) -> Option<usize> {
    let rev = esp_hal::efuse::chip_revision();
    let mac = esp_hal::efuse::base_mac_address();
    let mac = mac.as_bytes();
    let mut w = SliceWriter::new(out);
    write!(
        w,
        "{{\"type\":\"hello\",\"now\":{},\"specs\":{{\"chip\":\"esp32\",\"rev\":\"v{}.{}\",\"cores\":2,\"cpu_mhz\":240,\
         \"flash_bytes\":{},\"psram_bytes\":0,\"fw\":\"{}\",\"commit\":\"{}\",\"mac\":\"{:02x}:{:02x}\",\"heap_total\":{},\
         \"ina\":{},\"temp_sensor\":false}},\"boot\":{{\"reset\":\"{}\",\"seq\":{}}}}}",
        Instant::now().as_millis(),
        rev.major,
        rev.minor,
        specs.flash_bytes,
        env!("CARGO_PKG_VERSION"),
        option_env!("LUCINAVOICE_COMMIT").unwrap_or("unknown"),
        mac[4],
        mac[5],
        specs.heap_total,
        match specs.ina {
            Some(Ina::Ina219) => "\"ina219\"",
            Some(Ina::Ina226) => "\"ina226\"",
            None => "null",
        },
        specs.reset.as_str(),
        specs.boot_seq,
    )
    .ok()?;
    Some(w.bytes().len())
}

/// Estimated draw of an ESP32-WROOM-32 board at 3.3 V (datasheet figures):
/// CPU at 240 MHz with the radio always listening (power save off) about
/// 100 mA; TX bursts about 140 mA more for their airtime; the MAX98357A at
/// speech level about 0.3 W while playing; the INA/INMP441 a few mW.
pub struct PowerModel {
    last: Instant,
    last_air_ms: u32,
    last_play: u32,
    last_rec: u32,
    pub energy_mwh: f32,
}

const BASE_MW: f32 = 330.0;
const TX_EXTRA_MW: f32 = 462.0;
const AMP_MW: f32 = 300.0;
const MIC_MW: f32 = 5.0;
/// Assumed PHY rate for the airtime estimate (802.11n at moderate RSSI).
const PHY_MBPS: f32 = 24.0;
/// Air time per write beyond its payload (preamble, ACK, TCP/IP headers).
const PER_WRITE_US: f32 = 150.0;

impl Default for PowerModel {
    fn default() -> Self {
        Self {
            last: Instant::now(),
            last_air_ms: 0,
            last_play: 0,
            last_rec: 0,
            energy_mwh: 0.0,
        }
    }
}

/// Estimated TX airtime since boot, ms.
pub fn tx_air_ms() -> u32 {
    let bytes = UP_BYTES.load(Ordering::Relaxed) as f32;
    let writes = UP_WRITES.load(Ordering::Relaxed) as f32;
    (bytes * 8.0 / (PHY_MBPS * 1000.0) + writes * PER_WRITE_US / 1000.0) as u32
}

impl PowerModel {
    /// Average draw since the last call (mW), measured if `rail_mw` is given;
    /// adds the interval's energy.
    pub fn update(&mut self, rail_mw: Option<f32>) -> f32 {
        let dt_ms = self.last.elapsed().as_millis().max(1) as f32;
        let air = tx_air_ms();
        let play = PLAY_MS.load(Ordering::Relaxed);
        let rec = REC_MS.load(Ordering::Relaxed);
        let mw = rail_mw.unwrap_or_else(|| {
            BASE_MW
                + (TX_EXTRA_MW * air.wrapping_sub(self.last_air_ms) as f32
                    + AMP_MW * play.wrapping_sub(self.last_play) as f32
                    + MIC_MW * rec.wrapping_sub(self.last_rec) as f32)
                    / dt_ms
        });
        self.energy_mwh += mw * dt_ms / 3_600_000.0;
        self.last = Instant::now();
        (self.last_air_ms, self.last_play, self.last_rec) = (air, play, rec);
        mw
    }
}

/// A current/voltage monitor on the 5 V input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ina {
    Ina219,
    Ina226,
}

const INA_ADDR: u8 = 0x40;
/// Shunt on the common breakout boards (R100).
const SHUNT_OHM: f32 = 0.1;

fn read_reg(i2c: &mut I2c<'static, Blocking>, reg: u8) -> Option<u16> {
    let mut b = [0u8; 2];
    i2c.write_read(INA_ADDR, &[reg], &mut b).ok()?;
    Some(u16::from_be_bytes(b))
}

/// Which monitor answers at 0x40, if any.
pub fn probe(i2c: &mut I2c<'static, Blocking>) -> Option<Ina> {
    if read_reg(i2c, 0xFE) == Some(0x5449) {
        return Some(Ina::Ina226);
    }
    // INA219 has no ID register; its configuration resets to 0x399F.
    (read_reg(i2c, 0x00) == Some(0x399F)).then_some(Ina::Ina219)
}

/// Rail voltage (mV) and current (mA) from the shunt and bus registers.
pub fn measure(i2c: &mut I2c<'static, Blocking>, ina: Ina) -> Option<(f32, f32)> {
    let shunt = read_reg(i2c, 0x01)? as i16 as f32;
    let bus = read_reg(i2c, 0x02)?;
    Some(match ina {
        // Shunt LSB 10 µV; bus value in bits 15..3, LSB 4 mV.
        Ina::Ina219 => (f32::from(bus >> 3) * 4.0, shunt * 0.010 / SHUNT_OHM),
        // Shunt LSB 2.5 µV; bus LSB 1.25 mV.
        Ina::Ina226 => (f32::from(bus) * 1.25, shunt * 0.0025 / SHUNT_OHM),
    })
}

/// What the link task needs to report specs, stats and power.
pub struct Tele {
    pub specs: Specs,
    pub i2c: Option<I2c<'static, Blocking>>,
    pub power: PowerModel,
}

impl Tele {
    /// Records a `stats` event: uptime, heap, RSSI, bytes, airtime, power.
    pub fn record_stats(&mut self) {
        let rail = match (self.specs.ina, self.i2c.as_mut()) {
            (Some(ina), Some(i2c)) => measure(i2c, ina),
            _ => None,
        };
        let mw = self.power.update(rail.map(|(mv, ma)| mv * ma / 1000.0));
        let energy = self.power.energy_mwh;
        let heap = esp_alloc::HEAP.stats();
        let free = heap.size.saturating_sub(heap.current_usage);
        let min_free = heap.size.saturating_sub(heap.max_usage);
        let rssi = RSSI.load(Ordering::Relaxed);
        let up = UP_BYTES.load(Ordering::Relaxed);
        let down = DOWN_BYTES.load(Ordering::Relaxed);
        let air = tx_air_ms();
        record("stats", |w| {
            write!(
                w,
                ",\"uptime_ms\":{},\"heap_free\":{free},\"heap_min_free\":{min_free},",
                Instant::now().as_millis()
            )?;
            if rssi == 0 {
                w.write_str("\"rssi\":null,")?;
            } else {
                write!(w, "\"rssi\":{rssi},")?;
            }
            write!(
                w,
                "\"up_bytes\":{up},\"down_bytes\":{down},\"tx_air_ms\":{air},\"cpu_mhz\":240,\"temp_c\":null,\
                 \"power\":{{\"est_mw\":{mw:.0},\"energy_mwh\":{energy:.1}"
            )?;
            if let Some((mv, ma)) = rail {
                write!(w, ",\"rail_mv\":{mv:.0},\"rail_ma\":{ma:.1}")?;
            }
            w.write_str("}")
        });
    }
}

/// Shows a value's `Debug` form safely inside a JSON string (quotes,
/// backslashes and control characters become `'`, `/` and spaces).
pub struct Esc<T>(pub T);

impl<T: core::fmt::Debug> core::fmt::Display for Esc<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        struct Clean<'a, 'b>(&'a mut core::fmt::Formatter<'b>);
        impl core::fmt::Write for Clean<'_, '_> {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                for c in s.chars() {
                    let c = match c {
                        '"' => '\'',
                        '\\' => '/',
                        c if c.is_control() => ' ',
                        c => c,
                    };
                    self.0.write_char(c)?;
                }
                Ok(())
            }
        }
        write!(Clean(f), "{:?}", self.0)
    }
}
