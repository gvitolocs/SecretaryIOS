# Rust porting guidelines (all Lucina projects)

Binding for every port, rebuild or upstream sync: lucina-rs (from the Node Lucina), Lucina Agents (from Paseo),
LucinaOS (kernel from Asterinas, Kindle from Amazon's kernel and userspace, the gbgo and surya boards), the Ink
reader, lucina-media, and anything added to `docs/upstreams.toml` later. Every porting brief links this file.
A copy is a fixed Claude memory (`rust-porting-guidelines`), so every session loads it.

Sources: LucinaOS `docs/management/master-plan.md` "Principles"; the owner's rules "Original code is the
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

## 6. The old-against-new review, before a port slice merges

- **The rule.** Before a port slice merges, the original and the new Rust are compared line by line. Two reviewers read
  them side by side and answer one question: does the port behave the same. They run on free lanes, the no-cost model
  lanes in the dispatch chain (`docs/design/flaws.md`), on model families other than the one that wrote the slice
  (Claude, Qwen, DeepSeek, Gemini); the bans in section 9 still hold. Every finding cites the original's file and line
  and the port's file and line. The lead decides each finding: accepted, or rejected with the reason. The slice merges
  when both reviews come back with nothing open. The reviewers do not edit the port and do not edit tests.
- **Why.** A compiler accepts every one of the mistakes below. Source: GitHub's account of porting the Copilot agent
  runtime from TypeScript to Rust (Stephen Toub, The GitHub Blog, 2026-09-16), where dozens of regressions were
  traced and every one of them had compiled; summary in the vault at `Harness/Events/2026-11-12`.
- **The checklist**, one item per family of mistake, with what to look for:
  - Number types and how they are written out. An integer that became a float and now prints `42.0`; a value that can
    be fractional stored in an integer; signed against unsigned; the width.
  - Defaults on empty values. JavaScript `a || b` replaces an empty string, zero and false; Rust `unwrap_or` only
    replaces a missing value. The same for null against undefined against an absent key.
  - Behaviour the old platform supplied without being asked: time zone and locale, the working directory, PATH and
    other environment read at a different moment than before, flags a wrapper added to every spawned process,
    encodings, line endings, file modes.
  - Pairs. Where the original does two things together (mark aborted AND cancel the loop; persist AND update the live
    state; open AND register for close), check both halves are in the port.
  - Lifetimes of things referred to by a handle or id. Can the handle outlive the object; can an object be disposed in
    the middle of a request; are generation counters and flags updated on both sides.
  - Features left out. Every public entry point, callback, option and error path of the original exists in the port, or
    is listed as deliberately dropped. A deleted or changed end-to-end test is a finding by itself.
  - Replacement libraries. Where the port uses a different library than the original, how the two differ on malformed
    input, limits, ordering and error reporting.
  - Changes lost while the trunk moved. After merging the trunk into the port branch, every change the trunk made to
    the original code since the port started is present in the port.
  - Speed that the original had: caches and memoisation, streaming instead of reading whole files, borrowing instead of
    copying large data, work kept off the thread that draws or serves.
  - Order and timing. Events delivered in the same order; nothing that used to happen before a wait now happening
    after it.
- **What the oracle is.** The original's behaviour, and the end-to-end tests written against it before the slice starts
  (section 5). A port slice does not change those tests, golden files, snapshots or parity fixtures; a slice that needs
  to is stopped and brought to the lead. Translate first; redesign, optimisation and bug fixes are separate changes
  after the slice has merged.
- **How it runs here.** The lead starts the two review runs with a brief that names the original's path at its pinned
  revision and the port's path. Reports go to the workspace's scratch folder (`~/scratch/…` on nezopt, never the repo).
  Findings and the decision on each are recorded in the slice's adopt / adapt / reference / reject record (section 3).

## 7. Staying current with the upstream (the weekly watch)

- `docs/upstreams.toml` lists every upstream with the pin we are based on. `lucina-rs upstream check` runs every
  Monday: it fetches what changed since the pin, JEV scores it, and Lucina asks the owner
  "Port into ours / Later / Ignore" for significant changes.
- On "Port into ours", Lucina Agents gets a workspace and a brief (`data/upstream-watch/briefs/`). Port the
  behaviour changes that matter to us, with tests, then move `pin` in `docs/upstreams.toml` to what you ported.
  Record changes you deliberately skip (and why) in the commit message.

## 8. Giving back

- Improvements of ours that the upstream lacks are listed as `[[upstream.contrib]]` in `docs/upstreams.toml`.
- Draft only: no fork, push, issue or pull request upstream without the owner's explicit OK in chat. A draft is a
  write-up and a local branch at most.
- Check the upstream's CONTRIBUTING rules, the licence direction, and that nothing private (keys, hostnames,
  owner data, vendor blobs) is in the change.

## 9. How the work runs

- One task per Lucina Agents workspace (`lagent workspace create … --base origin/main`); merge to `main` inside
  `flock /tmp/lucina-merge.lock`; deploy only from `main` through deploy-guard; archive the workspace after.
- Go-aheads through Lucina (`lucina_decide`), not chat. Owner-only actions (money, passwords, terms, messages in
  his name, upstream contributions) still need his own words.
- No Codex on Lucina repos (lane categories `deep-lucina` / `routine-lucina`). Never the paid DeepSeek key.
- Heavy commands run on nezopt over SSH; verification gates through `verify-gate`.
