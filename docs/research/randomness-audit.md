# Randomness audit: Secretary apps and Hermes

2026-11-03. Prompted by Juno's kernel seeding its generator from the boot counter (fixed separately, in the LucinaOS kernel). These apps run on normal operating systems whose kernels are sound, so the question here was our own code.

Read from source: SecretaryIOS `origin/main` ff4137d5 (server, Lucina crates, web, iOS, Android, ESP32 firmware), SecretaryCode 064dc71 (Mac), Hermes `main` dcc11a7. Nothing was attacked and no secret value was read.

## Verdict

**No weak generator guards a secret anywhere.** Every token, key, code and nonce comes from the OS generator (`getrandom`, `ring`, `uuid` v4, `SecureRandom`, `crypto.randomBytes`, `UUID()`). No secret needs rotating, no token is invalidated, nobody has to sign in again.

What was wrong: one fixed OAuth `state`, tokens compared in non-constant time, and three failure paths.

## Findings, by what an attacker could do

| # | Where | Weakness | Attacker needs, and gets | Result |
|---|---|---|---|---|
| 1 | Hermes OAuth callbacks | `state` was a fixed word (`poko`, `cityhop`, `griffin`, `lucina-gmail`), and the callback stored whatever token its code yielded | To reach a callback, with a Google account authorised on our OAuth client. Replaces a stored token with their own, so uploads, comments or Lucina's mail use their channel or mailbox. Not reachable from the internet at audit time: redirects are 127.0.0.1, peer1 rejects :8789 from outside, Flareon is not running | **Fixed.** `src/oauth-state.js`: `<flow>.<256 random bits>`, single use, 30 minutes, stored hashed in a 0600 file. Callback pages no longer hand out sign-in links |
| 2 | Hermes bearer checks (12 sites), `cityhopdashboard` push | `!==` and `includes` on tokens | A timing attack. All on private paths, except the dashboard push on Vercel, where network noise makes it impractical. The public X webhook was already constant time | **Fixed.** `src/secret-compare.js`, same behaviour |
| 3 | Hermes Cityhop queue | Open when no token is set; token also accepted in the URL | To reach the port: reads the video queue. Cityhop was off everywhere checked | **Fixed.** No token means refused; header only |
| 4 | Hermes `bring-shopping-list`, `cityhopdashboard` Postgres | `rejectUnauthorized: false` behind an env flag | A network position between Vercel and Postgres. The flag and the database URLs are not set in either Vercel project, so the code was idle | **Fixed.** The flag now means verified TLS |
| 5 | `secretary-server` `links.rs`, `todoist.rs` | `let _ = getrandom::fill`: a failed OS generator gave all-zero OAuth `state` and PKCE verifier | The OS generator failing, which practically does not happen on Linux | **Fixed.** Now a 500 |
| 6 | `secretary-nclient` `signing_key` | Key written with the default mode, then chmod 600 | A local user, in the moment of first creation. The deployed key is 600 | **Fixed.** Created private under a temporary name, then renamed. The existing key is unchanged, so no URL breaks |

Negative tests: `crates/secretary-server/src/links.rs` (`a_failed_random_source_gives_no_secret`), `crates/secretary-nclient/src/main.rs` (`tests`), Hermes `tests/oauth-state.test.mjs`, `tests/secret-compare.test.mjs`.

## Where each fix runs

| Fix | Code | Live |
|---|---|---|
| 5 | SecretaryIOS main 6966eeb2 | **Yes:** `secretary-server` deployed 2026-11-03 |
| 6 | SecretaryIOS main 6966eeb2 | **Not yet, no urgency:** it only matters when a key is created, and the key exists. Comes with `secretary-nclient`'s next deploy |
| 1, 2, 3 | Hermes main 8ebbb28 | **Yes on peer1** for Poko, restarted 2026-11-03 00:46 UTC. `src/index.js` and `src/lucina-gmail.js` are not copied there: Cityhop is off on peer1 and it has no Lucina Gmail |
| 4 (`bring-shopping-list`) | Hermes main 8ebbb28 | **Yes:** `flareon-bring-shopping-list` deploys on push to main, and the merge put it in production at 00:42 UTC. The code path is idle there: no database URL is set |
| 2 (dashboard push), 4 (`cityhopdashboard`) | Hermes main 8ebbb28 | **Not yet:** `cityhopdashboard`'s latest deployment is old, so it does not appear to deploy on push. Comes with its next deploy |

No app needs installing: the iOS, Android and Mac apps did not change.

Checked on peer1 after the restart: `/health` answers, Discord logged in. No token or a wrong token gets 401 on four API routes. The right `POKO_API_TOKEN` is accepted (405 on a GET, read from the env file without showing it). `state=poko` with a code gets 400, and an unsigned X webhook gets 401.

**peer1 runs older code than Hermes `main`.** `scripts/deploy-peer1-poko.sh` would also have shipped undeployed changes to `src/planner-fallback.js` (133 changed lines), `config.js`, `harness-agent.js` and `cursor-agent.js`, so this deploy copied only the 15 files of this fix. All of them matched the merge base on peer1, except `oauth-callback.js`, which lacked the Lucina Gmail flow. Backup: `/opt/hermes-poko/.backups/randomness-audit-20261103.tar.gz`.

**The deploy guard's marker was overridden.** A `poko-news-scout` task marker from 2026-10-30 pointed at the running service, which would have refused a deploy. It was 4 days old, with no child process and no scout log lines in the last 6 hours, so it was treated as hung and the service restarted. The cause of the hang was not looked into.

## Checked and fine

**Server secrets**, all from the OS generator:

