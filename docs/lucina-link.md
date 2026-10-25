# Lucina Link

One connectivity layer for every Lucina and Secretary service. A client (CLI, agent, later the apps and the Kindle)
asks "how do I reach service X for this kind of traffic right now?" and Lucina Link picks the best working route by
itself, at home or away. Cloudflare carries light traffic and remote shells; heavy transfers stay on the LAN, the
tailnet or the p1 relay.

Code: `crates/lucina-link` (library + `lucina-link` CLI). Proof: `verification/verus/link_policy.rs`.

## Services

| Service | What | Routes (best first) |
|---|---|---|
| `nezopt-ssh` | nezopt shell | lan `192.168.178.55:22` → tailnet `100.107.164.33:22` → cloudflare `nezopt-ssh.getsecretary.tech` → Oracle jump (`oracle-peer1` → `127.0.0.1:2224`) |
| `kindle-ssh` | Kindle PW3 root shell (KOReader dropbear :2222, only while KOReader runs) | lan `kindle.fritz.box:2222` (placeholder) → cloudflare `kindle-ssh.getsecretary.tech` |
| `secretary` | Secretary API + Lucina MCP on p1 | public `https://getsecretary.tech` |
| `lucina-media` | photos and videos (nezopt :8830) | tailnet `http://100.107.164.33:8830` → public `https://getsecretary.tech/v1/media` (p1 Caddy relay) |
| `lucina-memo` | voice memos (nezopt :8795) | tailnet `:8795` → public `https://getsecretary.tech/v1/memos` |
| `lucina-rs` | Rust Lucina (agents only) | tailnet `:8790` |

The table is `crates/lucina-link/routes.toml`, compiled in. Override it with `$LUCINA_LINK_CONFIG` or
`~/.config/lucina-link/routes.toml`.

## The route ladder

1. **lan**: direct on the home network (192.168.178.0/24). Fastest, no crypto overhead beyond the protocol's own.
2. **tailnet**: Tailscale. At home it runs direct over the LAN (`direct 192.168.178.55:41641`); away it is direct
   when NAT allows, else relayed through a DERP server.
3. **public**: the service's own HTTPS endpoint, `getsecretary.tech` on p1 (Caddy, DNS-only, **not** proxied by
   Cloudflare, so no body cap). For nezopt services p1 relays over its tailnet link.
4. **cloudflare**: a Cloudflare Tunnel hostname behind Cloudflare Access, opened with a service token. Works from
   any network with outbound HTTPS, needs no inbound port and no Tailscale on the client. Light traffic and shells.
5. **ssh_jump**: SSH ProxyJump through Oracle (`oracle-peer1`, the reverse tunnel on :2224). Kept as the last
   resort for shells: it drops connections away from home.

Preference per kind of traffic:

| Traffic | Order |
|---|---|
| `api` (request/response) | lan, tailnet, public, cloudflare |
| `shell` (SSH) | lan, tailnet, cloudflare, ssh_jump |
| `bulk` (uploads, downloads, media sync) | lan, tailnet, public, cloudflare (only if the size policy allows) |

Within one rung, the lower round-trip time wins; ties keep the table order. A route's target must also fit the
traffic: shells need a TCP, Cloudflare-SSH or jump target; API and bulk traffic need an HTTP(S) target.

## Size and type policy

Cloudflare's free plan refuses request bodies over 100 MB, and its CDN terms restrict serving video. So:

| Rule | Why |
|---|---|
| Video **never** goes through Cloudflare, whatever its size | CDN terms |
| Any body over 100 MB (`CLOUDFLARE_BODY_CAP`) never goes through Cloudflare | free-plan hard limit |
| Other bulk transfers through Cloudflare only up to 64 MiB (`CLOUDFLARE_BULK_MAX`) | margin, and keeps the tunnel light |
| `ssh_jump` carries shells only | it is an SSH channel, not an HTTP path |
| A route's own `max_body_bytes`, if set, is respected | per-route limits |

Heavy media therefore moves over the LAN, the tailnet, or the p1 relay. Uploads that may be interrupted use
resumable, chunked transfers (lucina-memo already has them; lucina-media's PhotoKit backup sends one asset per request
and retries). Chunks stay below the 100 MB cap anyway, so a chunked upload never depends on which rung carries it.

The rules live in `crates/lucina-link/src/policy.rs` and are proven in Verus (`verification/verus/link_policy.rs`):

- L1: a selected route is never Cloudflare for a video transfer.
- L2: a selected Cloudflare route carries at most 100 MB, and a non-video bulk transfer at most 64 MiB.
- L3: the selected route is allowed and up.
- L4: no other usable route has a better rank, or the same rank and a lower RTT.
- L5: nothing is selected only when no route is allowed and up.
- L6: an SSH jump only carries shells.

