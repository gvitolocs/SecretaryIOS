# Lucina device and plugin integration — 2026-10-24

The device core and Secretary gateway use Rust. Official Hermes and its upstream plugins retain their own implementation languages. Existing web build tools and the legacy bot transport are unchanged.

## Samsung OTP background delivery

Installed package `tech.getsecretary.lucinaotp`, version 0.4.0, retains the existing pairing and incoming-SMS permission. Its SMS broadcast receiver wakes on a new message, forwards a recognized OTP over authenticated HTTPS, then exits. There is no foreground service or permanent notification. The previous `otp` notification channel is cancelled and removed during migration.

Continuous forwarding is enabled on the relay through the systemd drop-in `lucina-otp.service.d/continuous.conf`. `/otp stop` pauses forwarding and clears pending codes; that pause persists across relay restarts. `/otp arm` resumes continuous forwarding. The Android app also has an explicit disable switch. No SMS history or outgoing SMS permission is used.

The Samsung's Android Doze firewall blocked network access during a background diagnostic. The existing authenticated ADB connection was used to add this package to Android's battery allowlist (`cmd deviceidle whitelist +tech.getsecretary.lucinaotp`). This is an OS setting, not a kernel modification. Removal: `cmd deviceidle whitelist -tech.getsecretary.lucinaotp`.

Codes expire after two minutes, are consumed once, and stay out of model tools, shared memory and logs. The phone uses its scoped key; owner access uses a separate key. No keys are in this document. Offline delivery is not queued; force-stop, lost connectivity, or revoked SMS permission can prevent delivery. Continuous availability is not a promise of an immortal Android process.

Verification: physical S20 FE protected synthetic diagnostic sent its fixed test code over HTTPS; the Rust private-chat handler retrieved it once. A second read found no code. No foreground service or active OTP notification remained. Real carrier-SMS delivery is still untested.

## Rust calling function

Secretary (`tech.getsecretary.secretary`) is installed on the S20 FE. Its phone UI and authenticated CLI provider call the same Rust JNI function, which validates the number and invokes Android Telecom. The MCP `phone_call` tool supports a non-calling readiness check. The actual phone reports `needs_permission`; no call was placed. Android's call permission and optional default-dialer role require setup before use. SIM call recording/transcription is not implied by installing this function.

## Official plugin inventory

All 18 plugins from the supplied graphic are installed in Lucina's dedicated Hermes profile. Enabled registrations: Advanced Delegation, Agent Importers, MCP Serve, Memory Wiki, Bubblewrap, Singularity/Apptainer, Accent Picker. Registration is not proof that an external application/backend is available. Cua remains the selected terminal backend. No public MCP listener or tracing export was started.

The remaining plugins are installed but disabled pending their application, endpoint or account setup: Blender, TouchDesigner, Spotify, Home Assistant, X Chat, Langfuse, Claude Subscription DirectSDK and Camofox. Meshy, Snyk and REA stay disabled because their upstream MCP servers require Node.js. No new Node runtime was installed for these plugins.

Installations used Hermes' plugin installer and catalog pins. Camofox was absent from this installed Hermes catalog, so its official subdirectory was installed disabled at explicit revision `e9910ad065087f2833c9a460157290eff5d5b186`; the installer correctly labels this a custom source, not a catalog-reviewed pin. No scanner or trust checks were bypassed.

The Rust dashboard reports installation/enabled flags from actual local state plus setup notes, without returning credential configuration. The web and Android app's My desk show this inventory. The standalone native iOS dashboard extension is still pending.

## Wardrobe and deployment

Restored the private 13-outfit catalog from the existing backup after validating all GLB headers, mesh/skin presence, thumbnails and per-outfit JSON manifests in Rust. Existing private model files were reused; no artwork or models were generated or uploaded. The school-blazer outfit was selected and rendered in an isolated authenticated QA session using copies of the production assets. Production avatar preferences were preserved.

Rust device tests, web typechecking, production build and 286 web tests passed. Updated web and Rust gateway were deployed to getsecretary.tech. `/desk` returns 200; unauthenticated `/v1/hermes` and `/v1/phone/otp/status` return 401. Production backups: `/var/lib/secretary/web-before-20261024-hermes.tgz`, `/usr/local/bin/secretary-server.before-20261024-hermes`, and private `wardrobe.json.before-restore-20261024`. The PDF production worker and Telegram regression fixes were preserved.
