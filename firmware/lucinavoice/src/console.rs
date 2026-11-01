//! Setup over the USB serial port (115200 baud, e.g. `espflash monitor`).
//!
//! | command | |
//! |---|---|
//! | `wifi` | asks for the SSID and the password (not echoed), saves, restarts |
//! | `pair` | shows a 6-digit code to approve in the Lucina app, saves the device token |
//! | `talk` | a 3-second voice turn without the button (bench test) |
//! | `status` | Wi-Fi, address, pairing, free heap |
//! | `forget` | erases Wi-Fi and pairing, restarts |
//! | `reboot` | restarts |
//!
//! The console only reads and parses; the main loop acts on [`Command`]s.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_hal::Async;
use esp_hal::uart::UartRx;
use esp_println::print;
use lucinavoice_core::settings::{Field, MAX_PASSWORD, MAX_SSID};

pub enum Command {
    Wifi {
        ssid: Field<MAX_SSID>,
        password: Field<MAX_PASSWORD>,
    },
    Pair,
    Talk,
    Status,
    Forget,
    Reboot,
}

pub static COMMANDS: Channel<CriticalSectionRawMutex, Command, 2> = Channel::new();

pub fn help() {
    print!("\r\nLucinaVoice commands: wifi, pair, talk, status, forget, reboot\r\n> ");
}

#[embassy_executor::task]
pub async fn console_task(mut rx: UartRx<'static, Async>) {
    let mut line = LineReader::new(&mut rx);
    help();
    loop {
        let mut buf = [0u8; 80];
        let n = line.read(&mut buf, Echo::Plain).await;
        let cmd = core::str::from_utf8(&buf[..n]).unwrap_or("").trim();
        let command = match cmd {
            "" => {
                print!("> ");
                continue;
            }
            "wifi" => {
                print!("SSID: ");
                let mut ssid_buf = [0u8; MAX_SSID + 1];
                let n = line.read(&mut ssid_buf, Echo::Plain).await;
                print!("Password (empty for an open network): ");
                let mut pw_buf = [0u8; MAX_PASSWORD + 1];
                let m = line.read(&mut pw_buf, Echo::Masked).await;
                let ssid = core::str::from_utf8(&ssid_buf[..n])
                    .ok()
                    .and_then(Field::new);
                let password = core::str::from_utf8(&pw_buf[..m]).ok().and_then(Field::new);
                pw_buf.fill(0);
                match (ssid, password) {
                    (Some(ssid), Some(password)) if !ssid.is_empty() => {
                        Command::Wifi { ssid, password }
                    }
                    _ => {
                        print!("SSID must be 1-32 bytes and the password at most 64.\r\n> ");
                        continue;
                    }
                }
            }
            "pair" => Command::Pair,
            "talk" => Command::Talk,
            "status" => Command::Status,
            "forget" => Command::Forget,
            "reboot" => Command::Reboot,
            _ => {
                help();
                continue;
            }
        };
        COMMANDS.send(command).await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Echo {
    Plain,
    Masked,
}

/// Line editing on raw serial bytes: CR or LF ends a line, backspace works.
struct LineReader<'a> {
    rx: &'a mut UartRx<'static, Async>,
    pending: [u8; 32],
    start: usize,
    end: usize,
    last_cr: bool,
}

impl<'a> LineReader<'a> {
    fn new(rx: &'a mut UartRx<'static, Async>) -> Self {
        Self {
            rx,
            pending: [0; 32],
            start: 0,
            end: 0,
            last_cr: false,
        }
    }

    async fn byte(&mut self) -> u8 {
        while self.start == self.end {
            self.start = 0;
            self.end = self.rx.read_async(&mut self.pending).await.unwrap_or(0);
        }
        self.start += 1;
        self.pending[self.start - 1]
    }

    /// Reads one line into `out` (extra bytes are dropped) and returns its length.
    async fn read(&mut self, out: &mut [u8], echo: Echo) -> usize {
        let mut n = 0;
        loop {
            let b = self.byte().await;
            match b {
                b'\n' if self.last_cr => {
                    self.last_cr = false;
                }
                b'\r' | b'\n' => {
                    self.last_cr = b == b'\r';
                    print!("\r\n");
                    return n;
                }
                0x08 | 0x7F => {
                    self.last_cr = false;
                    if n > 0 {
                        n -= 1;
                        print!("\x08 \x08");
                    }
                }
                0x20..=0x7E | 0x80..=0xFF => {
                    self.last_cr = false;
                    if n < out.len() {
                        out[n] = b;
                        n += 1;
                        if echo == Echo::Masked {
                            print!("*");
                        } else if b < 0x80 {
                            print!("{}", b as char);
                        }
                    }
                }
                _ => self.last_cr = false,
            }
        }
    }
}
