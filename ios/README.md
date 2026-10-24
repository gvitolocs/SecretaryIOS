# ios/

The native SwiftUI app for Secretary: a thin client of `secretary-server`'s
`/v1` API (`../docs/server-api.md`). Product logic stays in the Rust engines and
in Lucina; the app sends requests and renders the views it gets back.

- iOS 17+, bundle id `tech.getsecretary.app`. One Swift package: the Google Maps SDK
  (`GoogleMaps`, exact tag in `project.yml`), for the grocery deals map on the Shopping tab.
- Tabs: **Chat** (Lucina, with job cards), **Shopping**, **Gym** (the LucinaGym
  screens), **Weight**, **Settings**.
- Auth: email + password (`POST /v1/auth/device`) or a `sec_…` device key. The
  token lives in the Keychain (`afterFirstUnlockThisDeviceOnly`) and is sent as
  `Authorization: Bearer`.
- Server: `https://getsecretary.tech`. Debug builds accept
  `-SecretaryBaseURL http://localhost:8791` and have a hidden developer row in
  Settings (tap Version five times). ATS allows plain HTTP only to local hosts.

## Layout

```
project.yml                 xcodegen spec (targets Secretary, SecretaryTests, SecretaryUITests)
Secretary/App/              AppModel (session, token, banner), RootView, tabs
Secretary/Core/             APIClient, APIError, Keychain, ServerConfig, engine protocol, wire types
Secretary/Features/Chat/    ChatStore (persisted, job polling), ChatView = stage + transcript + composer
Secretary/Features/Shopping/ loose item normalisation, optimistic toggles
Secretary/Features/Gym/     GymTransport (HTTP now, FFI later), GymStore, ported LucinaGym screens
Secretary/Features/Weight/  BodyClient.swift (all body wire knowledge), WeightStore, WeightView
Secretary/Features/Settings/ health, device, sign out, change password
scripts/run-mock.sh         runs the web mock with an iOS overlay (Bearer, /v1/auth/device)
Resources/…/AppIcon        the Lucina portrait (personal fan art: private builds only, never the App Store)
screenshots/                every tab, light and dark
```

## Build and test

The gym runs the Rust engine on the device, so build the static library first
(`ios/Frameworks/`, gitignored; needs the `aarch64-apple-ios` and `-sim` targets):

```sh
scripts/build-ios-core.sh        # from the repo root: SecretaryCore.xcframework
cd ios
xcodegen generate
SIM=05958E3F-AAC6-4F62-9E00-B318CF024A11   # "Secretary iPhone 17"

# compile check, unsigned
xcodebuild -project Secretary.xcodeproj -scheme Secretary -destination "id=$SIM" \
  -derivedDataPath build CODE_SIGNING_ALLOWED=NO build

# tests: the Keychain needs at least ad-hoc signing on the simulator
# (an unsigned build gets errSecMissingEntitlement, -34018)
scripts/run-mock.sh start
xcodebuild -project Secretary.xcodeproj -scheme Secretary -destination "id=$SIM" \
  -derivedDataPath build CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM= test
```

`SecretaryUnit` runs only the unit tests. UI tests sign in to the mock as
`owner@example.test` / `dev-password-123` (or with the access key `dev`).

## On a device

```sh
xcodebuild -project Secretary.xcodeproj -scheme Secretary -configuration Release \
  -destination 'platform=iOS,name=<device name>' -derivedDataPath build/device \
  -allowProvisioningUpdates DEVELOPMENT_TEAM=<team id> CODE_SIGN_STYLE=Automatic build
xcrun devicectl device install app --device <devicectl id> build/device/Build/Products/Release-iphoneos/Secretary.app
xcrun devicectl device process launch --device <devicectl id> tech.getsecretary.app
```

With a free (personal) team the provisioning profile expires after 7 days;
rebuild and reinstall then. TestFlight needs a paid Apple Developer team.

## The mock

