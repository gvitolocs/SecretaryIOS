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
- **Repeats:** a repeated `eventId` within 2 minutes gets `{accepted:true, duplicate:true}`. The relay remembers each id as its
  SHA-256 with the time it is forgotten, in `$LUCINA_OTP_STATE_DIR/seen.json` (0600, written atomically before the 200; if it cannot
  be written the code is refused with 503 and the phone retries). A restart reloads it and skips expired entries, so a retry after a
  restart is still a duplicate.
- **Test events:** `test:true` is validated and answered `{accepted:true, test:true}`, never stored. The phone can measure the whole
  path this way without a fake code reaching the owner.
- **Limits:** at most 8 codes wait, and the oldest is dropped first; at most 64 event ids are remembered.

`/otp` returns the newest code that has not expired. Codes live only in memory, never on disk: a relay restart drops the waiting
codes (their ids stay in `seen.json`). When `LUCINA_OTP_CONTINUOUS=true`, the relay starts armed and continuous unless
`$LUCINA_OTP_STATE_DIR/paused` exists. `/disarm` writes that file and `/arm` removes it.

- **`/arm` keeps waiting codes.** Only `/disarm` (and a timed window ending) drops them; each code still expires at its own
  `expiresAt`. Arming a window that is still live keeps its `windowId` (a protocol-1 phone holding it keeps working; a timed window
  gets the new length from now). Arming a continuous relay that is already armed changes nothing.

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
- **Redaction covers what `parse_otp` reads:** `otp::otp_spans` tokenizes the raw text and the same digit-normalized, link-stripped
  view `parse_otp` uses, mapping those spans back to the original bytes. Codes in Arabic-Indic, Devanagari or full-width digits,
  and codes that only come within 64 bytes of a context word once a link is removed, are masked too; other text is unchanged.

## Lucina OTP app (`tech.getsecretary.lucinaotp`, 1.0.1, versionCode 6)

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
- **Not codes:** an all-digit number next to a currency marker (`1500 DKK`, `DKK 1500`, `$1999`, `kr`, `EUR`, …) or joined to
  another number by `. , : / -` (`1250,00`, `2026-10-25`) is an amount or a date, never a code. `294-728` and `294 728` are codes.
- **Dedup:** the same code from any source within 120 s is sent only once, so an SMS and its notification count as one.
- **Write-ahead (1.0.1):** before the first `POST /incoming`, the code is queued in `queue.bin` with its event id and its dedup
  fingerprint is saved in `state.bin`. The relay's answer then removes the entry (accepted, duplicate, 409, 401, 410, 400), or a
  network error or 5xx leaves it for `flush`, which resends it with the same event id. A reboot or process death during the send
  therefore loses nothing, and the other copy of the code (the notification) is still a duplicate. `init` re-derives the
  fingerprints of queued codes, for a crash between the two writes. The JNI mutex holds a second copy until the first send returns.
- **Network:**
  - one `POST /incoming` (protocol 2) over rustls (graviola provider, webpki roots, host must be `getsecretary.tech`);
  - no redirects; connect timeout 3 s, total 5 s;
  - the response may be at most 4 KiB, and the agent and connection are reused within the process.
  - A 409 means paused: drop the code and set `lastResult=paused`. A 401 means unpaired: drop the code. 410 or 400: drop it.
  - On a network error or a 5xx, the code stays queued.
- **Queue:**
  - stored AES-256-GCM-encrypted in `filesDir/lucina/queue.bin`, holding code, sender, event id, received time and source;
  - each entry lives at most `CODE_TTL_MS` after it was received, since an older code is useless;
  - at most 8 entries; writes are atomic (temp file, fsync, rename); `flush` saves it after each answered entry.
  - **Gap:** Kotlin schedules `RetryJobService` only on a `queued` result and at boot. After a process death mid-send (not a
    reboot), the written-ahead entry waits until the next `queued` result or reboot. Fix in Kotlin (not done in 1.0.1):
    schedule the job in `Core.ensure` when `status().pending > 0`.
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

