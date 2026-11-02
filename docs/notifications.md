# Notifications

Lucina can reach the owner outside the chat: a reminder that is due, a reply
that came while he was away, a calendar event about to start, a device command
waiting for his tap. This document covers how that works on the server, the
website and the iPhone app, what each iPhone option gives today, and what stays
private.

## Architecture

```
   Lucina (Node, live)                     secretary-server (p1)                       devices
 ┌──────────────────────┐   timeline SSE   ┌──────────────────────────────┐  Web Push   ┌───────────────────┐
 │ timeline.jsonl ──────┼─────────────────▶│ ingest ─┐                    │ (VAPID,     │ browsers, iPhone  │
 │ reminders, check-ins │  POST /v1/notify │         ▼                    │  aes128gcm) │ home-screen app   │
 │ (LUCINA_APP_NOTIFY) ─┼─────────────────▶│ publish: dedup → switch →    ├────────────▶│ (sw.js)           │
 │ Rust: `notify` tool ─┼─────────────────▶│  presence → quiet/DND →      │             └───────────────────┘
 └──────────────────────┘   service token  │  inbox + push targets        │  APNs       ┌───────────────────┐
                                           │ tick: calendar, digest,      ├ ─ ─ ─ ─ ─ ─▶│ native app        │
   phone ── POST /v1/device/state ────────▶│  "Lucina offline"            │             │ (polls the inbox, │
   apps ─── POST /v1/notifications/presence│                              │◀────────────│ local notifs)     │
                                           └──────────────────────────────┘  GET inbox  └───────────────────┘
```

**Producers**

| Source | Category | How |
|---|---|---|
| Lucina's timeline (any channel) | `lucina_message` | The server keeps one SSE stream open to Lucina (`notify::ingest`) with its own token; every `role: lucina` item becomes a notification unless the owner has the app in front, or it is her chat with an agent (below). |
| Delivered reminder | `reminder` | Timeline items with `meta.kind: "reminder"`, and Lucina's `POST /v1/notify` when `LUCINA_APP_NOTIFY=true` (deduplicated: same timeline id, or `reminder:<id>`). |
| Device command that needs a tap | `device_confirm` | Timeline items with `meta.kind: "device_command"` and `meta.command.requiresConfirmation` (else the iOS rule: deletes and calendar changes). |
| Coding agent finished, needs permission, or the Infisical broker asks | `agent` | lucina-agentd's `POST /v1/notify` (`data: {kind, agentId or permissionId, reason, requestId, actions}`); while an app is in front it stays in the inbox without a push (Paseo's rule). |
| Lucina changed a list | `shopping` | Timeline items with `meta.kind: "shopping"`, or `POST /v1/notify`. |
| Calendar | `calendar` | The phone's `POST /v1/device/state` carries the next week of events; the server's tick notifies `calendarLeadMinutes` before each timed event. |
| Proactive check-in / nudge | `lucina_message` | `POST /v1/notify` from Lucina (`LUCINA_APP_NOTIFY`), per-day dedup key. |
| Lucina unreachable 5 min / back | `presence` (off by default) | The ingester's connection state. |
| New device signed in | `system` (off by default) | `POST /v1/auth/device`. |

**Her chats with agents make no alert** (owner, 2026-11-02: "just messages for me"). A timeline item with
`meta.via` set (a turn through an agent link, any provider or caller) or `meta.kind: "reach"` (an agent's reply
to her) makes no notification: no push, no inbox entry, nothing on the unread badge, no local alert on iOS. It
stays readable in the chat. The text does not matter: naming the owner changes nothing. Reminders, device
commands and shopping changes made in such a turn are still his. A new agent link stays silent by recording
its turns with `via` and posting nothing to `/v1/notify`. An agent that needs him goes through `lucina_decide`,
an approval, or asks Lucina to tell him (her `notify` tool); those have their own push. A run that Lucina or
another agent started (labels `from: lucina`, `lucina.origin`, `lucina.parent-agent-id`) pushes nothing when it
finishes, and a job or delegation an agent asked for pushes nothing either. Permission requests always do.

