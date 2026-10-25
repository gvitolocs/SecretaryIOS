# Lucina on Android: Rust core, thin Kotlin shell

Owner's direction (2026-10-24): "that app also should be in rust and very fast in everything minimal kotlin".

Two apps run on Lucina's phone, a Samsung S20 FE (SM-G780G, Android 13, Lebara SIM):

| App | Package | What it does |
|---|---|---|
| **Lucina OTP** | `tech.getsecretary.lucinaotp` | Forwards OTP codes from incoming SMS (and, when allowed, notifications) to the relay, so Lucina can hand them to the owner. |
| **Secretary · Lucina** | `tech.getsecretary.secretary` | Calls (Telecom/InCallService), voice and transcripts for Lucina. |

The two apps stay separate, each with the fewest permissions it needs. They are built from this workspace, and each has its own
Rust library so neither ships the other's code.

## Split: what is Rust, what is Kotlin

Rust does all the logic. Kotlin does only what Android forces into the JVM:

| Kotlin (JVM-only) | Why it cannot be Rust |
|---|---|
| `AndroidManifest.xml` | Android reads it from the APK. |
| `MainActivity`: status, enable/disable, permission prompt | Runtime permission prompts are an `Activity` API. |
| `SmsReceiver` (`goAsync`, `Telephony.Sms.Intents.getMessagesFromIntent`) | The framework instantiates receivers as JVM classes. PDU decoding (GSM-7/UCS-2, multipart) is one framework call. |
| `OtpNotificationListener` (`NotificationListenerService`) | Same; the owner grants it in system settings. |
| `RetryJobService` (`JobScheduler`, network constraint) | Job scheduling is a framework API. |
| `KeyVault` (Android Keystore wrap of the 32-byte data key) | The Keystore is a JVM API. |
| `PairingProvider` (adb-only `ContentProvider`) | Providers are JVM classes. |

Everything else is Rust: detection, redaction, dedup, the encrypted state and queue, HTTPS, the protocol and status.

- **No foreground service.** An SMS broadcast wakes the app even when it is not running. The app is on the deviceidle (Doze)
  allowlist, so its network calls go through. Version 0.4.0 removed the old service and its permanent notification on purpose.
- **No UniFFI/JNA.** UniFFI's Kotlin bindings need JNA (about 1.3 MB) and make the first call slower. Each app instead uses a few
  hand-written `jni` functions that take strings and return JSON strings.
- **No Gradle and no AndroidX.** The build is `kotlinc` → R8 (which shrinks the Kotlin stdlib away) → `aapt2` → `zipalign` →
  `apksigner`, plus `cargo` for the native library.

## Crates

| Crate | Kind | Used by |
|---|---|---|
| `lucina-phone-core` | lib, no I/O: `otp` (parse/redact), `intent`, `proto`, `reply` | everything below |
| `lucina-android` | cdylib `liblucina_android.so` (JNI for `tech.getsecretary.lucinaotp.Native`) | Lucina OTP app |
| `lucina-android-calls` | cdylib `liblucina_calls.so` (JNI for `tech.getsecretary.secretary.Native`) | Secretary app |
| `lucina-otp-relay` | bin `lucina-otp-relay` (x86_64 musl, runs on p1) | Caddy `/v1/phone/otp/*` |
| `lucina-server` / `lucina-intent` | Rust Lucina | answers `/otp` in `/api/secretary` |

`tools/lucina-device` is the imported 0.4 code (relay, phone MCP, JNI parser). The new crates replace it piece by piece.

## Relay protocol

Base URL: `https://getsecretary.tech/v1/phone/otp` (`lucina_phone_core::proto::ORIGIN`). Bodies are at most 2 KiB. Every
response has `Cache-Control: no-store`. The phone token and the owner token are different 64-hex bearer tokens. The relay reads
them from `$LUCINA_OTP_STATE_DIR/{phone,owner}.token`.

| Route | Token | Request → response |
|---|---|---|
| `GET /window` | phone | → `WindowState` (protocol 1, for app 0.4.0 only) |
| `POST /incoming` | phone | `Incoming` → `IncomingAck` (200). 409 when paused, or when a protocol-1 `windowId` is wrong. 410 when the code is already expired. 400 when the body is invalid. |
| `GET /status` | owner | → `RelayStatus` (`protocol: 2`) |
| `POST /arm` | owner | `ArmRequest` → `WindowState` |
| `POST /disarm` | owner | → `{"disarmed":true}` |
| `GET /otp` | owner | → `Delivered` (removed by this read), or 204 when nothing fresh is waiting |

`/incoming` details:

- **Protocol 2:** no `windowId`. The relay accepts the code while it is armed (continuous or a timed window).
- **Expiry:** `expiresAt` comes from `proto::expires_at(receivedAt, now, windowEnd)`.
- **Repeats:** a repeated `eventId` within 2 minutes gets `{accepted:true, duplicate:true}`.
- **Test events:** `test:true` is validated and answered `{accepted:true, test:true}`, never stored. The phone can measure the whole
  path this way without a fake code reaching the owner.
