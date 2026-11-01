# Rust porting guidelines (all Lucina projects)

Binding for every port, rebuild or upstream sync: lucina-rs (from the Node Lucina), Lucina Agents (from Paseo),
LucinaOS (kernel from Asterinas, Kindle from Amazon's kernel and userspace, the gbgo and surya boards), the Ink
reader, lucina-media, and anything added to `docs/upstreams.toml` later. Every porting brief links this file.
A copy is a fixed Claude memory (`rust-porting-guidelines`), so every session loads it.

Sources: LucinaAndroidOS `docs/management/master-plan.md` "Principles"; the owner's rules "Original code is the
source of truth (all Lucina)" (2026-11-01), "No Codex on Lucina" (2026-10-29), "Scripts and tools in Rust"
(2026-10-15); the weekly upstream watch (`docs/upstream-watch.md`).

## 1. The original is the source of truth for behaviour

- When a port behaves differently from the original, the original is right until shown otherwise. Read the
  original's handling of the same feature first (Amazon's board files for the Kindle, vendor/Android code for the
  phone, the archived Node Lucina for lucina-rs, Paseo for Lucina Agents, Asterinas for the kernel, the Ink code for
  the reader). Match its values, sequences, timings and error cases before guessing at a cause.
- Use the right variant of the original (board revision, release, build flavour). A fix read from the wrong variant
  is a wrong fix (the PW3 is WARIO_BASE, not WOODY).
- Undocumented behaviour is settled against a working native implementation and hardware or wire traces, not
  against docs alone.

## 2. Copy behaviour, not structure

- Reimplement it the Lucina way: Rust, optimized, with our own interfaces. Redesign interfaces rather than
  transliterate C, JS or Python; keep the observable contract (formats, protocols, defaults, edge cases).
- Where it must live in someone else's kernel or firmware, write a clean mainline-style change instead.

## 3. Rust-first

- New code, scripts and tools are Rust. Assembly, FFI or vendor firmware only where justified and recorded
  (`decisions.md` / an ADR in the project).
- Reuse mature Rust projects before writing our own, and record the verdict for every candidate:
  **adopt** (use directly), **adapt** (take parts and change them), **reference** (read only), **reject**
  (with the reason). LucinaOS keeps them in `docs/research/integration-decisions.md`; other projects in their
  docs next to the port. Pin what is adopted (`references/upstream.lock`, Cargo pins) and record the pin in
  `docs/upstreams.toml`.

## 4. Licences, before anything is copied

- Check the upstream licence (its LICENSE file, not a README badge) and ours, and write both into
  `docs/upstreams.toml` (`licence_upstream`, `licence_ours`).
- GPL / AGPL / unlicensed code: facts and behaviour only, no code taken into an MIT/MPL tree. MPL-2.0 (Asterinas):
  modified MPL files stay MPL and their source stays available. Permissive (MIT/Apache/BSD/Zlib): keep the notice.
- No licence declared = all rights reserved: ideas only.

## 5. Tests against the original's behaviour

- Every ported capability has an automated acceptance test that pins the original's behaviour: golden files
  or traces captured from the original, parity runs (lucina-rs `parity`, `shadow-replay`), `docs/parity/` for LucinaOS.
- Evidence before claims: a status line links the test, trace or log that shows it.
- Verus / Stateright / TLA+ where a proof buys a real guarantee; never claim more than the proof covers.

## 6. Staying current with the upstream (the weekly watch)

- `docs/upstreams.toml` lists every upstream with the pin we are based on. `lucina-rs upstream check` runs every
  Monday: it fetches what changed since the pin, JEV scores it, and Lucina asks the owner
  "Port into ours / Later / Ignore" for significant changes.
- On "Port into ours", Lucina Agents gets a workspace and a brief (`data/upstream-watch/briefs/`). Port the
  behaviour changes that matter to us, with tests, then move `pin` in `docs/upstreams.toml` to what you ported.
  Record changes you deliberately skip (and why) in the commit message.

## 7. Giving back

- Improvements of ours that the upstream lacks are listed as `[[upstream.contrib]]` in `docs/upstreams.toml`.
- Draft only: no fork, push, issue or pull request upstream without the owner's explicit OK in chat. A draft is a
  write-up and a local branch at most.
- Check the upstream's CONTRIBUTING rules, the licence direction, and that nothing private (keys, hostnames,
  owner data, vendor blobs) is in the change.

## 8. How the work runs

- One task per Lucina Agents workspace (`lagent workspace create … --base origin/main`); merge to `main` inside
  `flock /tmp/lucina-merge.lock`; deploy only from `main` through deploy-guard; archive the workspace after.
- Go-aheads through Lucina (`lucina_decide`), not chat. Owner-only actions (money, passwords, terms, messages in
  his name, upstream contributions) still need his own words.
- No Codex on Lucina repos (lane categories `deep-lucina` / `routine-lucina`). Never the paid DeepSeek key.
- Heavy commands run on nezopt over SSH; verification gates through `verify-gate`.