**Pipeline** (`Notifier::publish`): dedup (explicit `dedupKey`, or `tl:<timeline id>`)
→ category switch → presence (a Lucina message while any app reported itself in
front within 90 s is dropped) → quiet hours / Do Not Disturb (stored and flagged
for the digest, not pushed; reminders pass when `remindersBypassQuiet`) → inbox
→ push to every subscription the per-device settings allow.

**Inbox:** `notifications.json` in the data dir, newest 500 (read ones dropped
first), with read state. `GET /v1/notifications`, `POST /v1/notifications/read`.

**Settings** (`notification-settings.json`): per-category switches, quiet hours
(local time in `timeZone`), `dndUntil`, `calendarLeadMinutes`,
`remindersBypassQuiet`, `digest`, `previews`, `language`, and `devices`: per
push subscription (`ps_…`) or native device (`dev_…`) `{enabled, categories}`.

**Digest:** while quiet, notifications are stored but not pushed; when quiet
hours or DND end, the tick sends one push per device: "While you were away —
Lucina: 3 · Reminder: 1". Bursts of Lucina messages share one Web Push `Topic`
and notification `tag` (`lucina`), so the lock screen shows one entry titled
"3 messages from Lucina".

**Web Push:** VAPID keys are generated on first start into `vapid.json` (0600).
Payloads are encrypted with RFC 8291 `aes128gcm` (`web-push-native` 0.5.0, pure
Rust), the VAPID JWT (RFC 8292) is signed with `p256`. Endpoints must be HTTPS on
a known push service (FCM, Mozilla, Apple, WNS; `SECRETARY_PUSH_HOSTS`).
Delivery retries 429/5xx/network errors (2 s, 15 s, 60 s, honouring
`Retry-After`), drops the subscription on 404/410, and after 10 failures in a row.

**APNs adapter** (`notify::apns`): ES256 provider JWT (refreshed every 50 min),
HTTP/2 to `api(.sandbox).push.apple.com`, collapse ids, dead tokens removed.
On with `SECRETARY_APNS_TEAM_ID` and a key installed with Add secret, or with
`SECRETARY_APNS_KEY_FILE`, `_KEY_ID` and `_TEAM_ID` together. While off,
`POST /v1/push/apns` still keeps the token (`apnsEnabled: false`) and pushes to
it are skipped without counting as failures.

## Website (PWA)

- The bell in the header shows the unread count; the panel lists notifications
  by day and opens their route (`/`, `/shopping`, …) when clicked.
- Account → Notification settings: everything above, plus this browser's push
  (enable, test, turn off) and the device list (mute, remove).
- `manifest.webmanifest` + `/sw.js` make it installable. The service worker shows
  pushes, routes clicks to the app (app paths only), re-subscribes on
  `pushsubscriptionchange`, and caches only static files (`/assets/*`, icons,
  the last good `index.html`); it never touches `/v1`.
- Permission is asked only from the "Enable notifications" click.
- While a window is visible and focused the app sends a presence heartbeat
  every 45 s, so Lucina's messages don't buzz the other devices meanwhile.
