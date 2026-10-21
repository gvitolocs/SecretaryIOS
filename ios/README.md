# ios/

The native SwiftUI app for Secretary: a thin client of `secretary-server`'s
`/v1` API (`../docs/server-api.md`). Product logic stays in the Rust engines and
in Lucina; the app sends requests and renders the views it gets back.

- iOS 17+, bundle id `tech.getsecretary.app`, no third-party packages.
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
- Push notifications for reminders (APNs, server side + a device-registration route).
- `LucinaStage` (the animated avatar with voice) in the slot above the chat transcript.
- TestFlight, which needs the owner's paid Apple developer team.