`tests/verus_policy.rs` checks that the crate and the proven functions agree on an exhaustive grid (over 100,000
candidate lists).

## Probing and failover

- **Probes run in parallel** (plain threads, no async runtime), one per route the policy allows:
  - TCP routes: connect with a timeout;
  - HTTP routes: `GET <base><health>`, where any answer below 500 counts as up (a 401 from an auth gate still
    means reachable);
  - Cloudflare routes: an HTTPS GET with the Access headers and no redirects. 200 means the origin was reached,
    401/403 or a redirect to the Access login means a bad token, 502 means the tunnel is up but the origin is
    unreachable, and 530 means no connector;
  - the Oracle jump: a TCP connect to the jump host, reported as "partial".
- **Timeouts** per rung: lan 400 ms, tailnet 1.5 s, public 3 s, cloudflare 4 s, jump 3 s (per route `timeout_ms`).
- **Early exit**: as soon as the best route that can still win is up and every better-ranked route has answered,
  the choice is made. Slower losers are reported "not waited for". At home a shell choice takes about 10 ms.
- **LAN detection**: before probing a `lan` route, the OS is asked which local address it would send from
  (a connected UDP socket, no packet). If that address is outside `lan_cidr`, the route is down at once ("not on
  the home LAN"). Another network that also uses 192.168.178.0/24 (a common Fritz!Box default) can pass this
  check. Then the TCP probe either fails or reaches a stranger, and SSH refuses it on the host key, because
  `lucina-link ssh` pins one `HostKeyAlias` per service.
- **Failover in the library** (`Link`):
  - choices are cached per service and traffic shape for 60 s;
  - `report_failure` backs a route off for 30 s, doubling per consecutive failure up to 5 min, and the next call
    picks again;
  - `report_success` clears the backoff;
  - a change of the machine's default source address (moving between networks) drops the cache.
- **Forcing a route**: `--via ROUTE[,ROUTE]` or `LUCINA_LINK_VIA` (which `ssh` passes on to its ProxyCommand).

## Auth

- **Cloudflare Access**:
  - Application "Lucina Link SSH" covers `kindle-ssh.getsecretary.tech` and `nezopt-ssh.getsecretary.tech`.
  - Its only policy is `non_identity` and includes the service token `lucina-link`, which never expires
    (`duration: forever`).
  - Without the token the hostnames answer 401.
- **The token** is stored in **Infisical** (self-hosted on nezopt, project "Apis", env `dev`) as
  `LUCINA_LINK_CF_CLIENT_ID` / `LUCINA_LINK_CF_CLIENT_SECRET`. Clients read it from the environment, or from
  `~/.config/lucina-link/secrets.env`, which must be mode 600 (a looser mode is refused).
  - It is never in git and never printed: the CLI hands it to `cloudflared` through the child's environment
    (`TUNNEL_SERVICE_TOKEN_ID/SECRET`) and to `curl` through a config on stdin, not through argv.
  - Rotation: create a new token, add it to the policy, update Infisical and the clients' `secrets.env`, then delete
    the old token.
- **SSH itself is still key-based.** Cloudflare Access is an extra gate in front of sshd and dropbear, not a
  replacement for keys. The Kindle uses `~/.ssh/kindle_pw3`.
- The HTTP services keep their own auth (Secretary device tokens, the media upstream key, the memo gateway). Lucina Link
  only finds the path.

## Cloudflare objects (account `e2620981…`, zone getsecretary.tech)

| Object | Id | Notes |
|---|---|---|
| Tunnel `lucina-link` | `8beba485-5215-4c5e-b255-1661cab935b2` | remote-managed; ingress `kindle-ssh` → `ssh://kindle.fritz.box:2222` (placeholder), `nezopt-ssh` → `ssh://127.0.0.1:22`, else 404 |
| DNS | `kindle-ssh`, `nezopt-ssh` | proxied CNAMEs to the tunnel |
| Access app "Lucina Link SSH" | `aaa63afb-9784-4ac5-861d-c5a731daa005` | policy: service token only |
| Service token `lucina-link` | `8d4a9e25-8fcf-403a-a4a6-b0784348e722` | non-expiring |

- The connector runs on nezopt as the **user** unit `cloudflared-lucina-link.service`: a copy is in
  `deploy/lucina-link/`, the token file is `~/.config/cloudflared/lucina-link.token` (mode 600), metrics are on
  127.0.0.1:20249, and MemoryMax is 96M.
- It is separate from the existing tunnels (`cloudflared-harness`, `cloudflared-qwen`, the pokoin ones), which were
  not touched.
- The setup is idempotent: `deploy/lucina-link/cloudflare-setup.sh`. Re-run it with `KINDLE_ORIGIN=<ip>:2222` once
  the Kindle has a reserved address.

## The Kindle

- **Now (no install on the Kindle):**
  - nezopt is the gateway. The Kindle's dropbear is reached from nezopt over the LAN, so `kindle-ssh` through
    Cloudflare works from anywhere while the Kindle is at home and KOReader's SSH is on.
  - Missing: a stable address. `kindle.fritz.box` does not resolve, so the origin and the `lan` route are
    placeholders until the owner gives the Kindle's address. The best fix is a DHCP reservation on the Fritz!Box.
- **Away from home (planned, needs the owner's OK for that specific install):**
  - The Kindle would dial out itself: a reverse SSH with `dbclient -R` (dropbear's client, already on the device)
    to p1 or nezopt, started and stopped by one KUAL menu item.
  - That costs one process of about 1 MB, off unless switched on, with no new daemon and no new binary.
  - A full `cloudflared` on the Kindle (around 35 MB of binary and 30 MB of RAM out of 512 MB) is not proposed.
- **Library sync (later):**
  - Lucina Ink posts to `POST /v1/library/sync` on getsecretary.tech, which is outbound HTTPS and light traffic.
  - The Kindle client can use the `lucina-link` library for the route choice. A static armv7 build without the
    `https` feature is 863 KB.
  - The HTTPS feature uses rustls with `ring`, which needs an ARM C cross-compiler that nezopt does not have yet:
    `rust-lld` alone links the pure-Rust build.

## Resource budget on small devices

- No async runtime and no background threads at rest: probing starts threads only while choosing, and each ends on
  its own timeout.
- Binary sizes:

  | Build | Size |
  |---|---|
  | macOS aarch64 | 2.5 MB |
  | Linux x86_64 static (musl) | 3.0 MB |
  | armv7 musl without `https` | 863 KB |

- Memory: a few MB while probing, nothing afterwards (the CLI exits; `Link` keeps a small map).
- On the Kindle: nothing runs unless switched on from KUAL. Battery cost is limited to the probe and the session itself.

## Using it

```bash
lucina-link probe                                  # every service, every allowed route, the choice marked
lucina-link probe lucina-media --media video --bytes 2000000000
lucina-link ssh nezopt-ssh -- uptime               # best route for a shell
lucina-link ssh kindle-ssh                         # root shell on the Kindle (KOReader SSH must be on)
lucina-link curl lucina-memo /health               # best API route, auth headers handled
lucina-link route secretary --json                 # for scripts and agents
```

To make plain `ssh kindle` work, add to `~/.ssh/config`:

```
Host kindle
  User root
  IdentityFile ~/.ssh/kindle_pw3
  IdentitiesOnly yes
  HostKeyAlias kindle-ssh
  ProxyCommand ~/.local/bin/lucina-link proxy kindle-ssh
```

## Verified (2026-10-25)

| Check | From | Result |
|---|---|---|
| SSH nezopt over lan | Mac at home | ok, 0 s |
| SSH nezopt over tailnet | Mac at home | ok; Tailscale SSH "check" mode may ask for a browser re-auth |
| SSH nezopt over Cloudflare (Access + token) | Mac at home, through the Cloudflare edge | ok, 1 s |
| SSH nezopt over the Oracle jump | Mac at home | ok, 1 s |
| Cloudflare hostname without a token | Mac | 401 |
| HTTP: media/memo/rs over tailnet; media/memo/secretary over public | Mac | 200 (401 from the media and memo auth gates, as expected) |
| Probe from p1 (off the home LAN) | p1 | lan "not on the home LAN"; tailnet chosen; Cloudflare to nezopt up with the token |
| kindle-ssh | Mac, p1 | tunnel up, origin unreachable (placeholder address, KOReader SSH off) |
| Policy: 2 GB video | Mac | Cloudflare skipped, tailnet or public chosen |

Not verified yet: a full SSH session from a network other than home (only probed from p1, which has no
`cloudflared` or nezopt key), and anything on the Kindle.

## Known limits and follow-ups

- **Tailscale SSH**: port 22 on nezopt's tailnet address is taken over by Tailscale SSH in "check" mode. An
  unattended agent may hit a re-auth prompt there; it then fails over to Cloudflare.
- **p1 ↔ nezopt over DERP**: p1 reaches nezopt through a DERP relay (fra), at about 100 ms. The public relay for
  media therefore rides DERP too. Allowing UDP 41641 to p1 in the Oracle security list would let WireGuard go direct.
- **LAN-only bindings**: lucina-media, lucina-memo and lucina-rs listen on the tailnet address and localhost, not on
  192.168.178.55. At home the tailnet route is direct anyway; an iPhone without Tailscale on the LAN would need a
  LAN binding.
- **The Kindle**: a reserved address, then the origin update; an away-from-home dial-out helper only with the owner's OK.
