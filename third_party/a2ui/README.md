# A2UI oracle: what upstream published, unchanged

The original of A2UI (Agent-to-User Interface) — its conformance suite, its catalogues and its
v1.0 specification — vendored. They are the oracle of the port: the behaviour
`crates/lucina-a2ui` must match. Port record: `docs/design/a2ui-port.md`.

- Source: <https://github.com/a2ui-project/a2ui>
- Commit: `5ff1b05f02d39f969b52a942e78e864ef3ac01fa` (2026-11-20)
- Licence: Apache-2.0, as `upstream/LICENSE` says; the notice is kept with the code.

## What is here

`upstream/` holds, copied byte for byte (415 files): the upstream `conformance/` directory
(without Python files, `pyproject.toml`, `conformance/scripts/` and `conformance/tests/`), the
`catalogs/` directory (without Python files), `specification/v1_0/`,
`specification/inference_formats/`, `specification/versions.json`, and `LICENSE`.

`MANIFEST.sha256` lists every file as `<sha256>  ./<path>`, the way `sha256sum` prints, sorted.
`crates/lucina-a2ui/src/vendored_tree.rs` checks `upstream/` against that manifest, that no
Python file and no symlink came in, and that the pin and the licence are recorded.

## The rule

Never edit, rename, reformat or delete a file under `upstream/`, and never add one. A case we
cannot meet goes into `docs/design/a2ui-port.md` under "Conformance cases we do not meet", with
its reason — the oracle is never moved to make a test pass.

## Moving the pin

One change moves it. Copy the same paths from the new upstream commit over `upstream/`, run from
the repository root

`(cd third_party/a2ui/upstream && find . -type f | LC_ALL=C sort | xargs -d '\n' sha256sum) > third_party/a2ui/MANIFEST.sha256`

then change `UPSTREAM_COMMIT` in `crates/lucina-a2ui/src/lib.rs`, the commit id above, and `pin`
and `pin_date` in the `a2ui` block of `docs/upstreams.toml`. `cargo test -p lucina-a2ui` says
whether the copy is complete.

## Secret scan

The merge gate does not scan `third_party/` file by file. The scan is done once per copy, by hand, with the same
scanner in one invocation, and its result is recorded here. A pin move repeats it and replaces the line below.

- 2026-11-22, pin `5ff1b05f02d39f969b52a942e78e864ef3ac01fa`: `gitleaks dir --no-banner --redact third_party/a2ui/upstream` scanned 3.96 MB, no leaks found.