- **Limits:** at most 8 codes wait, and the oldest is dropped first; at most 64 event ids are remembered.

`/otp` returns the newest code that has not expired. Codes live only in memory. When `LUCINA_OTP_CONTINUOUS=true`, the relay starts
armed and continuous unless `$LUCINA_OTP_STATE_DIR/paused` exists. `/disarm` writes that file and `/arm` removes it.

## Rust Lucina (`crates/lucina-server/src/otp.rs`)

`/api/secretary/chat` answers an OTP request (`otp_intent`) before anything else. The request never reaches the model, the timeline,
the recent turns, Honcho or the `clientMessageId` replay cache.

- **Lucina MCP turns** (`[via Lucina MCP…]` prefix or an `mcp-…` session) get `reply::NOT_PRIVATE` and `action: "reply"`.
- **Otherwise** Lucina calls the relay with the owner token. The take logic is the same as `tools/lucina-device` `otp::chat`: on a 204
  she checks `/status`, arms the relay (300 s) if it is not armed, and answers `reply::waiting(continuous)`.
- **Response:** `{reply, action: "private_otp", sensitive: true, expiresAt?, sessionId, clientMessageId, job: null, userItemId: null,
  lucinaItemId: null}`. `expiresAt` is set only when the reply carries a code.
- **Config:** `LUCINA_OTP_OWNER_TOKEN_FILE` is the relay's owner token file (mode 0600), read on every request. It is unset by
  default, and then every OTP request answers `reply::UNAVAILABLE`. The relay URL is `proto::ORIGIN` and has no variable.
- **Relay calls:** 5 s timeout, no redirects, responses of at most 8 KiB. Any error answers `reply::UNAVAILABLE`.
- **Telegram:** the Rust Lucina has no Telegram transport yet. A future one calls `otp::respond` with its `reply::Asker`, which applies
  `private_owner`.
- **Defence in depth:** prompts to the model and log lines get `lucina_secrets::otp` (`Redactor::redact_outbound`), which masks codes
  near an OTP context word as `[otp]`. The plain redactor used by the API reply, the timeline and memory does not mask codes, because
  the owner's own delivery passes through the API sink.

## Lucina OTP app (`tech.getsecretary.lucinaotp`, 1.0.0, versionCode 5)

- **Manifest:**
  - `RECEIVE_SMS`, `INTERNET`, `RECEIVE_BOOT_COMPLETED` (to re-arm the retry job after a reboot);
  - the receiver protected by `BROADCAST_SMS`;
  - the listener service protected by `BIND_NOTIFICATION_LISTENER_SERVICE`;
  - the job service protected by `BIND_JOB_SERVICE`;
  - the provider protected by `DUMP` and checking adb UIDs (shell 2000, root 0, self);
  - `allowBackup=false`, no cleartext traffic, `extractNativeLibs=false` with page-aligned, uncompressed `.so`;
  - minSdk 26, targetSdk 35.
- **It is not allowed** to read SMS history, send SMS, read contacts or call logs, or run a foreground service.
- **Signing:** the same signing key as 0.4.0, so it upgrades in place and keeps the granted SMS permission and the Doze allowlist
  entry. The key lives in `/home/nez/Projects/Lucina/data/android-build/otp-bridge/signing.p12`, with the password in
  `signing.pass` next to it, both mode 0600. Never print either.

### JNI surface (`object Native` in Kotlin, `@JvmStatic external fun`, library `lucina_android`)

All functions are synchronous and must not be called on the main thread. Every function returns a JSON string. They never return or
log a code, a token or a message body. `"result"` values are the snake_case strings below.

| Function | Returns |
|---|---|
| `init(filesDir: String, dataKey: ByteArray, legacyToken: String?): String` | Opens or creates `filesDir/lucina/` (0700). `dataKey` is 32 bytes. `legacyToken` is the 0.4.0 `SharedPreferences("bridge").internetToken`, imported once if it is valid; Kotlin then deletes the pref. Returns `status()`. Safe to call repeatedly: the first call wins and later calls are cheap. |
| `onSms(sender: String, body: String, receivedAtMs: Long): String` | `{"result": "disabled"\|"not_otp"\|"duplicate"\|"sent"\|"queued"\|"paused"\|"unpaired"\|"expired", "ms": <elapsed>}` |
| `onNotification(pkg: String, title: String, text: String, postedAtMs: Long): String` | Same result set. Returns `"disabled"` unless notification forwarding is on. Ignores the app's own package. |
| `flush(): String` | `{"sent": n, "dropped": n, "pending": n, "retry": bool}`. Sends queued codes that are still fresh; `retry` means "schedule me again when the network is up". |
| `status(): String` | `{"version", "protocol", "enabled", "notifications", "paired", "pending", "lastResult", "lastForwardAtMs", "lastLatencyMs", "selfTest"}`. `selfTest` means the parser round-trips its fixture. |
| `configure(json: String): String` | Patch: `{"enabled"?: bool, "notifications"?: bool, "token"?: "<64 hex>", "url"?: ORIGIN}`. `url` must equal `ORIGIN`. Returns `status()`. |
| `selfTest(network: Boolean): String` | Parser fixture only, or with `network=true` also a `test:true` round trip to the relay. Returns `{"parse": bool, "result", "ms"}`. |