`scripts/run-mock.sh start|stop|restart` copies `web/mock/server.mjs` (from
nezopt over SSH, or the local clone) into `ios/.mock/` (gitignored) and inserts
`scripts/mock-overlay.js` when the mock lacks it: Bearer device tokens on every
route (CSRF-exempt, as on the real server), `POST /v1/auth/device` and
`POST /v1/session/password`. The patched copy is never committed.

## Next

- Gym is local-first: `FFIGymTransport` runs `secretary-ffi` on `Application Support/Secretary/gym.json`
  (offline, like the original LucinaGym). Gym settings can move training to the server engine, and
  upload/download the whole history (`export_json` → `restore_json`). Live op replay between the two
  engines is not possible as is: each engine creates its own workout/set ids, so a command recorded on
  one refers to ids the other never made. Two-way sync needs engine-side id assignment from the client
  (or an id map) first.
- A body-weight `FFIBodyTransport` the same way (`secretary_body_*` is already in the library).
- `LucinaStage` (the animated avatar with voice) in the slot above the chat transcript.
- TestFlight, which needs the owner's paid Apple developer team.

## Notifications (docs/notifications.md)

- The bell in the navigation bar opens the in-app notification center (the server inbox:
  mark read, swipe, tap opens the right tab). Settings → Notifications mirrors the web
  settings (categories, quiet hours, Do Not Disturb, previews) plus this iPhone's switch.
- Local notifications (`Features/Notifications`): new inbox entries while the app runs or
  refreshes in the background (`BGAppRefreshTask` `tech.getsecretary.app.refresh`), Lucina's
  timeline messages when the chat is not in view, and Lucina's reminders and calendar events
  scheduled ahead so they fire with the app closed.
- APNs is built but off (`SecretaryAPNsEnabled: false`, no `aps-environment`): the free
  Personal Team cannot use it. Until a paid team exists, the home-screen web app gives real
  push on the iPhone (Web Push, iOS 16.4+).

## Device bridge (docs/device-bridge.md)

Settings → Lucina's access turns each source on, with an explanation before iOS asks:

| Source | How | Notes |
|---|---|---|
| Alarms, timers | AlarmKit (iOS 26) | Only alarms Secretary made; Clock's own alarms are out of reach. |
| Reminders, calendar | EventKit, full access | Deletes and calendar changes need a tap in the app. |
| Health | HealthKit | Weight → Weight tab; weigh-ins and gym workouts → Health; daily summary and workouts → Lucina. |
| Places | CoreLocation visits | Needs "Always". Coordinates rounded to ~10 m. |
| Focus | Focus filter intent | Add "Tell Lucina" to a Focus in iOS Settings. |
| Safari | Safari Web Extension (`Extensions/SecretarySafari`) | Domain + title, once per domain per 5 min; the user enables it in Safari. |
| Screen Time | DeviceActivity report + monitor extensions | Report shown in-app; only the name of a passed limit reaches Lucina. |

Siri / Shortcuts: Ask Lucina, Add to shopping, Start workout, Log weight.

Commands arrive as timeline items (`meta.kind: "device_command"`), are run once, acked with
`POST /v1/device/commands/{id}/result`, expire after 10 minutes, and are ignored when they
predate this install or sign-in. Events queue on disk until `POST /v1/device/events` answers 200.

Extensions reach the server themselves: the app writes the token and server URLs to the
shared keychain group (`$(AppIdentifierPrefix)tech.getsecretary.shared`) only while Safari or
Screen Time is on.

### Signing on the free Personal Team (tested 2026-10-21)

- Signs and installs: HealthKit, Family Controls (development), keychain sharing, Siri,
  AlarmKit, EventKit, location, the Safari extension and both DeviceActivity extensions.
- Refused: **App Groups** ("profile doesn't match the application-groups entitlement"), so
  nothing here relies on them.
- Needs the paid Apple Developer Program: Family Controls **distribution** (Apple must
  approve it for TestFlight / App Store), TestFlight itself, push notifications (APNs), and
  profiles longer than 7 days.