| Value | Random bits | Notes |
|---|---|---|
| Sessions, device tokens | 256 | Stored hashed, constant-time compare. Cookie `HttpOnly; Secure; SameSite=Strict`, plus a CSRF header |
| Service tokens | 256 | Constant-time compare |
| MCP OAuth codes, refresh tokens, client secrets | 256 | PKCE S256 only. `==` on stored hashes of these values, which leaks nothing useful |
| Desk tickets | 256 | |
| Computer views, agentd file tokens | 244 | Two UUID v4 |
| OTP window id | 192 | |
| QR `code`, `pollToken` | 128 | 120 s, single use |
| Album shares, background upload capability | 128 | |
| Approval ids, grant `jti` | 128 | |
| agentd terminal token | 122 | UUID v4, constant-time compare |

**Signed URLs.** `/v1/media/s/` and the Instagram, nHentai and Pixiv URLs: HMAC-SHA256 truncated to 128 bits, 32-byte random keys, expiry inside the signature, constant-time compare.

**Short codes.**
- QR `shortCode`: 6 digits, the modulo bias is negligible. It lives 120 s and only an authenticated device can use it.
- Approval PIN: Argon2, 5 failures then a 15-minute lock that survives restarts.
- Login: Argon2, lockout that doubles.

**Encryption.**
- `links.key` (ChaCha20-Poly1305) and Android's vault (AES-256-GCM): random 96-bit nonce per write.
- Android's data key: `SecureRandom`, wrapped by AndroidKeyStore.
- OTP dedup: keyed HMAC of the code, never a bare hash. `seen.json` hashes random event ids, not codes.
- Grant, VAPID and web push keys: OS generator.

**Short ids that are names, not capabilities, all behind auth:** `dc-` (48 bits), `rt-`, `fl-` (32), `gr_`, `n_`, `i_`, timeline ULIDs, notebook ids.

**Clients.**
- iOS: no `arc4random`. `UUID()` for ids only, `.random` only for AR blinks and poses and UI tests. Secure Enclave keys, Keychain this-device-only, no trust overrides, ATS exception for local networking only.
- Android: no `java.util.Random` or `kotlin.random`, cleartext off, stock certificate checks plus rustls with web PKI roots.
- Web: `Math.random` only as an unreachable fallback (`getRandomValues` exists in every browser), checklist item ids, a multipart boundary, stage animation, mocks.
- Mac (SecretaryCode): server password `randomBytes(32)`, viewer token `randomBytes(24)`. `Math.random` only in UI, temp suffixes, one fallback client id, all upstream. `packages/stats` turns TLS checks off in upstream's console, which we do not appear to deploy.
- ESP32 firmware (aborted project): hardware TRNG.

**Harmless by inspection.**
- Backoff jitter; temp file names (`create_new`); etags.
- `secretary-server` `random_id`, which ignores errors but names calendar events, links and proposals, not secrets; `owner.rs` `dummy_hash`, which only burns time for unknown users.
- `new_item_id`'s clock fallback; the prompter's multipart boundary; `lucina-agentd` port and name picks.
- Pixiv's fixed client secret and hash secret, which are public values of Pixiv's Android app.
- Everything matching secret patterns in the repos is a test fixture or a redaction rule, checked by file and line.
- Hermes: `Math.random` only for turn, story and active-task ids, shuffles, a ComfyUI seed and a nudge skip. X OAuth nonce `randomBytes`. Bring import AES-GCM with a random IV. `private/` and `.env*` are ignored and never committed.

## Unproven

- **OTP `owner.token`, `phone.token`:** no generator in the repo. The relay enforces length and that they differ, not entropy. Not read.
- **Hermes service tokens:** made outside the repo, no minimum length enforced. Hermes' install scripts use `openssl rand -hex 24`, which is fine if those made them.
- **:8789 from outside:** refused when tried from nezopt, and peer1's iptables end in REJECT. Not tried from elsewhere; the Oracle VCN rules were not read.
- **The OAuth fix end to end:** tested with unit tests and refused codes. No real Google sign-in was run, since that would replace live tokens.
- **Not audited:** Pokoin's `/connect` link codes (made in Pokoin), LucinaOS `lucina-sync` pairing, third-party crates' internals (`web-push-native`, `graviola`).

## Seen in passing, not randomness

- `grocery-dashboard/api/grocery.js` has no auth, takes `userId` from the query and calls Hermes with the owner's bearer. **Not deployed** (2026-11-03): none of the 23 Vercel projects is that app or has `GROCERY_API_URL`. `grocery-dashboard.vercel.app` answers but is not in our account. `docs/grocery-intelligence.md` in Hermes still says to deploy it, which would open the hole.
- `bring-shopping-list/api/list.js` has no auth on GET and PATCH, and **is live** at `flareon-bring-shopping-list.vercel.app` with `GROCERY_API_TOKEN`, `HERMES_GROCERY_API_BASE` and `SHOPPING_LIST_USER_ID` set. An unauthenticated GET returned 502 `database_unavailable` and no data on 2026-11-03. The user is fixed on the server, so a caller cannot choose a `userId`. If the upstream answers again, anyone with the URL can read the list and change item status. Left unchanged, for the owner to decide. PATCH was not tried and the base URL was not read.
- Hermes' docs still describe `hermes-flareon-nez` on nezopt. No such container exists and nothing of Hermes listens on :8788. What runs: `hermes-poko` on peer1, `flareon-honcho-mcp` on nezopt.
- ZCode Desktop's credential key falls back to a hash of platform, home and username unless `ZCODE_CREDENTIAL_SECRET` is set. Third-party design; an attacker needs the file first.