## Caller screening (Secretary · Lucina 0.7.1, versionCode 10)

Unknown callers are screened with Jev (`POST /v1/callers/screen`, contract in `crates/lucina-phone-core/src/caller.rs` and the server's
`/v1/callers/*`). Rust does the work in `crates/lucina-android-calls/src/caller.rs`; Kotlin only hosts the Android pieces.

| Kotlin | Job |
|---|---|
| `CallerScreenService` (`CallScreeningService`, `BIND_SCREENING_SERVICE`, held through `ROLE_CALL_SCREENING`) | Incoming calls only. Answers ALLOW at once, then shows the heads-up notification. |
| `CallerScreenActivity` (MainActivity's "Callers" button) | Role status and request, the three switches, permissions and the last screened call. |

- **Flow:** `Native.localVerdict` (no network: the shared core's fallback rules, from facts about the number only) posts the first notification;
  `Native.screenCall` asks the server (`call: {source: "android"}`, 1.2 s in total, the WebView cookie read on the
  main thread like the share job) and replaces it. Any error, an answer for another number or an out-of-range probability falls back to
  the rules (`source: "rules"`, marked "quick guess" in the text). Hidden numbers get no notification and no request.
- **Facts only:** the phone sends the number, the language and `call.source`. It makes no assumption about who might call (no shift, delivery or
  customer guesses): the labels come from the number, his contacts, his earlier calls and the server's public caller-ID reports.
- **Auto-reject (the owner's choice, on by default):** the decision is the shared core's `reject_decision`, run in Rust
  (`caller.rs::rejection`); Kotlin has no policy. Default limits: spam >= 0.60 or pick-up <= 0.25, and while an order is active only
  spam >= 0.97 (editable in the Callers screen; Rust refuses anything outside 0..1). Guards, read on the phone at ring time
  (`CallFacts.kt`): contact (READ_CONTACTS), a call in the call log that is outgoing or incoming with duration > 0 (READ_CALL_LOG), the local
  allow list, `TelephonyManager.isEmergencyNumber` plus the core's emergency and short-number floor. **If a check cannot run (permission
  missing, a query throws), nothing is rejected.** The fallback rules never reject (no report facts, `Keep::NoEvidence`).
- **Waiting:** if the rule is on and no guard already puts the call through (`dialer` op `plan`), the answer waits for the server, at most
  1.5 s, then allows. Otherwise the call is allowed at once. The response for a rejection is `setDisallowCall(true).setRejectCall(true).setSkipCallLog(false).setSkipNotification(true)`
  (`ScreenPlan.kt`, host-tested); only one path answers (the first ask; a second ask for the same number gets the same answer).
- **After a rejection:** the log entry (`caller-rejected.json`), a queued `POST /v1/callers/rejected` (outbox with retries, flushed after the
  rejection and whenever the Callers screen opens), and a quiet note (low importance) with the core's `reject_line`. Its main button is
  **Allow & call back** (his tap: allow list, queued `POST /v1/callers/allow`, then `TelecomManager.placeCall`); **Allow** only allows.
  The Callers screen lists recent rejections with the same buttons.
- **Order signal:** Lucina Maps 0.3.5 sends `orderAtMs` on each navigation link (`OrderSignalReceiver`, permission `ORDER_SIGNAL`). It is kept in
  `caller-order.json` and used only to raise the bar for 45 minutes; it is in no request to the server or Jev.
- **Settings from 0.6:** the old silence/reject switches and threshold are dropped; a 0.6 settings file keeps only the label choice.
- **Contacts:** `READ_CONTACTS` is optional (asked in the screen). With it, a caller who is a contact is looked up on the phone only and
  gets no notification and no request; without it every caller is screened.
- **Privacy:** nothing is logged. The last screened call (display number, label) is kept in `caller-last.json` for the screen, 0600.
- **JNI additions:** `callerState`, `callerSettings`, `callerFlush`, `localVerdict`, `screenCall` (12 exports in all, checked by `build.sh`).
- **Role:** `adb shell cmd role add-role-holder android.app.role.CALL_SCREENING tech.getsecretary.secretary` or the screen's button.

## Phone app (Secretary · Lucina 0.7.1, versionCode 10)

Secretary can be the phone app (`ROLE_DIALER`); Samsung Phone stays installed. **Only the owner makes it the default**:
the app asks (`RoleManager.createRequestRoleIntent`), Android shows its own dialog, and he taps. Nothing grants the role over adb unless he says so.

| Piece | Where | Notes |
|---|---|---|
| Role eligibility | manifest | `DIAL` activities (plain, `tel:`), `VIEW tel:`, `CALL_BUTTON`, `LucinaCallService` (`BIND_INCALL_SERVICE`, `IN_CALL_SERVICE_UI`). |
| Dialing | `DialPlan.kt` (pure Kotlin), `Phone.kt` | Every call is `TelecomManager.placeCall`. Host-tested (`android/secretary/test/run.sh`). |
| In-call | `LucinaCallService`, `InCallActivity`, `CallActionReceiver` | Rust `callView` decides rows, buttons, audio routes and the proximity lock; `CallPlan.fallback` does the same without Rust. |
| Log, contacts, voicemail | `CallLogActivity`, `ContactsActivity`, `PhoneEventsReceiver` | Rust `dialer` formats the log, the missed-call and voicemail texts. |
| Jev label on the call | `caller.rs` cache, `dialer` op `label` | `screen()` remembers each verdict by number (300 numbers, 30 days). |

- **Emergency calls:** `DialPlan.plan` asks `TelephonyManager.isEmergencyNumber` (then `PhoneNumberUtils`; 112 and 911 always count) *first*. An
  emergency number goes straight to `TelecomManager.placeCall(tel:<n>)`: no CALL_PHONE request of ours, no confirmation, no Rust, no network, no
  Jev, no journal entry. If Telecom refuses it (not the phone app and no CALL_PHONE), the number is opened ready in the system dialer.
  `InCallActivity` and the notification fall back to `CallPlan.fallback` when the native library fails, and every call-control tap is guarded.
  Check without dialing: `content call --uri content://tech.getsecretary.secretary.calls --method emergency --arg 112`.
- **Incoming:** Telecom adds the call after the screening service answered. `IN_CALL_SERVICE_RINGING` is false, so the system rings and
  the notification channels are silent. Screen off or locked: full-screen intent. Screen in use (another app in the foreground): heads-up with
  **Answer** and **Decline**, which act only on his tap. Nothing answers or rejects by itself; the only automatic rejection is the owner's auto-reject rule (Caller screening).
- **One label, one lookup:** the call screen reads the cached verdict (or the instant rules guess) and refreshes when the screening service
  gets Jev's answer. As the phone app the screening service posts no second heads-up. If Telecom asks it twice for one number within 15 s,
  the second ask waits for the first and gives the same answer.
- **Call waiting:** a second ringing call shows Answer (Telecom holds the first), "End current call & answer" and Decline; held calls get Resume.
- **Audio:** mute, and one button per offered output (earpiece or wired headset, Bluetooth, speaker) from `CallAudioState`; the proximity
  wake lock is held only for a connected or dialing call at the earpiece.
- **Missed calls and voicemail:** `SHOW_MISSED_CALLS_NOTIFICATION` and `SHOW_VOICEMAIL_NOTIFICATION` are handled; opening Recent calls clears them.
  Long-press 1 on the dialpad, or the Voicemail row, calls `voicemail:` through Telecom.
- **Back to Samsung Phone:** the Phone screen's "Use Samsung Phone again" asks Telecom (`ACTION_CHANGE_DEFAULT_DIALER`, the previous
  default remembered) and falls back to Settings > Default apps. adb: `adb shell cmd role add-role-holder android.app.role.DIALER com.samsung.android.dialer`
  (or `adb shell telecom set-default-dialer com.samsung.android.dialer`).
- **Read-only device check:** `android/secretary/test/device-check.sh` (role holders, qualifying components, the Samsung package, the
  emergency dry run). It dials nothing.

### Device test checklist (owner present; nothing is placed, answered or rejected by an agent)

0. Phone not in use for work, USB or wireless adb. `adb install -r secretary-0.7.1.apk`. Run `test/device-check.sh`: version 0.7.1, DIAL activities listed,
   InCallService and CallScreeningService listed, missed/voicemail receivers listed, `112 -> EMERGENCY`, `placed:false`.
1. Open Secretary > Calls > "Make Secretary your phone app". Android shows its dialog: **he taps "Set as default"**. Then
   `adb shell cmd role get-role-holders android.app.role.DIALER` shows `tech.getsecretary.secretary`.
2. Outgoing: he dials a number he chooses (a contact or his other phone). The call screen shows the name or number, "Dialing" then
   "Connected"; mute, speaker/earpiece and Bluetooth (if paired) change the audio; hold/resume; the keypad sends tones; End ends it. The
   screen turns off when the phone is at his ear (earpiece route) and on again when he takes it away. The call appears in Recent calls.
3. Incoming (screen off, locked): the full-screen call screen appears, the system ringtone plays once (no second sound), the Jev label
   shows within about a second, Answer works only when tapped. Decline works only when tapped; it then shows in Recent calls.
4. Incoming with another app in the foreground (the one he works with) and the phone unlocked: a heads-up with Answer and Decline appears over it and the app is not
   interrupted. Tapping Answer answers and leaves that app in front; the ongoing-call notification has Hang up and opens the call screen.
5. Call waiting: during a call, a second call rings. The screen offers Answer, "End current call & answer", Decline. Answer holds the first;
   Resume swaps back; End ends one at a time.
6. Missed call: let one ring out (his other phone). A "Missed call" notification appears with the Jev label for an unknown number;
   opening Recent calls clears it. Tapping a row asks nothing and calls back.
7. Voicemail: long-press 1 calls the carrier voicemail (he hangs up). Voicemail notification if the carrier sends one.
8. Emergency, **without dialing**: type 112 in the dialpad and look at the route only: `... --method emergency --arg 112` reports
   `EMERGENCY` and `TelecomManager.placeCall(tel:112) at once`. **Never press Call on 112.** (Samsung's own dialer can be used to check that the SIM treats 112 as an emergency number; also without pressing Call.)
9. Switch back: "Use Samsung Phone again" (or Settings > Default apps > Phone app) and he confirms. `get-role-holders` shows
   `com.samsung.android.dialer`; an incoming call now rings in Samsung Phone; Secretary still screens it (the call-screening role is separate).
   If the screen does not appear: `adb shell cmd role add-role-holder android.app.role.DIALER com.samsung.android.dialer`.

## Attachments in the chat WebView

Secretary Android 0.4.0 (versionCode 5) lets the chat page attach photos, videos and files. A small
`WebChromeClient` in `MainActivity` handles `<input type=file>`:

- `onShowFileChooser` honours `FileChooserParams`: accept types become an `ACTION_OPEN_DOCUMENT` MIME filter
  (`EXTRA_MIME_TYPES` when there is more than one), `MODE_OPEN_MULTIPLE` sets `EXTRA_ALLOW_MULTIPLE`, and
  `isCaptureEnabled` adds `ACTION_IMAGE_CAPTURE` and/or `ACTION_VIDEO_CAPTURE` to a chooser beside the picker.
- Camera output is written to a pending `MediaStore` content URI, so no `FileProvider` and no `CAMERA`
  permission are needed. The unused camera entry is deleted, the captured entry is published
  (`IS_PENDING = 0`), and the result Uris (including `ClipData` items) are returned through the callback.
  Cancelling returns `null`.
- `ACCESS_FINE_LOCATION` and `ACCESS_COARSE_LOCATION` are declared. A geolocation prompt is granted only for
  the exact `https://getsecretary.tech` origin, after the runtime permission is granted; the grant is not
  retained, so a reload asks again.
- `allowFileAccess` and `allowContentAccess` stay `false`; uploads use the chooser callback, not `file://` or
  `content://` page access. `javaScriptEnabled` and `domStorageEnabled` were already on, and WebView's
  Chromium paste events need no additional setting. The web composer owns paste and drag-and-drop handling.
- The phase-1 `ShareActivity`, Direct Share shortcut and background upload are unchanged.

## Share to Lucina

Secretary Android 0.4.0 (versionCode 5) receives `ACTION_SEND` and `ACTION_SEND_MULTIPLE`, for `*/*` and
`text/plain`, through the exported, dialog-themed `ShareActivity` labeled **Lucina**. It accepts text, links,
photos, videos, PDFs, documents, audio, contacts, locations and files exposed by other apps as content URIs.
The dark framework UI previews snippets, file names and sizes, and available content-provider thumbnails.
The owner can add a note and choose **Send to Lucina** (`ask`), **Save to library**, **Add to shopping**,
**Save as note**, or **Remind me**. Without a WebView session cookie it offers **Open Secretary and sign in first**.

Kotlin only bridges Android APIs: intent extras and ClipData (deduplicated), ContentResolver metadata and
streams, HEIC/HEIF-to-JPEG conversion with ImageDecoder, the views, ShortcutManager and JobScheduler. Files are
copied with a bounded stream buffer into `cacheDir/shares/<UUID>/`; no whole-file byte arrays are created.
A synced `manifest.json` contains the UUID, intent, note, language, subject/title, texts, URLs and staged file
paths/names/MIME types. HTML is a fallback only when plain EXTRA_TEXT is absent. Rust classifies a text that
is a single HTTP(S) URL as a `url` part. Unknown file MIME types use `application/octet-stream`.

After staging, a network-required job is scheduled and the activity closes with **Sent to Lucina**. This means
queued for upload; the reply appears asynchronously in the chat timeline after server acceptance. Each job reads
the current WebView cookie, then calls `Native.shareUpload` on a worker thread. Rust validates canonical paths
under the app's cache shares directory (also refusing `..` and symlinks escaping the share folder), UUID, intent,
note and limits. It streams a multipart reader made of small headers/fields and open files, with a computed
Content-Length, to `POST https://getsecretary.tech/v1/share`, using `Cookie`, `X-Secretary-CSRF: 1`,
`channel=web`, and `source=android-share`. The connect timeout is 10 seconds; the upload timeout is 30 minutes.
Redirects are disabled. No cookie, shared content or server response body enters the local journal; it records
share ID, intent, part count and delivery status. The UUID stays the same across retries for server idempotency.

Limits are **32 multipart parts including metadata**, **2 GB including multipart framing**, and **4000 note
characters**. Rust enforces these before networking. JobScheduler retries network failures, 408, 429 and 5xx
with exponential backoff; other 4xx are permanent, and 401 means sign in again. Staged files are removed after
success or a permanent 4xx failure. No boot permission is added, so jobs are not persisted across reboot; Android
may also reclaim cache storage. Once the server accepts the single request with 202, it owns storage and the
asynchronous Lucina turn even if Android kills the client.

### Direct Share

`res/xml/shortcuts.xml`, linked from MainActivity's `android.app.shortcuts` metadata, declares the share target
and category `tech.getsecretary.secretary.category.SHARE_LUCINA`. On app startup and after every queued share,
Secretary publishes the long-lived dynamic shortcut **Lucina** (id `lucina`, bot Person `Lucina`, app icon,
ACTION_VIEW to MainActivity). It uses `pushDynamicShortcut` on API 30+ and `addDynamicShortcuts` on API 29,
and calls `reportShortcutUsed("lucina")` after each share. Direct Share's EXTRA_SHORTCUT_ID follows the same
share intake as the ordinary chooser target.

The chooser's order is the system's decision and cannot be forced. Direct Share ranks shortcuts using reported
usage and other system signals. The owner can pin Lucina in the Samsung share sheet: long-press **Lucina** →
**Pin**. Build with `bash android/secretary/build.sh`; installation remains the owner's separate
`android/install.sh secretary` step.