### Rust behaviour

- **Order:** parse first. A message without a code never touches the network or the disk.
- **Dedup:** the same code from any source within 120 s is sent only once, so an SMS and its notification count as one.
- **Network:**
  - one `POST /incoming` (protocol 2) over rustls (graviola provider, webpki roots, host must be `getsecretary.tech`);
  - no redirects; connect timeout 3 s, total 5 s;
  - the response may be at most 4 KiB, and the agent and connection are reused within the process.
  - A 409 means paused: drop the code and set `lastResult=paused`. A 401 means unpaired: drop the code. 410 or 400: drop it.
  - On a network error or a 5xx, queue the code.
- **Queue:**
  - stored AES-256-GCM-encrypted in `filesDir/lucina/queue.bin`, holding code, sender, event id, received time and source;
  - each entry lives at most `CODE_TTL_MS` after it was received, since an older code is useless;
  - at most 8 entries; writes are atomic (temp file, fsync, rename).
- **State:** config and token in `filesDir/lucina/state.bin`, AES-256-GCM with the same data key and a random 96-bit nonce per write.
  The token is never written in plaintext, and keys and tokens are zeroized after use.
- **No logging** of bodies, codes or tokens. Kotlin may log `result` and `ms` only.

### Kotlin behaviour

- `SmsReceiver`: `goAsync()`, then one background thread (`Native.onSms`). A `queued` result schedules `RetryJobService`
  (network-connected constraint, backoff 10 s). `finish()` comes within 9 s.
- `OtpNotificationListener`: a single-thread executor calls `Native.onNotification` with `EXTRA_TITLE` and
  `EXTRA_BIG_TEXT ?: EXTRA_TEXT`, skipping ongoing and group-summary notifications.
- `KeyVault`: an AES-256-GCM key `lucina-otp-data` in `AndroidKeyStore`, non-exportable. It wraps a random 32-byte data key stored
  in `filesDir/lucina/key.wrapped`.
- `MainActivity`: built in code, no resources. It shows a status line from `status()`, plus:
  - Enable/disable SMS forwarding (requests `RECEIVE_SMS`);
  - a notification-access button (`ACTION_NOTIFICATION_LISTENER_SETTINGS`) and a toggle;
  - a "Test connection" button (`selfTest(true)`).
- `PairingProvider`: `call()` with `status`, `selftest` (`network` extra) or `cleanup`. `openFile("/configure", "w")` reads at most
  2 KiB of JSON into `configure` — the same adb pairing as 0.4.0.

## Speed and size targets

- **Measured on 0.4.0 (2026-10-24, LTE):**
  - activity cold start 222 ms;
  - a cold process for a broadcast or provider call costs about 130 ms more than a warm one;
  - HTTPS to getsecretary.tech takes 74–103 ms on a new connection and 20 ms on a reused one;
  - 0.4.0 makes 2 requests per SMS, even for an SMS without a code;
  - battery: 0.079 mAh in 4 h 23 m.
- **Targets for 1.0.0:**
  - SMS to relay acknowledgement under 250 ms from a cold process, measured with `selftest`, network on;
  - no network call for an SMS without a code;
  - APK under 700 KB;
  - no wakelocks beyond the broadcast's own.

## Measured after the port (2026-10-24, S20 FE on LTE, live relay on p1)

| What | 0.4.0 (Java) | 1.0.0 (Rust + Kotlin) |
|---|---|---|
| Activity cold start (`am start -W`) | 222 ms | 154 ms |
| Parse + `POST /incoming` + ack, cold process (new TLS connection) | about 2 requests, est. 300–450 ms | 112–146 ms (`selftest`, `test:true`) |
| Same, warm process (connection reused) | — | 26–46 ms |
| Network for an SMS without a code | 1 HTTPS request | none |
| Parse on the host, steady state | — | 0.9 µs (OTP SMS), 0.7 µs (plain) |
| APK | 1.86 MB (5.0 MB `.so`, compressed, extracted at install) | 4.2 MB (3.7 MB `.so`, stored, mmapped; no extraction) |
| Relay on p1 | `lucina-device otp-server` | `lucina-otp-relay`, 910 KB static binary, about 0.5 MB RSS |

- **Size:** the 700 KB APK target was dropped on purpose.
  - The `.so` is stored uncompressed (`extractNativeLibs=false`) so it loads by mmap with no extraction at install or first start.
  - Its largest part is graviola's ECDSA verification, about 1.08 MB, which the certificate chain needs.
  - Dropping regex `perf` would save about 375 KB, but steady-state parsing would slow to about 48 µs.
- **Upgrade from 0.4.0:** done in place with the same signing key. RECEIVE_SMS and the Doze allowlist entry were kept. The legacy
  token moved into the encrypted `state.bin`, and the pref was deleted.
- **Not yet verified:** a real carrier SMS, and a notification-sourced code.
