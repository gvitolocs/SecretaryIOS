# Widgets, controls and Shortcuts

Owner, 2026-10-26: "Lucina deve avere tanti widget, come ce li hanno anche le altre app… così non dobbiamo sempre aprire
l'applicazione".

## What there is

All of these live in the `SecretaryWidgets` extension (`Extensions/SecretaryWidgets`), next to the voice-memo Live Activity.

| Widget | Home Screen / StandBy | Lock Screen | Tap / buttons |
| --- | --- | --- | --- |
| Ask Lucina | small, medium: her last reply + an "Ask Lucina…" field | circular, rectangular, inline | the chat with the keyboard up (`chat/compose`) |
| Next appointment | small, medium (+ the one after) | circular, rectangular, inline | the in-app place map (`map/place?title=&address=&when=&end=&eventId=`) |
| Shopping list | small, medium (+ deals), large (+ deals) | circular, rectangular, inline | **tick items off in place**; "+" opens the add field; deals open the deals map |
| Today (tasks + reminders) | small, medium, large | circular, rectangular, inline | **complete in place** (Todoist task, or dismisses Lucina's reminder) |
| Weight | small, medium (30-day trend) | circular, rectangular, inline | "+" opens the quick weight sheet |
| Training | small, medium (this week's sessions) | circular gauge, rectangular, inline | Start / Resume a workout |
| Voice memo | small | circular, inline | opens the recorder recording |
| Lucina plays Pokémon | small, medium, large (live screen) | rectangular, inline | the Pokémon plugin |
| Continue watching | small, medium (3 films) | rectangular | plays the film where it stopped |

The shopping widget is configurable (long-press → Edit widget → List).

**Controls** (iOS 18: Control Center, Lock Screen, Action button): Ask Lucina, Add to shopping, Record memo, Log weight.
Each runs `OpenSecretaryIntent`, which opens the app at the route.

**App Shortcuts / Siri** (`Features/Device/SecretaryIntents.swift`): Send to Lucina, Ask Lucina, Add to shopping, Start
workout, Log weight, Record a memo, and now Next appointment, Shopping list and Today's tasks. Each of them also shows up for
the Action button (Settings → Action Button → Shortcut).

## Data path

- `Secretary/Shared/Widgets/` is compiled into the app and the extension:
  - `WidgetSnapshot.swift`: one section per widget;
  - `WidgetStore.swift`: storage;
  - `WidgetFetcher.swift`: the API calls, decoded leniently with JSONValue;
  - `WidgetRoutes.swift`: `secretary://open/<route>` and `OpenSecretaryIntent`.
- The extension fetches on its own with the device token the app already puts in the shared keychain group
  (`BridgeCredentials`). The widgets stay current while the app is closed. Refresh: 15 min (Pokémon while playing) to
  3 h (training), plus at event start, leave time and midnight.
- **Training** comes from the app, because the training engine runs on the iPhone. The app publishes it when it goes to the
  background (`AppModel.publishWidgets`).
- After a change in the app (a synced shopping op, a weigh-in, a sent task command) the app calls `WidgetStore.changed(...)`.
  That marks the section stale and reloads the widget.
- **Storage.** Without the App Group each section is a small JSON item in the shared keychain group (works today, no Xcode
  step). With the App Group it becomes a file in the group container, and the posters and game screen are shared too. A
  section is replaced only by a newer one, and a copy younger than 60 s is reused as is.
- **Media.** `/v1/media/*` goes to the primary URL only, because p1's Caddy serves it and the tailnet fallback is
  secretary-server.
- **Travel time** comes from secretary-maps:
  1. `GET /v1/maps/config` gives home (the widget has no location of its own).
  2. `GET /v1/maps/geocode?q=<address>` finds the event's place.
  3. `GET /v1/maps/routes?from=&to=&modes=drive,transit,walk&arrive=` gives the routes.

  The widget uses the quickest route and the server's `leaveAt` (5 min buffer). A 503 (no Google key) or a missing home
  means the widget shows time and place only.

## Privacy

- Lock Screen and StandBy: everything personal is `.privacySensitive()`, so iOS redacts it while the phone is locked. That
  covers the reply text, the appointment title and place, task titles, the shopping items on the rectangular widget, and the
  weight figures and trend. The trend line becomes a flat placeholder, because shapes don't redact by themselves.
  Counts, times, the Pokémon status and film titles stay visible.
- Signing out deletes every widget section and image (`WidgetStore.removeAll`) and reloads the widgets.

## Testing on the iPhone

- `-SecretaryWidgetSelfTest YES` (Debug):
  - fetches every section the way the extension does, round-trips it through the store and publishes training;
  - opens the routes and checks the screen state;
  - lists the widgets placed on the Home and Lock Screens;
  - results go to `Documents/widget-selftest.txt` (`devicectl device copy from`).
- `-SecretaryWidgetGallery <0-7>` (Debug) draws the widget views in the app at their real sizes from the cached sections,
  for `pymobiledevice3 developer dvt screenshot`. Page 7 is the Lock Screen as iOS shows it while locked.

## App Group (optional, owner)

The widgets work without it. To share files instead of keychain items, do this once in Xcode:

1. Open `ios/Secretary.xcodeproj`.
2. Select the **Secretary** target → Signing & Capabilities → **+ Capability** → **App Groups** → **+** →
   `group.tech.getsecretary.shared` (team 2N4DA9L8VH).
3. Tick the same group on the **SecretaryWidgets** and **SecretaryShare** targets.
4. Then add `com.apple.security.application-groups` = [`group.tech.getsecretary.shared`] to
   `Secretary/Secretary.entitlements`, `Extensions/SecretaryWidgets/SecretaryWidgets.entitlements` and
   `Extensions/SecretaryShare/SecretaryShare.entitlements`.

Nothing else changes: `WidgetStore` switches to the group container by itself.
