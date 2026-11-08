# LucinaVoice firmware

A push-to-talk speaker for Lucina on an ESP32, meant to replace the board of an old Amazon Echo. Hold the button,
speak, let go: the audio streams to `POST /v1/voice` on getsecretary.tech, and Lucina's spoken answer plays as it
arrives.

| crate | |
|---|---|
| `lucinavoice/` | the firmware (esp-hal 1.2, embassy, esp-radio Wi-Fi, mbedtls-rs TLS), `xtensa-esp32-none-elf` |
| `lucinavoice-core/` | logic without hardware (HTTP/1.1 framing, settings record, PCM), tested on the host |

Both are standalone (not members of the repository workspace): they use the `esp` toolchain and their own target.

## Board

ESP32-D0WD v3 on an ESP-WROOM-32, 4 MB flash, no PSRAM. It is the board plugged into the Mac in November 2026,
serial `/dev/cu.usbserial-10` (CH340).

| part | signal | GPIO |
|---|---|---|
| MAX98357A amplifier (I2S0) | BCLK / LRC / DIN | 26 / 25 / 22 |
| INMP441 microphone (I2S1) | SCK / WS / SD | 32 / 33 / 34 (L/R either way) |
| push to talk | button to GND | 0 (BOOT) or 4 |
| status LED | on: listening; blinking: pairing | 2 |

The Echo's own microphone array and LED ring are not driven. Put the INMP441 behind one of the original mic holes
and the amplifier on the original speaker. Power the amp from 5 V (buck from the Echo's supply) and the ESP32 board
from the same 5 V.

## Build and flash (on the Mac, where the board is plugged in)

```bash
. ~/export-esp.sh
cd firmware/lucinavoice
cargo build --release
espflash flash --port /dev/cu.usbserial-10 --chip esp32 --monitor target/xtensa-esp32-none-elf/release/lucinavoice
```

Host tests for the core crate: `cd firmware/lucinavoice-core && cargo +stable test`.

## Setup (serial console, 115200 baud, e.g. `espflash monitor --port /dev/cu.usbserial-10`)

1. `wifi`: type the SSID, then the password (shown as `*`). It is saved to flash and the board restarts. Supported
   modes: WPA2-Personal, or open networks.
2. Wait for `wifi: connected`, then `pair`. A 6-digit code is printed. Approve it in the Lucina app (Account → Scan QR
   code → type the code). The device token is saved to flash. The new device appears in the app's device list as
   "LucinaVoice", where it can be revoked.
3. Hold the button and speak. `talk` runs a 3-second turn without the button.

Other commands: `status`, `forget` (erases Wi-Fi and pairing), `reboot`.

The Wi-Fi password and the device token live only in the board's flash (`nvs` partition, first sector). They are
never logged, and the console never echoes the password.

## Server contract

- Pairing: `POST /v1/session/qr {"device":{"name":"LucinaVoice"}}`, then poll `GET /v1/session/qr/poll?t=` until
  `{status:"approved", token}`.
- A turn: `POST /v1/voice` with `Authorization: Bearer <token>`, `Content-Type: audio/x-s16le;rate=16000`, and a
  chunked body. The answer is `audio/x-s16le;rate=24000`, with `X-Lucina-Transcript` / `X-Lucina-Reply`
  (percent-encoded). See `docs/server-api.md`.

TLS uses Mbed TLS with the ISRG Root X1/X2 certificates in `lucinavoice/certs/`. The board has no clock, so
certificate dates are not checked (chain and hostname are).

## Known limits

- No wake word: there is no PSRAM for a model. Swap in an ESP32-S3 with PSRAM for that.
- One turn at a time. The answer is played as it streams, and a network stall shorter than about 0.3 s is buffered.
- Pure WPA3 networks are not supported by the current station config.
