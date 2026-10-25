# Share to Lucina — implementation handoff

Implemented on `share-ios`, against the 2026-10-25 v1 share contract. No server, Android or web files changed. No new application dependencies.

## Behavior

- `SecretaryShare` appears as **Lucina**, accepts the supplied broad activation predicate, and handles ordinary shares and send-message suggestions through the same input-item path.
- The dark sheet previews every selected item, accepts a 4000-character note, and offers ask/library/shopping/note/remind. Files use Quick Look thumbnails; link metadata has a three-second timeout. Signed-out, empty, oversized and excessive-part payloads show errors.
- Provider temporary files are copied inside their callback into the App Group. HEIC/HEIF is converted using ImageIO to JPEG at quality 0.9 with orientation metadata preserved. Multipart files are streamed in 256 KiB chunks; large documents/movies never use `Data(contentsOf:)`.
- One initial background upload is enqueued with a UUID share ID. The extension displays “Sent ✓” for 600 ms and completes without awaiting the network. JSON metadata contains endpoints, creation time, intent, attempts and boundary; the token stays in the keychain.
- Share background-session identifiers have their own AppDelegate route. Terminal HTTP responses remove the body and record. Retryable failures keep them. Launch/foreground scans retry records older than two minutes, cancel/reconcile their background tasks, and stream foreground uploads through the endpoint list. A retry pass counts as one attempt; five failed passes including the initial send terminate the share and request a local failure notification. Leaving the foreground cancels its retry without consuming an attempt. Success refreshes the foreground chat, and Settings displays the last terminal result.
- The app always shares credentials for explicit `share` use while signed in. Safari and Screen Time reporting still require their own enabled sources. Sign-out still deletes shared credentials. Endpoints retain public-then-tailnet order.
- `SendToLucinaIntent` accepts optional files/text/note and a share action, uses the same body builder and background transport, and does not open the app. Disk-backed IntentFiles are copied; only already-memory-backed Shortcut files access their `data` property.
- Outgoing Lucina conversation interactions are donated after extension/intent sends, on each new chat send, and once per 24 hours at signed-in startup. Donation success records the timestamp in App Group defaults.
- Timeline attachments decode optional MIME/media ID/URL/byte fields leniently and omit missing fields when encoding. Media thumbnails use a device-token Authorization header and endpoint fallback. Visual attachments open a full-screen 1024-pixel thumbnail; links open with the system URL action; other kinds retain symbol capsules.
- English, Italian and Danish strings are included in both the catalog and translation source. The extension compiles the catalog as a resource.

## XcodeGen changes

`ios/project.yml` adds shared sources to Secretary, embeds SecretaryShare, links Intents, and declares `NSUserActivityTypes: [INSendMessageIntent]`. The new iOS 17 extension target uses bundle ID `tech.getsecretary.app.share`, display name Lucina, a UIViewController principal class, the exact requested activation predicate, and `IntentsSupported: [INSendMessageIntent]`. It compiles the shared keychain, tokens, donations and upload monitor and links Intents, QuickLookThumbnailing, LinkPresentation and ImageIO. Both app and extension entitlements include `group.tech.getsecretary.shared`; the extension includes the shared keychain group.

`Components.swift` includes `UIApplication.shared` keyboard handling, unavailable to extensions. The extension therefore uses a small local `ShareStyle` backed by the generated LucinaTokens rather than compiling the app-only component file.

## Validation and Mac follow-up

Linux validation: Swift syntax parsing for all changed/added Swift files; project YAML, entitlement plists and localization JSON parsing; target/source/entitlement/localization consistency checks; `git diff --check`.

Added XCTest coverage for exact reproducible multipart fields/file headers/boundary/body, metadata token exclusion, empty/excessive-part rejection, old/new/malformed attachment decoding and encoding, symbol mapping, URL/text classification and retry decisions.

This host has no Xcode, iOS SDK or Swift compiler. The app and extension have **not** been compiled, the XCTest suite has **not** been run, and share-sheet/background-transfer behavior has **not** been exercised on a device. On the Mac regenerate with XcodeGen, build Secretary and SecretaryShare, and run SecretaryTests. Real-device validation should include Photos HEIC/video, Safari URL/text, Files/Drive PDF/document/audio, Contacts and Maps, multi-selection, sign-out, offline/public-to-tailnet recovery, and terminating the extension immediately after Send. Signing/provisioning of the new App Group and extension must be performed by Xcode with the chosen team.

No implementation items are intentionally deferred. The full-screen thumbnail viewer is used instead of adding a Gallery navigation route. System share-sheet placement remains controlled by iOS; donations and the localized Favorites tip support ranking.

## Files added or changed
- `ios/Extensions/SecretaryShare/SecretaryShare.entitlements`
- `ios/Extensions/SecretaryShare/ShareStyle.swift`
- `ios/Extensions/SecretaryShare/ShareViewController.swift`
- `ios/Extensions/ShareShared/LucinaShareIcon.png`
- `ios/Extensions/ShareShared/ShareItemReader.swift`
- `ios/Extensions/ShareShared/ShareRequestBuilder.swift`
- `ios/Secretary/App/AppModel.swift`
- `ios/Secretary/App/RootView.swift`
- `ios/Secretary/Core/Keychain.swift`
- `ios/Secretary/Features/Chat/ChatStore.swift`
- `ios/Secretary/Features/Chat/ChatView.swift`
- `ios/Secretary/Features/Chat/Timeline.swift`
- `ios/Secretary/Features/Device/DeviceHub.swift`
- `ios/Secretary/Features/Device/SecretaryIntents.swift`
- `ios/Secretary/Features/Notifications/APNsAdapter.swift`
- `ios/Secretary/Features/Settings/SettingsView.swift`
- `ios/Secretary/Features/Share/LucinaDonations.swift`
- `ios/Secretary/Features/Share/SendToLucinaIntent.swift`
- `ios/Secretary/Features/Share/ShareAttachmentView.swift`
- `ios/Secretary/Features/Share/ShareUploads.swift`
- `ios/Secretary/Resources/Localizable.xcstrings`
- `ios/Secretary/Secretary.entitlements`
- `ios/SecretaryTests/ShareTests.swift`
- `ios/docs/share-to-lucina.md`
- `ios/l10n/translations.json`
- `ios/project.yml`
