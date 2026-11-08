# lucina-telegram

Minimal async Telegram Bot API client using reqwest/rustls. `BotApi::new(token)` uses
Telegram's API; `with_base_url` supports local fixtures. The client deliberately has
no `Debug` implementation. Transport errors omit request URLs, and API descriptions
are scrubbed of the token. `ApiError` carries the delivery outcome, HTTP/API codes
and optional `retry_after`; it never automatically retries sends.

`send_text` preserves text, splits at 4096 UTF-16 units (newline, space, then a
character boundary), and sends sequentially. Failure after any successful part is
uncertain: retrying the whole text could duplicate the accepted parts. File downloads
are streamed with a caller-supplied byte limit.

`outcome::classify` and `lease_decision::lease_decision` are pure decision functions
kept separate for later Verus mirrors. Unusable success responses and interrupted
response bodies are treated conservatively as uncertain by the transport.

`Lease::acquire` returns `Ok(None)` on contention and an error if the directory or
flock is unusable. It uses `HERMES_GATEWAY_LOCK_DIR`, otherwise
`~/.local/state/hermes-gateway-locks`, and `HERMES_GATEWAY_INSTANCE` (default
`lucina-rs`). The lease holds an exclusive flock and maintains the Node-compatible
JSON record every 30 seconds. Pollers must stop when `lost()` becomes true **or the
watch channel closes**, and retain the lease until polling stops. Dropping it stops
the heartbeat and removes only its own record.

`poller::Poller` is the polling loop on top of it (lease first, no dropped pending
updates, at-most-once claims from `claim.rs` persisted before any handler runs, 409 /
401 / back-off handling, one ordered queue per chat, a shared `PollerHealth`). The
server starts it only with `LUCINA_TELEGRAM_ENABLED=true` (`lucina-server/src/telegram`).

Reminder integration lives in `lucina-server/src/reminder_delivery.rs`. It is enabled
only with `LUCINA_SCHEDULER=true` and `LUCINA_REMINDERS_WRITER=local`; the token comes
from `TELEGRAM_BOT_TOKEN_LUCINA`, then `LUCINA_TELEGRAM_TOKEN_FILE` (default
`~/.config/lucina-rs/telegram-bot-token-lucina.token`). Without a token it warns and
uses the existing timeline sink. Node still owns reminder writes by default.
The initial delay is `LUCINA_REMINDER_INITIAL_DELAY_MS` (15000), with polls every
`LUCINA_REMINDER_POLL_INTERVAL_MS` (60000). The scheduler's last tick, error and
cumulative outcome counters are reported under `/health`'s `scheduler` object.
