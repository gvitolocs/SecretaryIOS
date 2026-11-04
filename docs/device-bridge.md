# Device bridge: Lucina ↔ the owner's phone

The owner wants Lucina to know what happens on their devices and to act on them: alarms, timers, reminders, calendar
and more. Everything stays private: device data goes only to the owner's own server and Lucina, and never to third parties.

## What iOS allows (honest limits)
| Want | Possible? | How |
|---|---|---|
| Set/delete alarms and timers | ✅ (our own, shown like Clock alarms) | **AlarmKit** (iOS 26). It can't touch alarms created in Apple's Clock app. |
| Reminders and calendar: read, create, delete | ✅ | **EventKit**, full access |
| Health: workouts, weight, steps, sleep | ✅ | **HealthKit** (read/write). Feeds the Gym and Weight modules. |
| Location, places visited | ✅ | **CoreLocation**: significant changes and visits |
| Websites visited | ◐ Safari only | **Safari Web Extension**, enabled by the user for all sites. It reports domain, title and time. Other browsers are not visible. On the Mac, the Chrome extension or browser history with permission. |
| Time per app (Screen Time) | ◐ | Only DeviceActivity, inside its sandbox. The numbers **cannot be sent** to the app or server. We can show a Screen Time report inside the app, and set thresholds that notify Lucina ("over 1 h of Instagram"). On the Mac, the knowledgeC / Screen Time DB is readable with Full Disk Access. |
| Other apps' content (messages, other browsers) | ❌ | iOS sandbox. Not possible for any App Store or sideloaded app. |
| Siri / Shortcuts / Action button | ✅ | **App Intents**: "Ask Lucina", "Add to shopping", "Start workout", "Log weight". Shortcuts automations can forward events (e.g. "when I open app X"). |
| Focus status, notifications | ✅ | Focus filter intents; user notifications |
| Contacts, photos | ✅ (with permission) | Contacts / PhotoKit, on demand only |

## Data flow
1. **Phone → server:** `POST /v1/device/events {events:[…]}` (Bearer device token, batched, offline-queued). Each event:
   `{id, ts, type, source:"ios"|"mac"|"safari-ext"|"chrome-ext", data}`.
   - `type` is one of `web_visit`, `location_visit`, `health_summary`, `workout`, `focus_change`,
     `screen_threshold`, `calendar_change`, `reminder_change`, `alarm_fired`, `app_open` (via Shortcuts).
2. **Server → Lucina:** `POST /api/secretary/device/events` stores the events in Lucina's device-context store
   (`data/device-context.jsonl`, retention configurable, default 90 days). Lucina summarises them into Honcho memory (daily
   digest, not raw events) and uses recent context in replies ("you spent the morning at the gym…").
3. **Lucina → phone (commands):** Lucina emits a device command as a timeline item with
   `meta.kind:"device_command"`, `meta.command:{id, type, args}`.
   - **Types:** `alarm.set {time, label, repeat?}`, `alarm.delete {id}`, `timer.start {seconds, label}`, `timer.cancel {id}`,
     `reminder.create {title, due?, list?}`, `reminder.complete|delete {id}`, `calendar.create {title, start, end, …}`,
     `calendar.delete {id}`.
   - The phone receives it over the timeline stream (or a push), executes it with the matching framework, and acks with
     `POST /v1/device/commands/{id}/result {ok, error?, deviceObjectId?}`. Lucina then confirms in the chat.
   - **Safety:** deletes and anything outside alarms, timers and reminders need an in-app confirmation tap. Commands expire
     after 10 minutes. There are no purchases and no messages to third parties.
4. **Lucina's knowledge of device state:** the phone periodically reports `device_state`: its alarms, timers, upcoming
   reminders and events, and the Focus mode. "Delete my 7:00 alarm" then resolves to the right id.
5. **Juno, the owner's laptop (Lucina's `juno_*` tools):** `lucina-devd` on the laptop dials out; no port listens there.
   - `GET /v1/device/juno/next?wait=<s>` (device token, `wait` 0..=25, default 25) long-polls Lucina's
     `/api/secretary/device/juno/next` for the next call: `{"call": null}` or `{"call": {id, tool, arguments, deadlineMs}}`.
   - `POST /v1/device/juno/calls/{id}/result {ok, result?, error?}` posts its answer (body up to 12 MiB, a base64 PNG
     screenshot). An unknown or expired id, or one queued for another device: 404 `unknown_call`.
   - Both stamp `device: {id, name}` from the token; a body's own `device` is ignored.
   - `GET /v1/juno/screenshots/{id}.png`: a screenshot she took, as her reply's image block shows it (kept an hour, last 8).
   - Lucina (`crates/lucina-server/src/juno.rs`) keeps a queue per device: a call nobody picks up in 10 s, or with no
     poll in the last 40 s, fails as "Juno is offline". A Juno Chat turn (session `juno-<device id>`) targets that
     laptop, any other turn the one that polled last. `juno_call` asks the owner first for `delete_file`, `write_file`
     with `overwrite` and `wifi_connect`, and never sends reloads, installs, power or disk tools. The tools are in the
     owner's registry only (never MCP turns); `LUCINA_JUNO_TOOLS=false` turns them off. Each call is logged as
     `juno_audit` (device, tool, a summary without payloads, ok, ms).

## Multi-language
The app follows the device language by default: iOS `Locale.preferredLanguages`, and on the web `navigator.languages`.
Italian, English and Danish to start, with an override in Settings. Lucina replies in the language you write in.

## Calendar reach of the snapshot

`POST /v1/device/state` carries EventKit events from 3 days back to 120 days ahead, at most 300, with notes
only for the next 14 days. The app keeps the body under the route's 256 KB limit by dropping notes, then the
farthest events. Accepted proposals and events Lucina creates go to her own calendar (`lucina:` ids) in
Secretary: **they do not reach the iPhone's Calendar app**, which only `calendar.create` device commands write.
