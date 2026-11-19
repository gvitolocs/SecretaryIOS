# JD-C008 slice 1: lucina-updates (run JD-R062) - report

Stopped early by the coordinator's order (host overloaded). No checks were run after the one described below.

## Built
- `crates/lucina-updates` (lib + bin, workspace member): loopback-only read-only server, routes
  `/v1/updates/<product>/<channel>/manifest` and `.../artefacts/<sha256>.<ext>`, GET/HEAD only (405 otherwise),
  strict path validation (no `%`, `..`, `//`, backslash, NUL), no symlink followed (lstat per component, O_NOFOLLOW,
  canonical-path check), manifest cap 16 KiB (500 + log line), streamed artefacts, Range / If-Range / 416 / 304,
  strong ETag, Cache-Control `no-cache` (manifest) and `private, immutable` (artefacts). No key, no signing, no writes.
- `secretary-server`: `/v1/auth/updates` beside `/v1/auth/media`, one shared `gate_check`, own limiter
  (`updates_limiter`, sized by `SECRETARY_MEDIA_AUTH_PER_MINUTE`). Accepts the same credentials as media, including
  Lucina's service token (same check, as asked). Media behaviour unchanged.
- Deploy proposals, not installed: `deploy/lucina-updates.service`, `deploy/lucina-updates/Caddyfile.snippet`
  (the sidecar is loopback-only, so p1 needs a path to it: coordinator's choice).

## Decision on serve_file
Not shared. `serve_file` is built on `tower_http::ServeFile` (opens by path, follows symlinks) and lives in
`lucina-media`, which would have to become a dependency (JPEG XL, SQLite, ML). The small file server is written inside
`lucina-updates`; `lucina-media` is untouched. No copy of `serve_file`.

## Tests (crates/lucina-updates/tests/serve.rs, unit tests in src/lib.rs, secretary-server/tests/updates_auth.rs)
Whole store frozen 0555/0444 in every test but the atomic one; each ends by asserting nothing changed.
- path_traversal_is_refused; route_parser_refuses_bad_paths (unit)
- unknown_product_and_bad_channel_are_404
- bad_artefact_names_are_404 (63 digits, uppercase, bad ext, extra suffix)
- symlinks_are_never_followed (file, in-store, manifest, directory component)
- directories_are_404_with_no_listing
- only_get_and_head_are_served (POST/PUT/DELETE/PATCH -> 405)
- range_returns_exactly_the_requested_bytes; unsatisfiable_range_is_416; stale_if_range_gets_the_full_file
- if_none_match_is_304; head_matches_get_with_no_body; etag_is_the_strong_sha256_of_the_name
- cache_and_safety_headers; a_manifest_over_16_kib_is_not_served
- range_above_4_gib_is_not_truncated (sparse 5 GiB); the_whole_store_is_read_only
- readers_only_ever_see_one_complete_manifest_while_it_is_replaced_by_rename
- updates_auth.rs: no_token_is_401, an_unknown_token_is_401, a_revoked_token_is_401_and_a_valid_one_passes, the_updates_bucket_is_its_own

## Checks
One bq job ran (`~/scratch/jd-c008/check.log`, `check.sh`): `cargo test -p lucina-updates` passed
(unit: 2 passed; serve.rs: 17 passed, 0 failed). `cargo test -p secretary-server auth`, clippy and rustfmt did not
finish before the job was cancelled: NOT VERIFIED (the secretary-server gate and its 4 new tests, clippy, rustfmt).
- The first run warned `unused import: Read` in lib.rs; removed by a text edit afterwards, not recompiled.
- Cargo.lock is committed (commit 2db30f84 swept in the new package entry).
- The lead check ran `cargo test -p secretary-server auth`, whose filter skipped the four gate tests in `tests/updates_auth.rs` (no "auth" in their names); run them as `cargo test -p secretary-server --test updates_auth`.
- The brief's `rustfmt --edition 2024` disagrees with the crates' edition 2021 (import order only); files were
  formatted with 2021.