- CSP: `worker-src 'self'` (already there for the stage's workers) admits
  `/sw.js`; `manifest-src 'self'` is explicit. Tested in `tests/api.rs`.

## iPhone: three ways

| | Works now? | Closed app | Needs |
|---|---|---|---|
| **Home-screen web app (Web Push)** | Yes | **Yes**: real push for every category | iOS 16.4+, the site added to the Home Screen |
| **Native app, local notifications** | Yes | Reminders and calendar events (scheduled ahead); the rest on background refresh (iOS decides when, roughly every 15 min to hours) and while open | Notification permission |
| **Native app, APNs** | No | Yes, instantly | Paid Apple Developer Program team + APNs key |

### Web Push on the iPhone, step by step

1. Open `https://getsecretary.tech` in **Safari** and sign in.
2. Share → **Add to Home Screen** → Add.
3. Open **Secretary from the Home Screen** (not from Safari: Web Push only works
   in the installed web app).
4. Tap the bell → **Enable notifications** (or Account → Notification settings),
   then **Allow** in the iOS prompt.
5. Account → Notification settings → **Send a test**. It arrives in a few seconds,
   also with the app closed and the phone locked.
6. If the native app is installed too, turn off "Notify this iPhone" in one of
   them, or turn off `reminder` and `calendar` for the web app's device entry,
   so the same reminder does not arrive twice.

iOS settings for it live under Settings → Notifications → Secretary (the web app).

### The native app now (free Personal Team)

- **In-app notification center:** the bell in the navigation bar opens the
  server inbox (mark read, swipe, tap to open the right tab).
- **Local notifications:** while the app runs or refreshes in the background
  (`BGAppRefreshTask`, `tech.getsecretary.app.refresh`), new inbox entries are
  shown once each; Lucina's timeline messages alert at once when the chat is not
  in view. A first sync never replays the backlog.
- **Scheduled:** Lucina's pending reminders (`GET /v1/reminders`) and the
  calendar's timed events (EventKit, when calendar access is on) are scheduled
  as local notifications, so they fire with the app closed (at most 40 + 20,
  iOS keeps 64 pending). Their inbox copies are not shown again.
- **Settings → Notifications** mirrors the web settings (same server document)
  and adds this iPhone's switch and the iOS permission.

### Live Activity for approvals

With APNs on, a new approval request also starts a **Live Activity** on the
owner's iPhone (push-to-start, also with the app closed); the time-sensitive
notification stays as it is. The lock screen shows who asks, the reason, the
first 3 command lines (monospace, "+N more"), the time left, **Deny** and
**Approve** side by side, and a **Details** link that opens the approval card.
The Dynamic Island shows the same, compact. While the iPhone is locked the
reason and the commands are redacted (who asks, the time left and the buttons
stay); with notification previews off, an activity the app starts itself says
only "Open Secretary to see what it is."

- **Deny** works without unlocking (like the widget)
  (`DenyApprovalActivityIntent`, unchanged).
- **Approve** is an intent button too (`ApproveApprovalActivityIntent`,
  `requiresLocalDeviceAuthentication`): iOS asks Face ID or the passcode on the
  Lock Screen before it runs. The app then fetches the request and does nothing
  more if it is decided (by this tap too: a repeat is a no-op) or expired. If its
  `payloadHash` differs from the card's, the app opens on the new request
  without asking. With **one-tap Approve** ready (below) it signs with the
  unlock key and answers in the background: nothing opens, the card ends
  "Approved". A plain request without the unlock key is answered unsigned, as
  the notification's Approve does. Otherwise (no unlock key or switched off, an
  approver change, or the unlock key's answer failing) the app comes to the
  front on the card and asks Face ID there (`continueInForeground`, iOS 26
  `.foreground(.dynamic)`). If it can't come to the front, "Open Secretary to
  approve" is posted. A second tap while one is on its way does nothing.
- **Tap log:** each card intent logs `card tap intent=<Deny|Approve> id=<id>`
  first thing, and each notification action `notification action=<id> id=<id>`
  (subsystem `tech.getsecretary.app`, category `ApprovalActivity`). The outcome
  follows as `tap outcome id=<id>: …` (`decided approve via unlock key`,
  `decided deny`, `opened card …`, `refused: <why>`; categories `Approvals` and
  `ApprovalActivity`). A tap that reaches the wrong intent shows here.
  On iOS 27.2 an earlier two-button card looked like Deny ran Approve, which is
  why the card briefly had an Approve link. Those were deliberate Approve taps.
  Cards from then still carry `secretary://open/approvals/<id>?approve=1&hash=…`,
  and the app still opens those on the card with its approve step.
- Every root approval is a real Face ID (or passcode) prompt. An earlier build
  reused the Face ID unlock of the iPhone instead; the Secure Enclave key
  refuses such a context (CryptoTokenKit -5), so that is gone, and so is the
  "Require Face ID or passcode for root approvals" setting.

**The notification** follows the same rule. Deny answers in place, without
unlocking. A root plan whose push body carries every command uncut (category
`APPROVAL_SENSITIVE_INLINE`; the expanded notification, where the buttons are,
shows the whole body) has **Approve** (Face ID icon): it unlocks, brings the
app to the front on the card and asks Face ID at once, provided the request is
still pending, unexpired and has the push's `payloadHash`; otherwise the card
opens without asking. A plan that does not fit (over 2400 bytes, or cut in the
inbox), or previews off, keeps `APPROVAL_SENSITIVE`, whose "Review and
approve" opens the card for the owner to read and tap Approve there.

**One-tap Approve** (Settings → Approvals → "One-tap Approve from
notifications", on by default). With it, that `APPROVAL_SENSITIVE_INLINE`
Approve (`APPROVAL_APPROVE_IN_PLACE`, checkmark icon) finishes in place. iOS
asks Face ID or the passcode to run it, no app opens, and there is no second
prompt. This takes a second Secure Enclave key, the **unlock key**
(`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, `.privateKeyUsage` only), which
signs whenever the iPhone is unlocked. The Face ID key
(`.privateKeyUsage, .userPresence`) can't be used from there: iOS can't show its
prompt from a background action, and the Secure Enclave refused a reused unlock
(CryptoTokenKit -5).
- **The trade-off**, accepted by the owner: anyone who can unlock the iPhone can
  approve a root command from the notification with one tap.
- **Enrollment:** the app makes the unlock key and the Face ID key signs it once
  ("Allow one-tap Approve"). This happens when the switch is turned on, the
  first time the app is in front with a Face ID key and no unlock key, and after
  "Set up Face ID again". The server then accepts it
  (`POST /v1/approvals/unlock-key`, see docs/server-api.md).
- **Turning it off** deletes the key on the server, then on the iPhone.
- **Who refuses it:** the server never accepts it for adding approver devices,
  and drops it when the Face ID key is removed, revoked or replaced. The app
  then deletes its copy too, and the notification goes back to the card's
  Approve.
- **The tap itself:** the app fetches the request and signs it with the unlock
  key only (never the Face ID key). Nothing happens if the request is decided or
  expired. A request that changed since the notification, or one-tap not being
  ready, gets "Open Secretary to approve". A signing or server failure gets
  "Your answer didn't go through". The server records it as
  `verifiedBy: unlock_key`, `biometric: false`,
  "unlock key (notification/card) on iPhone".
- **Repeats:** a repeated answer from the same iPhone is a 200 (`repeated`), so
  an action iOS runs twice succeeds without a second prompt.
- **Unchanged:** `APPROVAL` (plain) and `APPROVAL_SENSITIVE` are unchanged. The
  Live Activity card's Approve uses the same unlock key (above).

When the request is answered anywhere, withdrawn or expires, the server ends
the activity (an `end` push to its update token); the app also ends any
activity whose request is no longer pending whenever it refreshes the list.
With the app in front, a new request gets a local activity if none came by
push. Tokens: `POST /v1/push/liveactivity`, kept in
`live-activity-tokens.json` (0600); pushes go out as
`apns-push-type: liveactivity` to `<topic>.push-type.liveactivity`.

### APNs with a paid account

What it unlocks: instant push to the native app for every category, with the
app closed, without the home-screen web app; badge counts from the server;
TestFlight; profiles that do not expire after 7 days.

1. Join the Apple Developer Program (the owner's team).
2. In Certificates, IDs & Profiles enable **Push Notifications** for
   `tech.getsecretary.app`, and create an **APNs auth key** (Keys → +, "Apple
   Push Notifications service"). Download `AuthKey_<KEYID>.p8` once.
3. iOS app: add `aps-environment` (`development` for Xcode builds, `production`
   for TestFlight) to `ios/Secretary/Secretary.entitlements`, set
   `SecretaryAPNsEnabled: true` in `ios/project.yml`, `xcodegen generate`, build.
   The app then registers and sends its token to `POST /v1/push/apns`; the
   server keeps it even before a key exists.
4. Server (p1): `secretary.env` has `SECRETARY_APNS_TEAM_ID=2N4DA9L8VH` and
   `SECRETARY_APNS_ENV=sandbox` (Xcode builds; `production` for TestFlight).
   The owner adds the key in the app with **Add secret**: name
   `APNS_AUTH_KEY_<KEYID>`, value = the whole `.p8` file text. The server
   installs it in its data dir (`apns/AuthKey_<KEYID>.p8`, 0600) and push is on
   at once: no restart, no file copying, no app relaunch.

Note: the Mac has `~/Downloads/AuthKey_FP8MDH6T64.p8` (key id `FP8MDH6T64`). It
may belong to another team (e.g. Pokoin): check in the developer portal which
team owns key `FP8MDH6T64` and that it is enabled for APNs before using it. It
was not used or read for this work.

## Privacy

- Notifications are stored only on the owner's server (`notifications.json`,
  0600) and on his devices. Lucina's server sees nothing new.
- Web Push payloads are end-to-end encrypted (RFC 8291): Apple, Google or
  Mozilla carry them but cannot read them. They do see that a push was sent,
  when, its size, and the `Topic`/`Urgency` headers (`lucina`, `shopping`,
  `high`/`normal`), never the text.
- **Previews off** (`previews: false`) sends "Open Secretary to read it." instead
  of the message, so nothing private shows on the lock screen or leaves the
  server, even encrypted. The iOS app honours it for its local notifications.
- The service worker caches only static files, never `/v1` responses.
- Push subscription secrets (`auth`) stay on the server; the API shows only the
  push service's host and a label.
- `POST /v1/notify` needs the service token (`svc_…`, 0600 file on both sides);
  device tokens and cookies cannot post notifications, and the service token
  cannot read the inbox.
- Logs never contain notification text (Node bridge and server).
- Endpoints are limited to known push services, so a subscription cannot make
  the server call an arbitrary URL; redirects are not followed.
- APNs (when enabled) shows the text to Apple's push service only if previews
  are on, like any APNs alert.

## Environment

| Where | Variable | |
|---|---|---|
| secretary-server | `SECRETARY_SERVICE_TOKEN_FILE` | already used for insights; Lucina's `/v1/notify` uses the same token |
| | `SECRETARY_NOTIFY_INGEST` | `1` (default) / `0` |
| | `SECRETARY_VAPID_SUBJECT` | default `https://getsecretary.tech` |
| | `SECRETARY_PUSH_HOSTS` | default FCM, Mozilla, Apple, WNS |
| | `SECRETARY_PUSH_ALLOW_HTTP` | `0`; `1` only for local mock push services |
| | `SECRETARY_APNS_TEAM_ID`, `_TOPIC`, `_ENV` | APNs, paid team only; the key comes from Add secret |
| | `SECRETARY_APNS_KEY_FILE`, `_KEY_ID` | optional: a fixed key file instead (all three together) |
| Lucina (Node) | `LUCINA_APP_NOTIFY` | `true` to post reminders and proactive messages |
| | `LUCINA_SECRETARY_SERVER_URL` | `https://getsecretary.tech` (Lucina runs on nezopt; the server is on p1) |
| | `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` | file with the `svc_…` token (0600) |
| Lucina (Rust) | same two `LUCINA_SECRETARY_*` | the `notify` tool |
